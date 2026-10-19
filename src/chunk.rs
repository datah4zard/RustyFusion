#![allow(clippy::needless_range_loop)]

use std::{
    alloc::{self, Layout},
    collections::{HashMap, HashSet},
    fmt::{Display, Formatter},
};

use crate::{
    config::config_get,
    defines::ID_OVERWORLD,
    entity::{Entity, EntityID, Player, NPC},
    error::{log, panic_log, FFError, FFResult, Severity},
    net::FFClient,
    Position,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstanceID {
    pub channel_num: u8,
    pub map_num: u32,
    pub instance_num: Option<u32>,
}
impl Default for InstanceID {
    fn default() -> Self {
        Self {
            channel_num: 1,
            map_num: ID_OVERWORLD,
            instance_num: None,
        }
    }
}
impl Display for InstanceID {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/{}:{}",
            self.channel_num,
            self.map_num,
            self.instance_num
                .map(|id| id.to_string())
                .unwrap_or("None".to_string())
        )
    }
}

pub const MAP_SQUARE_COUNT: i32 = 16; // how many map squares there are in each direction
pub const NCHUNKS: usize = MAP_SQUARE_COUNT as usize * 8; // 16 map squares with side lengths of 8 chunks
pub const MAP_SQUARE_SIZE: i32 = 51200;
pub const MAP_SIZE: i32 = MAP_SQUARE_SIZE * MAP_SQUARE_COUNT; // top corner of (16, 16)
pub const CHUNK_SIZE: usize = MAP_SIZE as usize / NCHUNKS;

pub fn world_pos_to_chunk_pos(pos: Position) -> (i32, i32) {
    (
        (pos.x * NCHUNKS as i32) / MAP_SIZE,
        (pos.y * NCHUNKS as i32) / MAP_SIZE,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkCoords {
    pub i: InstanceID,
    pub x: i32,
    pub y: i32,
}
impl ChunkCoords {
    pub fn from_pos_inst(pos: Position, instance_id: InstanceID) -> Self {
        let (x, y) = world_pos_to_chunk_pos(pos);
        Self {
            x,
            y,
            i: instance_id,
        }
    }
}
impl Display for ChunkCoords {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "({}, {}, {})", self.x, self.y, self.i)
    }
}

fn get_visibility_range() -> usize {
    config_get().shard.visibility_range.get()
}

#[derive(Debug, Clone, Copy)]
pub enum TickMode {
    Always,
    WhenLoaded,
    Never,
}

struct RegistryEntry {
    entity: Box<dyn Entity>,
    chunk: Option<ChunkCoords>,
    tick_mode: TickMode,
}

struct ChunkMap {
    chunks: Box<[[Chunk; NCHUNKS]; NCHUNKS]>,
    loaded_chunk_count: usize,
    loaded_entity_count: usize,
}
impl ChunkMap {
    fn get_ids(&self) -> Vec<EntityID> {
        self.chunks
            .iter()
            .flatten()
            .flat_map(|chunk| chunk.get_all())
            .cloned()
            .collect()
    }

    fn get_player_count(&self) -> usize {
        self.chunks
            .iter()
            .flatten()
            .map(|chunk| chunk.get_player_count())
            .sum()
    }
}

pub struct EntityMap {
    registry: HashMap<EntityID, RegistryEntry>,
    chunk_maps: HashMap<InstanceID, ChunkMap>,
    instances_to_cleanup: HashSet<InstanceID>,
    entities_to_cleanup: HashSet<EntityID>,
    next_pc_id: u32,
    next_npc_id: u32,
    next_slider_id: u32,
    next_egg_id: u32,
}

impl EntityMap {
    pub fn get_entity<T: Entity + 'static>(&self, id: EntityID) -> Option<&T> {
        self.registry.get(&id).and_then(|entry| {
            let any_ref = entry.entity.as_ref().as_any();
            any_ref.downcast_ref()
        })
    }

    pub fn get_entity_mut<T: Entity + 'static>(&mut self, id: EntityID) -> Option<&mut T> {
        self.registry.get_mut(&id).and_then(|entry| {
            let any_ref = entry.entity.as_mut().as_any_mut();
            any_ref.downcast_mut()
        })
    }

    pub fn get_entity_raw(&self, id: EntityID) -> Option<&dyn Entity> {
        self.registry.get(&id).map(|entry| entry.entity.as_ref())
    }

    pub fn get_entity_raw_mut(&mut self, id: EntityID) -> Option<&mut dyn Entity> {
        // compiler doesn't like the use of a closure here
        match self.registry.get_mut(&id) {
            Some(entry) => Some(entry.entity.as_mut()),
            None => None,
        }
    }

    pub fn get_all_ids(&self) -> impl Iterator<Item = EntityID> + '_ {
        self.registry.keys().cloned()
    }

    pub fn get_tickable_ids(&self) -> impl Iterator<Item = EntityID> + '_ {
        self.registry
            .iter()
            .filter_map(|(id, entry)| match entry.tick_mode {
                TickMode::Always => Some(*id),
                TickMode::Never => None,
                TickMode::WhenLoaded => {
                    match entry.chunk {
                        Some(coords) => {
                            if let Some(chunk) = self.get_chunk(coords) {
                                if chunk.is_loaded() {
                                    return Some(*id);
                                }
                            }
                            None
                        }
                        // need to tick transient entities!
                        // e.g. when a mob is dead it is off-screen, but
                        // needs to tick or else it will never respawn
                        None => Some(*id),
                    }
                }
            })
    }

    pub fn get_entity_chunk(&self, id: EntityID) -> Option<ChunkCoords> {
        self.registry.get(&id).and_then(|entry| entry.chunk)
    }

    pub fn get_around_entity(&mut self, id: EntityID) -> HashSet<EntityID> {
        let mut entities = HashSet::new();
        if let Some(coords) = self.registry.get(&id).and_then(|entry| entry.chunk) {
            for coords in Self::get_coords_around(coords, get_visibility_range()) {
                if let Some(chunk) = self.get_chunk(coords) {
                    entities.extend(chunk.get_all());
                }
            }
        }
        entities
    }

    pub fn get_player_ids(&self) -> impl Iterator<Item = i32> + '_ {
        self.registry.keys().filter_map(|id| {
            if let EntityID::Player(pc_id) = id {
                Some(*pc_id)
            } else {
                None
            }
        })
    }

    pub fn find_players(&self, f: impl Fn(&Player) -> bool) -> Vec<i32> {
        self.registry
            .values()
            .filter_map(|entry| {
                let entity_id = entry.entity.get_id();
                if let EntityID::Player(pc_id) = entity_id {
                    let pc = self.get_entity(entity_id).unwrap();
                    if f(pc) {
                        return Some(pc_id);
                    }
                }
                None
            })
            .collect()
    }

    pub fn get_npc_ids(&self) -> impl Iterator<Item = i32> + '_ {
        self.registry.keys().filter_map(|id| {
            if let EntityID::NPC(npc_id) = id {
                Some(*npc_id)
            } else {
                None
            }
        })
    }

    pub fn find_npcs(&self, f: impl Fn(&NPC) -> bool) -> Vec<i32> {
        self.registry
            .values()
            .filter_map(|entry| {
                let entity_id = entry.entity.get_id();
                if let EntityID::NPC(npc_id) = entity_id {
                    let npc = self.get_entity(entity_id).unwrap();
                    if f(npc) {
                        return Some(npc_id);
                    }
                }
                None
            })
            .collect()
    }

    pub fn validate_proximity(&self, ids: &[EntityID], range: u32) -> FFResult<()> {
        let mut locations = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(entry) = self.registry.get(id) {
                locations.push((
                    entry.entity.get_position(),
                    entry.entity.get_chunk_coords().i,
                ));
            } else {
                return Err(FFError::build(
                    Severity::Warning,
                    format!("Entity with ID {:?} doesn't exist", id),
                ));
            }
        }

        for i in 0..locations.len() {
            for j in (i + 1)..locations.len() {
                let inst1 = locations[i].1;
                let inst2 = locations[j].1;
                if inst1 != inst2 {
                    return Err(FFError::build(
                        Severity::Warning,
                        format!(
                            "Entity with ID {:?} is in a different instance than entity with ID {:?} ({} != {})",
                            ids[i], ids[j], inst1, inst2
                        ),
                    ));
                }

                let pos1 = locations[i].0;
                let pos2 = locations[j].0;
                let distance = pos1.distance_to(&pos2);
                if distance > range {
                    return Err(FFError::build(
                        Severity::Warning,
                        format!(
                            "Entity with ID {:?} is too far from entity with ID {:?} ({} > {})",
                            ids[i], ids[j], distance, range
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn gen_next_pc_id(&mut self) -> i32 {
        let id = self.next_pc_id;
        if id == u32::MAX {
            panic_log("Ran out of PC IDs");
        }
        self.next_pc_id += 1;
        id as i32
    }

    pub fn gen_next_npc_id(&mut self) -> i32 {
        let id = self.next_npc_id;
        if id == u32::MAX {
            panic_log("Ran out of NPC IDs");
        }
        self.next_npc_id += 1;
        id as i32
    }

    pub fn gen_next_slider_id(&mut self) -> i32 {
        let id = self.next_slider_id;
        if id == u32::MAX {
            panic_log("Ran out of slider IDs");
        }
        self.next_slider_id += 1;
        id as i32
    }

    pub fn gen_next_egg_id(&mut self) -> i32 {
        let id = self.next_egg_id;
        if id == u32::MAX {
            panic_log("Ran out of egg IDs");
        }
        self.next_egg_id += 1;
        id as i32
    }

    pub fn track(&mut self, entity: Box<dyn Entity>, tick_mode: TickMode) -> EntityID {
        let id = entity.get_id();
        if self.registry.contains_key(&id) {
            panic_log(&format!("Already tracking entity with id {:?}", id));
        }
        let entry = RegistryEntry {
            entity,
            chunk: None,
            tick_mode,
        };
        self.registry.insert(id, entry);
        id
    }

    pub fn untrack(&mut self, id: EntityID) -> Box<dyn Entity> {
        self.registry
            .remove(&id)
            .unwrap_or_else(|| {
                panic_log(&format!("Entity with id {:?} already untracked", id));
            })
            .entity
    }

    pub fn mark_for_cleanup(&mut self, id: EntityID) {
        self.entities_to_cleanup.insert(id);
    }

    pub fn update(&mut self, id: EntityID, to_chunk: Option<ChunkCoords>, broadcast: bool) {
        let entry = self.registry.get_mut(&id).unwrap_or_else(|| {
            panic_log(&format!("Entity with id {:?} untracked", id));
        });
        let from_chunk = entry.chunk;
        if from_chunk == to_chunk {
            return;
        }

        let around_from = self.remove_from_chunk(id);
        let around_to = self.insert_into_chunk(id, to_chunk);
        if let Some(coords) = from_chunk {
            if to_chunk.is_none() && self.should_cleanup_instance(coords.i) {
                self.instances_to_cleanup.insert(coords.i);
            }
        }

        if !broadcast {
            return;
        }

        let removed = around_from.difference(&around_to);
        for e in removed {
            // us to them
            let from = self.get_entity_raw(id).unwrap();
            if let Some(from_client) = from.get_client() {
                // possible for the ID to be unregistered if the instance was cleaned up
                if let Some(to) = self.get_entity_raw(*e) {
                    to.send_exit(&from_client);
                }
            }

            // them to us
            // possible for the ID to be unregistered if the instance was cleaned up
            if let Some(from) = self.get_entity_raw(*e) {
                if let Some(from_client) = from.get_client() {
                    let to = self.get_entity_raw(id).unwrap();
                    to.send_exit(&from_client);
                }
            }
        }

        let added = around_to.difference(&around_from);
        for e in added {
            // us to them
            let from = self.get_entity_raw(id).unwrap();
            if let Some(from_client) = from.get_client() {
                let to = self.get_entity_raw(*e).unwrap();
                to.send_enter(&from_client);
            }

            // them to us
            let from = self.get_entity_raw(*e).unwrap();
            if let Some(from_client) = from.get_client() {
                let to = self.get_entity_raw(id).unwrap();
                to.send_enter(&from_client);
            }
        }

        // if let EntityID::Player(pc_id) = id {
        //     log(
        //         Severity::Debug,
        //         &match self.registry[&id].chunk {
        //             Some(coords) => format!("Moved {} to {}", pc_id, coords),
        //             None => format!("Removed {} from map", pc_id),
        //         },
        //     );
        // }
    }

    /// Replay nearby entities after an in-instance teleport, including same-chunk moves.
    pub fn reload_player_chunks(&mut self, pc_id: i32) {
        let id = EntityID::Player(pc_id);
        let chunk = self.registry[&id].entity.get_chunk_coords();
        self.update(id, None, true);
        self.update(id, Some(chunk), true);
        // Removing the last player briefly queued this instance for cleanup.
        self.instances_to_cleanup.remove(&chunk.i);
    }

    pub fn set_tick(&mut self, id: EntityID, tick_mode: TickMode) -> FFResult<()> {
        let entry = self.registry.get_mut(&id).ok_or(FFError::build(
            Severity::Warning,
            format!("Entity with ID {:?} doesn't exist", id),
        ))?;
        entry.tick_mode = tick_mode;
        Ok(())
    }

    pub fn for_each_around(&mut self, id: EntityID, mut f: impl FnMut(&FFClient)) {
        for eid in self.get_around_entity(id).iter() {
            let e = self.registry.get(eid).unwrap().entity.as_ref();
            if let Some(client) = e.get_client() {
                f(&client);
            }
        }
    }

    pub fn get_channel_population(&self, channel_num: u8) -> usize {
        self.chunk_maps
            .iter()
            .filter(|(instance_id, _)| instance_id.channel_num == channel_num)
            .map(|(_, chunk_map)| chunk_map.get_player_count())
            .sum()
    }

    pub fn get_min_pop_channel_num(&self) -> u8 {
        let num_channels = config_get().shard.num_channels.get();
        (1..=num_channels)
            .min_by_key(|channel_num| self.get_channel_population(*channel_num))
            .unwrap()
    }

    pub fn get_num_instances(&self) -> usize {
        self.chunk_maps.len()
    }

    pub fn get_num_base_instances(&self) -> usize {
        self.chunk_maps
            .keys()
            .filter(|iid| iid.instance_num.is_none())
            .count()
    }

    pub fn get_num_chunks(&self) -> usize {
        self.chunk_maps.len() * NCHUNKS * NCHUNKS
    }

    pub fn get_num_loaded_chunks(&self) -> usize {
        self.chunk_maps
            .values()
            .map(|cm| cm.loaded_chunk_count)
            .sum()
    }

    pub fn get_num_loaded_entities(&self) -> usize {
        self.chunk_maps
            .values()
            .map(|cm| cm.loaded_entity_count)
            .sum()
    }

    fn remove_from_chunk(&mut self, id: EntityID) -> HashSet<EntityID> {
        let mut affected = HashSet::new();
        let entry = self.registry.get_mut(&id).unwrap();
        if let Some(coords) = entry.chunk {
            entry.chunk = None;
            let instance_id = coords.i;
            let chunk = self.get_chunk_mut(coords).unwrap();
            if !chunk.remove(id) {
                panic_log(&format!(
                    "Chunk {:?} did not contain entity with ID {:?}",
                    coords, id
                ));
            }
            let chunk_was_loaded = chunk.is_loaded();
            let is_player = matches!(id, EntityID::Player(_));
            let mut chunks_unloaded = 0;
            let mut entities_in_unloaded = 0;
            let coords_around = Self::get_coords_around(coords, get_visibility_range());
            for coords in coords_around {
                if let Some(chunk) = self.get_chunk_mut(coords) {
                    affected.extend(chunk.get_all());
                    if is_player {
                        let was_loaded = chunk.is_loaded();
                        chunk.load_count -= 1;
                        if was_loaded && chunk.load_count == 0 {
                            chunks_unloaded += 1;
                            entities_in_unloaded += chunk.tracked.len();
                        }
                    }
                }
            }
            let cm = self.chunk_maps.get_mut(&instance_id).unwrap();
            if chunk_was_loaded {
                cm.loaded_entity_count -= 1;
            }
            cm.loaded_chunk_count -= chunks_unloaded;
            cm.loaded_entity_count -= entities_in_unloaded;
        }
        affected
    }

    fn insert_into_chunk(
        &mut self,
        id: EntityID,
        to_chunk: Option<ChunkCoords>,
    ) -> HashSet<EntityID> {
        let mut affected = HashSet::new();
        if let Some(coords) = to_chunk {
            let instance_id = coords.i;
            if let Some(chunk) = self.get_chunk_mut(coords) {
                if !chunk.insert(id) {
                    panic_log(&format!(
                        "Chunk {:?} already contained entity with ID {:?}",
                        coords, id
                    ));
                }
                let chunk_already_loaded = chunk.is_loaded();
                let entry = self.registry.get_mut(&id).unwrap();
                entry.chunk = to_chunk;
                let is_player = matches!(id, EntityID::Player(_));
                let mut chunks_loaded = 0;
                let mut entities_in_loaded = 0;
                let coords_around = Self::get_coords_around(coords, get_visibility_range());
                for coords in coords_around {
                    if let Some(chunk) = self.get_chunk_mut(coords) {
                        if is_player {
                            let was_loaded = chunk.is_loaded();
                            chunk.load_count += 1;
                            if !was_loaded {
                                chunks_loaded += 1;
                                entities_in_loaded += chunk.tracked.len();
                            }
                        }
                        affected.extend(chunk.get_all());
                    }
                }
                affected.remove(&id); // we don't want ourself in this
                let cm = self.chunk_maps.get_mut(&instance_id).unwrap();
                if chunk_already_loaded {
                    cm.loaded_entity_count += 1;
                }
                cm.loaded_chunk_count += chunks_loaded;
                cm.loaded_entity_count += entities_in_loaded;
            }
        }
        affected
    }

    fn get_chunk(&self, coords: ChunkCoords) -> Option<&Chunk> {
        if (0..NCHUNKS as i32).contains(&coords.x) && (0..NCHUNKS as i32).contains(&coords.y) {
            let chunk_map = self.chunk_maps.get(&coords.i)?;
            let chunk = &chunk_map.chunks[coords.x as usize][coords.y as usize];
            return Some(chunk);
        }
        None
    }

    fn get_chunk_mut(&mut self, coords: ChunkCoords) -> Option<&mut Chunk> {
        if (0..NCHUNKS as i32).contains(&coords.x) && (0..NCHUNKS as i32).contains(&coords.y) {
            let chunk_map = self.init_instance(coords.i);
            let chunk = &mut chunk_map.chunks[coords.x as usize][coords.y as usize];
            return Some(chunk);
        }
        None
    }

    fn get_coords_around(coords: ChunkCoords, range: usize) -> Vec<ChunkCoords> {
        let num_around = (range * 2 + 1) * (range * 2 + 1);
        let mut coords_around = Vec::with_capacity(num_around);
        let range = range as i32;
        for x in (coords.x - range)..=(coords.x + range) {
            for y in (coords.y - range)..=(coords.y + range) {
                coords_around.push(ChunkCoords { x, y, i: coords.i });
            }
        }
        coords_around
    }

    fn init_instance(&mut self, instance_id: InstanceID) -> &mut ChunkMap {
        let new = !self.chunk_maps.contains_key(&instance_id);
        self.chunk_maps.entry(instance_id).or_insert_with(|| {
            let chunks = unsafe {
                let ptr = alloc::alloc(Layout::new::<[[Chunk; NCHUNKS]; NCHUNKS]>())
                    as *mut [[Chunk; NCHUNKS]; NCHUNKS];
                if ptr.is_null() {
                    panic_log("Failed to allocate memory for chunk map");
                }
                for x in 0..NCHUNKS {
                    for y in 0..NCHUNKS {
                        let chunk_ptr = &mut (*ptr)[x][y] as *mut Chunk;
                        chunk_ptr.write(Chunk::default());
                    }
                }
                Box::from_raw(ptr)
            };
            let chunk_map = ChunkMap {
                chunks,
                loaded_chunk_count: 0,
                loaded_entity_count: 0,
            };
            log(
                Severity::Debug,
                &format!("Initialized instance {}", instance_id),
            );
            chunk_map
        });
        if instance_id.instance_num.is_some() && new {
            let main_instance = InstanceID {
                channel_num: instance_id.channel_num,
                map_num: instance_id.map_num,
                instance_num: None,
            };
            let mut npc_count = 0;
            let mut id_mappings = HashMap::new();
            let mut tight_follow_mappings = HashMap::new();
            for x in 0..NCHUNKS {
                for y in 0..NCHUNKS {
                    let template_chunks = &self.chunk_maps.get(&main_instance).unwrap().chunks;
                    let template_chunk = &template_chunks[x][y];
                    for id in template_chunk.get_all().clone() {
                        let tick_mode = self.registry[&id].tick_mode;
                        if let EntityID::NPC(_) = id {
                            let mut npc = self.get_entity::<NPC>(id).unwrap().clone();
                            npc.instance_id = instance_id;
                            let new_id = self.gen_next_npc_id();
                            id_mappings.insert(id, EntityID::NPC(new_id));
                            npc.id = new_id;

                            // since there's no guarantee on what order the NPCs will be iterated upon,
                            // we update follow ids after everyone is cloned
                            if let Some(tight_follow) = npc.tight_follow {
                                tight_follow_mappings.insert(new_id, tight_follow);
                            }

                            let chunk_pos = npc.get_chunk_coords();
                            let new_npc_id = self.track(Box::new(npc), tick_mode);
                            self.update(new_npc_id, Some(chunk_pos), false);
                            npc_count += 1;
                        }
                    }
                }
            }

            // update leaders
            for (new_npc_id, (old_leader_id, offset)) in tight_follow_mappings {
                let npc: &mut NPC = self.get_entity_mut(EntityID::NPC(new_npc_id)).unwrap();
                let new_leader_id = id_mappings[&old_leader_id];
                npc.tight_follow = Some((new_leader_id, offset));
            }

            log(
                Severity::Debug,
                &format!("Copied {} NPCs to instance {}", npc_count, instance_id),
            );
        }
        self.chunk_maps.get_mut(&instance_id).unwrap()
    }

    pub fn garbage_collect_instances(&mut self) -> Vec<Box<dyn Entity>> {
        let mut entities = Vec::new();
        let instances_to_cleanup = self.instances_to_cleanup.clone();
        for instance_id in instances_to_cleanup {
            entities.extend(self.cleanup_instance(instance_id));
        }
        self.instances_to_cleanup.clear();
        entities
    }

    pub fn garbage_collect_entities(&mut self) -> Vec<Box<dyn Entity>> {
        let mut entities = Vec::new();
        let entities_to_cleanup = self.entities_to_cleanup.clone();
        for id in entities_to_cleanup {
            entities.push(self.untrack(id));
        }
        self.entities_to_cleanup.clear();
        entities
    }

    fn cleanup_instance(&mut self, instance_id: InstanceID) -> Vec<Box<dyn Entity>> {
        let mut entities = Vec::new();
        let chunk_map = self.chunk_maps.get(&instance_id).unwrap();
        for id in chunk_map.get_ids() {
            entities.push(self.untrack(id));
        }
        self.chunk_maps.remove(&instance_id);
        log(
            Severity::Debug,
            &format!("Cleaned up instance {}", instance_id),
        );
        entities
    }

    fn should_cleanup_instance(&mut self, instance_id: InstanceID) -> bool {
        if instance_id.instance_num.is_none() {
            return false; // don't clean up the main instance
        }

        let chunk_map = self.chunk_maps.get(&instance_id).unwrap();
        chunk_map.get_player_count() == 0
    }
}
impl Default for EntityMap {
    fn default() -> Self {
        Self {
            registry: HashMap::new(),
            chunk_maps: HashMap::new(),
            instances_to_cleanup: HashSet::new(),
            entities_to_cleanup: HashSet::new(),
            next_pc_id: 1,
            next_npc_id: 1,
            next_slider_id: 1,
            next_egg_id: 1,
        }
    }
}

#[derive(Default, Clone)]
pub struct Chunk {
    load_count: usize,
    tracked: HashSet<EntityID>,
}

impl Chunk {
    fn is_loaded(&self) -> bool {
        self.load_count > 0
    }

    fn get_all(&self) -> &HashSet<EntityID> {
        &self.tracked
    }

    fn get_player_count(&self) -> usize {
        self.tracked
            .iter()
            .filter(|id| matches!(id, EntityID::Player(_)))
            .count()
    }

    fn insert(&mut self, id: EntityID) -> bool {
        self.tracked.insert(id)
    }

    fn remove(&mut self, id: EntityID) -> bool {
        self.tracked.remove(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        entity::{Combatant, Entity, EntityID},
        net::FFClient,
        state::ShardServerState,
        Position,
    };
    use std::any::Any;

    /// Minimal mock entity for testing chunk operations.
    #[derive(Debug)]
    struct MockEntity {
        id: EntityID,
        position: Position,
        instance_id: InstanceID,
    }
    impl MockEntity {
        fn new_player(pc_id: i32, pos: Position, instance_id: InstanceID) -> Self {
            Self {
                id: EntityID::Player(pc_id),
                position: pos,
                instance_id,
            }
        }

        fn new_npc(npc_id: i32, pos: Position, instance_id: InstanceID) -> Self {
            Self {
                id: EntityID::NPC(npc_id),
                position: pos,
                instance_id,
            }
        }
    }
    impl Display for MockEntity {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "MockEntity {:?}", self)
        }
    }
    impl Entity for MockEntity {
        fn get_id(&self) -> EntityID {
            self.id
        }
        fn get_client(&self) -> Option<FFClient> {
            None
        }
        fn get_position(&self) -> Position {
            self.position
        }
        fn get_rotation(&self) -> i32 {
            0
        }
        fn get_speed(&self, _: bool) -> i32 {
            0
        }
        fn get_chunk_coords(&self) -> ChunkCoords {
            ChunkCoords::from_pos_inst(self.position, self.instance_id)
        }
        fn set_position(&mut self, pos: Position) {
            self.position = pos;
        }
        fn set_rotation(&mut self, _: i32) {}
        fn send_enter(&self, _: &FFClient) {}
        fn send_exit(&self, _: &FFClient) {}
        fn cleanup(self: Box<Self>, _state: &mut ShardServerState) {}
        fn as_combatant(&self) -> Option<&dyn Combatant> {
            None
        }
        fn as_combatant_mut(&mut self) -> Option<&mut dyn Combatant> {
            None
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    /// Position that maps to chunk (x, y) in the grid.
    /// Each chunk spans MAP_SIZE / NCHUNKS units.
    fn pos_for_chunk(x: i32, y: i32) -> Position {
        let chunk_size = MAP_SIZE / NCHUNKS as i32;
        Position {
            x: x * chunk_size + chunk_size / 2,
            y: y * chunk_size + chunk_size / 2,
            z: 0,
        }
    }

    fn default_instance() -> InstanceID {
        InstanceID::default()
    }

    /// Place an entity into the map and return its chunk coords.
    fn place_entity(map: &mut EntityMap, entity: MockEntity) -> (EntityID, ChunkCoords) {
        let id = entity.get_id();
        let chunk = entity.get_chunk_coords();
        map.track(Box::new(entity), TickMode::WhenLoaded);
        map.update(id, Some(chunk), false);
        (id, chunk)
    }

    #[test]
    fn test_loaded_counts_zero_initially() {
        let map = EntityMap::default();
        assert_eq!(map.get_num_loaded_chunks(), 0);
        assert_eq!(map.get_num_loaded_entities(), 0);
    }

    #[test]
    fn test_player_loads_surrounding_chunks() {
        let mut map = EntityMap::default();
        let inst = default_instance();

        // Place a player at chunk (64, 64) — safely in the middle
        let pos = pos_for_chunk(64, 64);
        let player = MockEntity::new_player(1, pos, inst);
        place_entity(&mut map, player);

        // With visibility_range=1, a player loads a 3x3 grid = 9 chunks
        assert_eq!(map.get_num_loaded_chunks(), 9);
        // The player itself is in a loaded chunk
        assert_eq!(map.get_num_loaded_entities(), 1);
    }

    #[test]
    fn test_npc_in_loaded_chunk_counted() {
        let mut map = EntityMap::default();
        let inst = default_instance();
        let pos = pos_for_chunk(64, 64);

        // Place an NPC first (chunk is unloaded, so not counted)
        let npc = MockEntity::new_npc(1, pos, inst);
        place_entity(&mut map, npc);
        assert_eq!(map.get_num_loaded_chunks(), 0);
        assert_eq!(map.get_num_loaded_entities(), 0);

        // Now place a player at the same position — NPC's chunk becomes loaded
        let player = MockEntity::new_player(1, pos, inst);
        place_entity(&mut map, player);
        assert_eq!(map.get_num_loaded_chunks(), 9);
        // Both the player and the NPC are in loaded chunks
        assert_eq!(map.get_num_loaded_entities(), 2);
    }

    #[test]
    fn test_removing_player_unloads_chunks() {
        let mut map = EntityMap::default();
        let inst = default_instance();
        let pos = pos_for_chunk(64, 64);

        let player = MockEntity::new_player(1, pos, inst);
        let (player_id, _) = place_entity(&mut map, player);

        assert_eq!(map.get_num_loaded_chunks(), 9);
        assert_eq!(map.get_num_loaded_entities(), 1);

        // Remove player from the map
        map.update(player_id, None, false);

        assert_eq!(map.get_num_loaded_chunks(), 0);
        assert_eq!(map.get_num_loaded_entities(), 0);
    }

    #[test]
    fn test_two_players_overlapping_visibility() {
        let mut map = EntityMap::default();
        let inst = default_instance();

        // Two players in the same chunk
        let pos = pos_for_chunk(64, 64);
        let p1 = MockEntity::new_player(1, pos, inst);
        let p2 = MockEntity::new_player(2, pos, inst);
        place_entity(&mut map, p1);
        place_entity(&mut map, p2);

        // Same 3x3 grid, but load_count=2 on each. Still 9 loaded chunks.
        assert_eq!(map.get_num_loaded_chunks(), 9);
        // Both players are in loaded chunks
        assert_eq!(map.get_num_loaded_entities(), 2);
    }

    #[test]
    fn test_removing_one_of_two_overlapping_players() {
        let mut map = EntityMap::default();
        let inst = default_instance();
        let pos = pos_for_chunk(64, 64);

        let p1 = MockEntity::new_player(1, pos, inst);
        let p2 = MockEntity::new_player(2, pos, inst);
        let (p1_id, _) = place_entity(&mut map, p1);
        place_entity(&mut map, p2);

        // Remove one player — chunks still loaded by the other
        map.update(p1_id, None, false);
        assert_eq!(map.get_num_loaded_chunks(), 9);
        // Only p2 remains
        assert_eq!(map.get_num_loaded_entities(), 1);
    }

    #[test]
    fn test_npc_added_to_already_loaded_chunk() {
        let mut map = EntityMap::default();
        let inst = default_instance();
        let pos = pos_for_chunk(64, 64);

        // Player first
        let player = MockEntity::new_player(1, pos, inst);
        place_entity(&mut map, player);
        assert_eq!(map.get_num_loaded_entities(), 1);

        // Add NPC to the same loaded chunk
        let npc = MockEntity::new_npc(1, pos, inst);
        place_entity(&mut map, npc);
        assert_eq!(map.get_num_loaded_entities(), 2);

        // Add another NPC
        let npc2 = MockEntity::new_npc(2, pos, inst);
        place_entity(&mut map, npc2);
        assert_eq!(map.get_num_loaded_entities(), 3);
    }

    #[test]
    fn test_npc_removed_from_loaded_chunk() {
        let mut map = EntityMap::default();
        let inst = default_instance();
        let pos = pos_for_chunk(64, 64);

        let player = MockEntity::new_player(1, pos, inst);
        let npc = MockEntity::new_npc(1, pos, inst);
        place_entity(&mut map, player);
        let (npc_id, _) = place_entity(&mut map, npc);
        assert_eq!(map.get_num_loaded_entities(), 2);

        // Remove NPC — player still loaded, count goes to 1
        map.update(npc_id, None, false);
        assert_eq!(map.get_num_loaded_entities(), 1);
        assert_eq!(map.get_num_loaded_chunks(), 9);
    }

    #[test]
    fn test_player_moving_between_chunks() {
        let mut map = EntityMap::default();
        let inst = default_instance();

        let pos1 = pos_for_chunk(64, 64);
        let player = MockEntity::new_player(1, pos1, inst);
        let (player_id, _) = place_entity(&mut map, player);
        assert_eq!(map.get_num_loaded_chunks(), 9);

        // Move player to a distant chunk (no overlap with the old 3x3)
        let pos2 = pos_for_chunk(80, 80);
        let new_coords = ChunkCoords::from_pos_inst(pos2, inst);
        map.update(player_id, Some(new_coords), false);

        // Old chunks unloaded, new 3x3 loaded
        assert_eq!(map.get_num_loaded_chunks(), 9);
        assert_eq!(map.get_num_loaded_entities(), 1);
    }

    #[test]
    fn test_npcs_in_chunk_counted_on_load_transition() {
        let mut map = EntityMap::default();
        let inst = default_instance();
        let pos = pos_for_chunk(64, 64);

        // Place several NPCs first (no player = unloaded)
        for i in 1..=5 {
            let npc = MockEntity::new_npc(i, pos, inst);
            place_entity(&mut map, npc);
        }
        assert_eq!(map.get_num_loaded_entities(), 0);

        // A player arrives — all 5 NPCs become loaded
        let player = MockEntity::new_player(1, pos, inst);
        place_entity(&mut map, player);
        assert_eq!(map.get_num_loaded_entities(), 6); // 5 NPCs + 1 player

        // Player leaves — all unloaded again
        map.update(EntityID::Player(1), None, false);
        assert_eq!(map.get_num_loaded_entities(), 0);
        assert_eq!(map.get_num_loaded_chunks(), 0);
    }

    #[test]
    fn test_entity_at_chunk_boundary() {
        let mut map = EntityMap::default();
        let inst = default_instance();

        // Player at chunk (1, 1) — visibility includes (0,0) through (2,2)
        let player_pos = pos_for_chunk(1, 1);
        let player = MockEntity::new_player(1, player_pos, inst);
        place_entity(&mut map, player);

        // NPC at chunk (2, 2) — within the visibility of the player
        let npc_pos = pos_for_chunk(2, 2);
        let npc = MockEntity::new_npc(1, npc_pos, inst);
        place_entity(&mut map, npc);
        assert_eq!(map.get_num_loaded_entities(), 2);

        // NPC at chunk (3, 3) — outside the player's 3x3 visibility
        let far_npc_pos = pos_for_chunk(3, 3);
        let far_npc = MockEntity::new_npc(2, far_npc_pos, inst);
        place_entity(&mut map, far_npc);
        // The far NPC is in an unloaded chunk, shouldn't be counted
        assert_eq!(map.get_num_loaded_entities(), 2);
    }
}

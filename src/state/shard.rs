use std::{
    collections::{HashMap, HashSet},
    time::SystemTime,
};

use uuid::Uuid;

use crate::{
    ai,
    chunk::{EntityMap, InstanceID, TickMode},
    config::config_get,
    defines::*,
    entity::{Combatant, Egg, Entity, EntityID, Group, Player, Slider, NPC},
    enums::ItemLocation,
    error::{log, log_if_failed, FFError, FFResult, Severity},
    helpers,
    item::Item,
    net::{
        packet::{PacketID::*, *},
        LoginData,
    },
    skills::BuffEffect,
    tabledata::tdata_get,
    trade::TradeContext,
};

pub struct ShardServerState {
    pub shard_id: Option<i32>,
    pub login_server_conn_id: Option<Uuid>,
    pub login_data: HashMap<i64, LoginData>,
    pub entity_map: EntityMap,
    pub buyback_lists: HashMap<i32, Vec<Item>>,
    pub ongoing_trades: HashMap<Uuid, TradeContext>,
    pub groups: HashMap<Uuid, Group>,
    pub player_uid_to_id: HashMap<i64, i32>,
    pub pending_entering_uids: HashSet<i64>,
    pub pending_buff_effects: Vec<BuffEffect>,
}
impl Default for ShardServerState {
    fn default() -> Self {
        let mut state = Self {
            login_server_conn_id: None,
            shard_id: None,
            login_data: HashMap::new(),
            entity_map: EntityMap::default(),
            buyback_lists: HashMap::new(),
            ongoing_trades: HashMap::new(),
            groups: HashMap::new(),
            player_uid_to_id: HashMap::new(),
            pending_entering_uids: HashSet::new(),
            pending_buff_effects: Vec::new(),
        };

        let num_channels = config_get().shard.num_channels.get();
        if num_channels == 0 || num_channels > MAX_NUM_CHANNELS as u8 {
            panic!("Invalid number of channels {}", num_channels);
        }

        for channel_num in 1..=num_channels {
            for mut npc in tdata_get().make_all_npcs(&mut state.entity_map, channel_num) {
                if let Some(path) = tdata_get().get_npc_path(npc.ty) {
                    npc.path = Some(path);
                }

                let (ai, tick_mode) = ai::make_for_npc(&npc, false);
                npc.ai = ai;

                let chunk_pos = npc.get_chunk_coords();
                let entity_map = &mut state.entity_map;
                let id = entity_map.track(Box::new(npc), tick_mode);
                entity_map.update(id, Some(chunk_pos), false);
            }

            for egg in tdata_get().make_eggs(&mut state.entity_map, channel_num) {
                let chunk_pos = egg.get_chunk_coords();
                let entity_map = &mut state.entity_map;
                let id = entity_map.track(Box::new(egg), TickMode::Always);
                entity_map.update(id, Some(chunk_pos), false);
            }

            // spawn sliders uniformly across the circuit
            let mut slider_circuit = tdata_get().get_slider_path();
            let num_sliders = config_get().shard.num_sliders.get();
            let slider_gap_size = slider_circuit.get_total_length() / num_sliders as u32;
            let mut pos = slider_circuit.get_points()[0].pos;
            let mut dist_to_next = 0;
            let mut sliders_spawned = 0;
            loop {
                if dist_to_next > 0 {
                    let target_pos = slider_circuit.get_target_pos();
                    let dist_to_target = target_pos.distance_to(&pos);
                    if dist_to_target <= dist_to_next {
                        // next point is closer than the distance to the next slider,
                        // so we advance to the next point and continue
                        pos = target_pos;
                        dist_to_next -= dist_to_target;
                        slider_circuit.advance();
                    } else {
                        // next point is farther than the distance to the next slider,
                        // so we interpolate the position and prime a slider spawn
                        let (new_pos, _) = pos.interpolate(&target_pos, dist_to_next as f32);
                        pos = new_pos;
                        dist_to_next = 0;
                    }
                    continue;
                }

                // spawn slider here
                let instance_id = InstanceID {
                    channel_num,
                    map_num: ID_OVERWORLD,
                    instance_num: None,
                };
                let entity_map = &mut state.entity_map;
                let slider = Slider::new(
                    entity_map.gen_next_slider_id(),
                    pos,
                    0,
                    Some(slider_circuit.clone()),
                    instance_id,
                );
                sliders_spawned += 1;
                let chunk_pos = slider.get_chunk_coords();
                let id = entity_map.track(Box::new(slider), TickMode::Always);
                entity_map.update(id, Some(chunk_pos), false);
                dist_to_next = slider_gap_size;
                if sliders_spawned as usize == num_sliders {
                    break;
                }
            }
            log(
                Severity::Debug,
                &format!("Spawned {} sliders", sliders_spawned),
            );
        }
        state
    }
}
impl ShardServerState {
    pub fn get_npc(&self, npc_id: i32) -> FFResult<&NPC> {
        let id = EntityID::NPC(npc_id);
        self.entity_map.get_entity(id).ok_or(FFError::build(
            Severity::Warning,
            format!("NPC with ID {} doesn't exist", npc_id),
        ))
    }

    pub fn get_npc_mut(&mut self, npc_id: i32) -> FFResult<&mut NPC> {
        let id = EntityID::NPC(npc_id);
        self.entity_map.get_entity_mut(id).ok_or(FFError::build(
            Severity::Warning,
            format!("NPC with ID {} doesn't exist", npc_id),
        ))
    }

    pub fn get_player(&self, pc_id: i32) -> FFResult<&Player> {
        let id = EntityID::Player(pc_id);
        self.entity_map.get_entity(id).ok_or(FFError::build(
            Severity::Warning,
            format!("Player with ID {} doesn't exist", pc_id),
        ))
    }

    pub fn get_player_mut(&mut self, pc_id: i32) -> FFResult<&mut Player> {
        let id = EntityID::Player(pc_id);
        self.entity_map.get_entity_mut(id).ok_or(FFError::build(
            Severity::Warning,
            format!("Player with ID {} doesn't exist", pc_id),
        ))
    }

    pub fn get_player_by_uid(&self, pc_uid: i64) -> Option<&Player> {
        self.player_uid_to_id
            .get(&pc_uid)
            .and_then(|pc_id| self.get_player(*pc_id).ok())
    }

    pub fn get_slider(&self, slider_id: i32) -> FFResult<&Slider> {
        let id = EntityID::Slider(slider_id);
        self.entity_map.get_entity(id).ok_or(FFError::build(
            Severity::Warning,
            format!("Slider with ID {} doesn't exist", slider_id),
        ))
    }

    pub fn get_slider_mut(&mut self, slider_id: i32) -> FFResult<&mut Slider> {
        let id = EntityID::Slider(slider_id);
        self.entity_map.get_entity_mut(id).ok_or(FFError::build(
            Severity::Warning,
            format!("Slider with ID {} doesn't exist", slider_id),
        ))
    }

    pub fn get_egg(&self, egg_id: i32) -> FFResult<&Egg> {
        let id = EntityID::Egg(egg_id);
        self.entity_map.get_entity(id).ok_or(FFError::build(
            Severity::Warning,
            format!("Egg with ID {} doesn't exist", egg_id),
        ))
    }

    pub fn get_egg_mut(&mut self, egg_id: i32) -> FFResult<&mut Egg> {
        let id = EntityID::Egg(egg_id);
        self.entity_map.get_entity_mut(id).ok_or(FFError::build(
            Severity::Warning,
            format!("Egg with ID {} doesn't exist", egg_id),
        ))
    }

    pub fn get_entity(&self, id: EntityID) -> FFResult<&dyn Entity> {
        self.entity_map.get_entity_raw(id).ok_or(FFError::build(
            Severity::Warning,
            format!("Entity with ID {:?} doesn't exist", id),
        ))
    }

    pub fn get_entity_mut(&mut self, id: EntityID) -> FFResult<&mut dyn Entity> {
        self.entity_map.get_entity_raw_mut(id).ok_or(FFError::build(
            Severity::Warning,
            format!("Entity with ID {:?} doesn't exist", id),
        ))
    }

    pub fn get_combatant(&self, id: EntityID) -> FFResult<&dyn Combatant> {
        let entity = self.entity_map.get_entity_raw(id).ok_or(FFError::build(
            Severity::Warning,
            format!("Entity with ID {:?} doesn't exist", id),
        ))?;

        entity.as_combatant().ok_or(FFError::build(
            Severity::Warning,
            format!("Entity with ID {:?} isn't a combatant", id),
        ))
    }

    pub fn get_combatant_mut(&mut self, id: EntityID) -> FFResult<&mut dyn Combatant> {
        let entity = self
            .entity_map
            .get_entity_raw_mut(id)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Entity with ID {:?} doesn't exist", id),
            ))?;

        entity.as_combatant_mut().ok_or(FFError::build(
            Severity::Warning,
            format!("Entity with ID {:?} isn't a combatant", id),
        ))
    }

    pub fn check_for_expired_items(&mut self, time: SystemTime) {
        let pc_ids: Vec<i32> = self.entity_map.get_player_ids().collect();
        for pc_id in pc_ids {
            self.check_and_remove_expired_items(pc_id, time);
        }
    }

    pub fn check_and_remove_expired_items(&mut self, pc_id: i32, time: SystemTime) {
        let remove_bank_items = config_get().shard.remove_expired_items_from_bank.get();
        let player = self.get_player_mut(pc_id).unwrap();
        // Registered trade stacks must remain intact until the trade finishes.
        if player.trade_id.is_some() {
            return;
        }
        let expired = player.find_items_any(|item| item.has_expired(time));
        let mut equip_changes = Vec::new();
        let mut dismounted = false;
        for (location, slot) in expired {
            if location == ItemLocation::Bank && !remove_bank_items {
                continue;
            }
            if player.set_item(location, slot, None).is_err() {
                continue;
            }
            // Bank items are removed silently (we don't reach here unless remove_bank_items is true).
            if location == ItemLocation::Bank {
                continue;
            }
            let client = player.get_client().unwrap();
            if location == ItemLocation::Equip {
                equip_changes.push(slot);
                if slot == EQUIP_SLOT_VEHICLE as usize && player.vehicle_speed.take().is_some() {
                    dismounted = true;
                    client.send_packet(
                        P_FE2CL_PC_VEHICLE_OFF_SUCC,
                        &sP_FE2CL_PC_VEHICLE_OFF_SUCC::default(),
                    );
                }
            }
            if let Some(packet) = log_if_failed(
                PacketBuilder::new(P_FE2CL_PC_DELETE_TIME_LIMIT_ITEM)
                    .with(&sP_FE2CL_PC_DELETE_TIME_LIMIT_ITEM { iItemListCount: 1 })
                    .with(&sTimeLimitItemDeleteInfo2CL {
                        eIL: location as i32,
                        iSlotNum: slot as i32,
                    })
                    .build(),
            ) {
                client.send_payload(packet);
            }
            client.send_packet(
                P_FE2CL_REP_PC_ITEM_DELETE_SUCC,
                &sP_FE2CL_REP_PC_ITEM_DELETE_SUCC {
                    eIL: location as i32,
                    iSlotNum: slot as i32,
                },
            );
            if location == ItemLocation::Equip {
                helpers::send_equip_change(&client, pc_id, slot as i32, None);
            }
        }
        let flags = player.get_state_bit_flag();
        for slot in equip_changes {
            self.entity_map
                .for_each_around(EntityID::Player(pc_id), |client| {
                    helpers::send_equip_change(client, pc_id, slot as i32, None);
                });
        }
        if dismounted {
            helpers::broadcast_state(pc_id, flags, self);
        }
    }

    pub fn tick_garbage_collection(&mut self) {
        let mut removed_entities = self.entity_map.garbage_collect_instances();
        removed_entities.extend(self.entity_map.garbage_collect_entities());

        let total_removed = removed_entities.len();
        for entity in removed_entities {
            entity.cleanup(self);
        }

        if total_removed > 0 {
            log(
                Severity::Debug,
                &format!("Garbage collected {} entities", total_removed),
            );
        }
    }

    pub fn tick_groups(&mut self) {
        for group in self.groups.values() {
            let (pc_group_data, npc_group_data) = group.get_member_data(self);
            let mut pkt = PacketBuilder::new(P_FE2CL_PC_GROUP_JOIN_SUCC).with(
                &sP_FE2CL_PC_GROUP_MEMBER_INFO {
                    iID: unused!(),
                    iMemberPCCnt: pc_group_data.len() as i32,
                    iMemberNPCCnt: npc_group_data.len() as i32,
                },
            );

            for pc_data in &pc_group_data {
                pkt.push(pc_data);
            }
            for npc_data in &npc_group_data {
                pkt.push(npc_data);
            }

            if let Some(pkt) = log_if_failed(pkt.build()) {
                for eid in group.get_member_ids() {
                    let entity = self.entity_map.get_entity_raw(*eid).unwrap();
                    if let Some(client) = entity.get_client() {
                        client.send_payload(pkt.clone());
                    }
                }
            }
        }
    }

    pub fn tick_entities(&mut self, time: SystemTime) {
        let eids: Vec<EntityID> = self.entity_map.get_tickable_ids().collect();
        for eid in eids {
            self.tick_entity(eid, &time);
        }

        // Process all buff effects that were generated during this tick
        let buff_effects = std::mem::take(&mut self.pending_buff_effects);
        for buff_effect in buff_effects {
            match buff_effect {
                BuffEffect::HealEntity { target, amount } => {
                    if let Some(combatant) = log_if_failed(self.get_combatant_mut(target)) {
                        combatant.heal(amount);
                    }
                }
                BuffEffect::DamageEntity {
                    target,
                    source,
                    damage,
                } => {
                    if let Some(combatant) = log_if_failed(self.get_combatant_mut(target)) {
                        combatant.take_damage(damage, source);
                    }
                }
            }
        }
    }

    fn tick_entity(&mut self, id: EntityID, time: &SystemTime) {
        match id {
            EntityID::Egg(egg_id) => Egg::tick(self, time, egg_id),
            EntityID::Slider(slider_id) => Slider::tick(self, slider_id),
            EntityID::NPC(npc_id) => NPC::tick(self, npc_id),
            EntityID::Player(pc_id) => Player::tick(self, pc_id, time),
        }
    }
}

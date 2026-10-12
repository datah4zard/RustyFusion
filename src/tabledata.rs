#![allow(non_snake_case)]
#![allow(dead_code)]

use rand::{rngs::ThreadRng, thread_rng, Rng};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    sync::OnceLock,
    time::{Duration, SystemTime},
};

use crate::{
    chunk::{world_pos_to_chunk_pos, ChunkCoords, EntityMap, InstanceID},
    config::config_get,
    defines::*,
    entity::{Egg, EntityID, NPC},
    enums::*,
    error::{log, log_error, log_if_failed, FFError, FFResult, Severity},
    item::{CrocPotData, Item, ItemStats, Reward, VendorData, VendorItem},
    mission::{MissionDefinition, TaskDefinition},
    nano::{NanoStats, NanoTuning},
    net::FFProtocol,
    path::{Path, PathPoint},
    skills::Skill,
    util, Position,
};

static TABLE_DATA: OnceLock<TableData> = OnceLock::new();

struct XDTData {
    vendor_data: HashMap<i32, VendorData>,
    item_data: HashMap<(i16, ItemType), ItemStats>,
    crocpot_data: HashMap<i16, CrocPotData>,
    transportation_data: TransportationData,
    instance_data: InstanceData,
    nano_data: NanoData,
    skill_data: SkillData,
    mission_data: MissionData,
    respawn_data: Vec<RespawnPoint>,
    player_data: HashMap<i16, PlayerData>,
    npc_data: HashMap<i32, NPCData>,
}
impl XDTData {
    fn load() -> Result<Self, String> {
        let protocol = config_get().general.protocol.get();
        let root = load_json(match protocol {
            FFProtocol::v0104 => "xdt.json",
            FFProtocol::v1013 => "xdt1013.json",
        })?;

        Ok(Self {
            vendor_data: load_vendor_data(&root)
                .map_err(|e| format!("Error loading vendor data: {}", e))?,
            item_data: load_item_data(&root)
                .map_err(|e| format!("Error loading item data: {}", e))?,
            crocpot_data: load_crocpot_data(&root)
                .map_err(|e| format!("Error loading crocpot data: {}", e))?,
            transportation_data: load_transportation_data(&root)
                .map_err(|e| format!("Error loading transportation data: {}", e))?,
            instance_data: load_instance_data(&root)
                .map_err(|e| format!("Error loading instance data: {}", e))?,
            nano_data: load_nano_data(&root)
                .map_err(|e| format!("Error loading nano data: {}", e))?,
            skill_data: load_skill_data(&root)
                .map_err(|e| format!("Error loading skill data: {}", e))?,
            mission_data: load_mission_data(&root)
                .map_err(|e| format!("Error loading mission data: {}", e))?,
            respawn_data: load_respawn_data(&root)
                .map_err(|e| format!("Error loading respawn data: {}", e))?,
            player_data: load_player_data(&root)
                .map_err(|e| format!("Error loading player data: {}", e))?,
            npc_data: load_npc_data(&root).map_err(|e| format!("Error loading NPC data: {}", e))?,
        })
    }
}

#[derive(Debug)]
struct NPCSpawnData {
    group_id: Option<i32>,
    npc_type: i32,
    pos: Position,
    angle: i32,
    map_num: Option<u32>,
    followers: Vec<FollowerData>,
}

#[derive(Debug)]
struct EggSpawnData {
    egg_type: i32,
    pos: Position,
    map_num: Option<u32>,
}

#[derive(Debug)]
struct FollowerData {
    npc_type: i32,
    offset: Position,
}

#[derive(Debug)]
pub struct TripData {
    pub npc_id: i32,
    pub transportation_type: TransportationType,
    pub start_location: i32,
    pub end_location: i32,
    pub cost: u32,
    pub speed: i32,
    pub route_number: i32,
}

#[derive(Debug)]
pub struct TransporterData {
    pub npc_type: i32,
    pub pos: Position,
}

struct TransportationData {
    trip_data: HashMap<i32, TripData>,
    scamper_data: HashMap<i32, TransporterData>,
    monkey_skyway_data: HashMap<i32, TransporterData>,
}

#[derive(Debug)]
pub struct WarpData {
    pub pos: Position,
    pub npc_type: i32,
    pub is_instance: bool,
    pub is_group_warp: bool,
    pub map_num: u32,
    pub min_level: i16,
    pub req_task: Option<(i32, i32)>, // mission id, task id
    pub req_item: Option<(ItemType, i16)>,
    pub req_item_consumed: Option<(ItemType, i16)>,
    pub cost: u32,
}

pub struct MapData {
    pub ep_id: Option<u32>,
    pub map_square: (i32, i32),
}

pub struct WorldNameData {
    pub area_name: String,
    pub zone_name: String,
}

struct WorldNameDataContainer {
    world_name_data: HashMap<i32, WorldNameData>,
    lookup_grid: HashMap<(i32, i32), i32>,
}

struct InstanceData {
    warp_data: HashMap<i32, WarpData>,
    map_data: HashMap<u32, MapData>,
}

struct NanoData {
    nano_stats: HashMap<i16, NanoStats>,
    nano_tunings: HashMap<i16, NanoTuning>,
}

struct SkillData {
    skills: HashMap<i16, Skill>,
}

struct MissionData {
    mission_definitions: HashMap<i32, MissionDefinition>,
    task_definitions: HashMap<i32, TaskDefinition>,
    rewards: HashMap<i32, Reward>,
}

struct RespawnPoint {
    pos: Position,
    map_num: u32,
}

#[derive(Debug, Default)]
pub struct PlayerStats {
    pub hp_up: u32,
    pub max_hp: u32,
    pub req_fm_nano_create: u32,
    pub req_fm_nano_tune: u32,
    pub fm_limit: u32,
    pub nano_quest_task_id: Option<i32>,
    pub nano_id: i16,
}

#[derive(Debug, Default)]
struct PlayerData {
    stats: PlayerStats,
}

#[derive(Debug)]
pub struct NPCStats {
    pub team: CombatantTeam,
    pub style: CombatStyle,
    pub level: i16,
    pub max_hp: u32,
    pub power: i32,
    pub defense: i32,
    pub radius: u32,
    pub walk_speed: i32,
    pub run_speed: i32,
    pub sight_range: u32,
    pub idle_range: u32,
    pub combat_range: u32,
    pub attack_range: u32,
    pub regen_time: u64,
    pub delay_time: u64, // generic value for various delays
    pub ai_type: u8,     // TODO investigate further
    pub bark_type: Option<usize>,
}

#[derive(Debug)]
struct NPCData {
    name: String,
    stats: NPCStats,
}

pub struct EggStats {
    pub crate_id: Option<i16>,
    pub effect_id: Option<i32>,
    pub effect_duration: Duration,
    pub respawn_time: Duration,
}

#[derive(Debug, Deserialize)]
struct CrateDropChance {
    DropChance: i32,
    DropChanceTotal: i32,
    CrateTypeDropWeights: Vec<i32>,
}

#[derive(Debug, Deserialize)]
struct CrateDropType {
    CrateIDs: Vec<i32>,
}

#[derive(Debug, Deserialize)]
struct CrateData {
    ItemSetID: i32,
    RarityWeightID: i32,
}

#[derive(Debug, Deserialize)]
struct MiscDropChance {
    PotionDropChance: usize,
    PotionDropChanceTotal: usize,
    BoostDropChance: usize,
    BoostDropChanceTotal: usize,
    TaroDropChance: usize,
    TaroDropChanceTotal: usize,
    FMDropChance: usize,
    FMDropChanceTotal: usize,
}

#[derive(Debug, Deserialize)]
struct MiscDropType {
    PotionAmount: u32,
    BoostAmount: u32,
    TaroAmount: u32,
    FMAmount: u32,
}

#[derive(Debug, Deserialize)]
struct MobDrop {
    CrateDropChanceID: i32,
    CrateDropTypeID: i32,
    MiscDropChanceID: i32,
    MiscDropTypeID: i32,
}

#[derive(Debug, Deserialize)]
struct MobDropData {
    MobDropID: i32,
}

#[derive(Debug, Deserialize)]
struct RarityWeights {
    Weights: Vec<i32>,
}

#[derive(Debug, Deserialize)]
struct ItemSet {
    IgnoreRarity: bool,
    IgnoreGender: bool,
    DefaultItemWeight: i32,
    AlterRarityMap: HashMap<String, i32>,
    AlterGenderMap: HashMap<String, i32>,
    AlterItemWeightMap: HashMap<String, i32>,
    ItemReferenceIDs: Vec<i32>,
}

#[derive(Debug, Deserialize)]
struct ItemReference {
    ItemID: i32,
    Type: i32,
}

struct EggData {
    egg_stats: HashMap<i32, EggStats>,
    eggs: Vec<EggSpawnData>,
}

#[derive(Debug, Deserialize)]
struct EventDrop {
    MobDropID: i32,
}

struct DropData {
    events: HashMap<i32, EventDrop>,
    crate_drop_chances: HashMap<i32, CrateDropChance>,
    crate_drop_types: HashMap<i32, CrateDropType>,
    crate_data: HashMap<i32, CrateData>,
    misc_drop_chances: HashMap<i32, MiscDropChance>,
    misc_drop_types: HashMap<i32, MiscDropType>,
    mob_drops: HashMap<i32, MobDrop>,
    mob_drop_data: HashMap<i32, MobDropData>,
    rarity_weights: HashMap<i32, RarityWeights>,
    item_sets: HashMap<i32, ItemSet>,
    item_refs: HashMap<i32, ItemReference>,
    nano_capsules: HashMap<i16, i16>,
    code_items: HashMap<String, Vec<Item>>,
}

struct PathData {
    skyway_paths: HashMap<i32, Path>,
    slider_path: Path,
    npc_paths: HashMap<i32, Path>,
}

pub struct TableData {
    xdt_data: XDTData,
    world_name_data: WorldNameDataContainer,
    npcs: Vec<NPCSpawnData>,
    drop_data: DropData,
    path_data: PathData,
    egg_data: EggData,
}
impl TableData {
    fn new() -> FFResult<Self> {
        Self::load().map_err(|e| {
            FFError::build(Severity::Fatal, format!("Failed loading TableData: {}", e))
        })
    }

    fn load() -> Result<Self, String> {
        Ok(Self {
            xdt_data: XDTData::load().map_err(|e| format!("Error loading XDT: {}", e))?,
            world_name_data: load_world_name_data()
                .map_err(|e| format!("Error loading world name data: {}", e))?,
            npcs: load_npcs().map_err(|e| format!("Error loading NPC data: {}", e))?,
            drop_data: load_drop_data().map_err(|e| format!("Error loading drop data: {}", e))?,
            path_data: load_path_data().map_err(|e| format!("Error loading path data: {}", e))?,
            egg_data: load_egg_data().map_err(|e| format!("Error loading egg data: {}", e))?,
        })
    }

    pub fn get_item_stats(&self, item_id: i16, item_type: ItemType) -> FFResult<&ItemStats> {
        self.xdt_data
            .item_data
            .get(&(item_id, item_type))
            .ok_or(FFError::build(
                Severity::Warning,
                format!(
                    "Item with ID {} and type {:?} doesn't exist",
                    item_id, item_type
                ),
            ))
    }

    pub fn get_egg_stats(&self, egg_type: i32) -> FFResult<&EggStats> {
        self.egg_data.egg_stats.get(&egg_type).ok_or(FFError::build(
            Severity::Warning,
            format!("Stats for egg type {} don't exist", egg_type),
        ))
    }

    pub fn get_vendor_data(&self, vendor_id: i32) -> FFResult<&VendorData> {
        self.xdt_data
            .vendor_data
            .get(&vendor_id)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Vendor with ID {} doesn't exist", vendor_id),
            ))
    }

    pub fn get_crocpot_data(&self, level_gap: i16) -> FFResult<&CrocPotData> {
        self.xdt_data
            .crocpot_data
            .get(&level_gap)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("CrocPotOdds for level gap {} don't exist", level_gap),
            ))
    }

    pub fn get_trip_data(&self, trip_id: i32) -> FFResult<&TripData> {
        self.xdt_data
            .transportation_data
            .trip_data
            .get(&trip_id)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Trip data for trip id {} doesn't exist", trip_id),
            ))
    }

    pub fn get_scamper_data(&self, location_id: i32) -> FFResult<&TransporterData> {
        self.xdt_data
            .transportation_data
            .scamper_data
            .get(&location_id)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Scamper data for location id {} doesn't exist", location_id),
            ))
    }

    pub fn get_skyway_data(&self, location_id: i32) -> FFResult<&TransporterData> {
        self.xdt_data
            .transportation_data
            .monkey_skyway_data
            .get(&location_id)
            .ok_or(FFError::build(
                Severity::Warning,
                format!(
                    "Monkey Skyway data for location id {} doesn't exist",
                    location_id
                ),
            ))
    }

    pub fn get_warp_data(&self, warp_id: i32) -> FFResult<&WarpData> {
        self.xdt_data
            .instance_data
            .warp_data
            .get(&warp_id)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Warp data for warp id {} doesn't exist", warp_id),
            ))
    }

    pub fn get_nano_stats(&self, nano_id: i16) -> FFResult<&NanoStats> {
        self.xdt_data
            .nano_data
            .nano_stats
            .get(&nano_id)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Nano stats for nano id {} doesn't exist", nano_id),
            ))
    }

    pub fn get_nano_tuning(&self, tuning_id: i16) -> FFResult<&NanoTuning> {
        self.xdt_data
            .nano_data
            .nano_tunings
            .get(&tuning_id)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Nano tuning with tuning id {} doesn't exist", tuning_id),
            ))
    }

    pub fn get_skill(&self, skill_id: i16) -> FFResult<&Skill> {
        self.xdt_data
            .skill_data
            .skills
            .get(&skill_id)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Skill with id {} doesn't exist", skill_id),
            ))
    }

    pub fn get_map_data(&self, map_num: u32) -> FFResult<&MapData> {
        self.xdt_data
            .instance_data
            .map_data
            .get(&map_num)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Map data for map num {} doesn't exist", map_num),
            ))
    }

    fn make_npcs_from_spawn_data(
        spawn_data: &NPCSpawnData,
        entity_map: &mut EntityMap,
        channel_num: u8,
    ) -> Vec<NPC> {
        let dat = spawn_data;
        let mut npcs = Vec::new();
        let npc = match NPC::new(
            entity_map.gen_next_npc_id(),
            dat.npc_type,
            Position {
                x: dat.pos.x,
                y: dat.pos.y,
                z: dat.pos.z,
            },
            dat.angle,
            InstanceID {
                channel_num,
                map_num: dat.map_num.unwrap_or(ID_OVERWORLD),
                instance_num: None,
            },
        ) {
            Ok(npc) => npc,
            Err(e) => {
                log(
                    e.get_severity(),
                    &format!("Failed to spawn NPC: {}", e.get_msg()),
                );
                return npcs;
            }
        };

        for follower_data in &dat.followers {
            let id = entity_map.gen_next_npc_id();
            let mut follower = match NPC::new(
                id,
                follower_data.npc_type,
                dat.pos + follower_data.offset,
                dat.angle,
                InstanceID {
                    channel_num,
                    map_num: dat.map_num.unwrap_or(ID_OVERWORLD),
                    instance_num: None,
                },
            ) {
                Ok(follower) => follower,
                Err(e) => {
                    log(
                        e.get_severity(),
                        &format!("Failed to spawn NPC follower: {}", e.get_msg()),
                    );
                    continue;
                }
            };

            follower.tight_follow = Some((EntityID::NPC(npc.id), follower_data.offset));
            npcs.push(follower);
        }
        npcs.push(npc);
        npcs
    }

    pub fn make_all_npcs(&self, entity_map: &mut EntityMap, channel_num: u8) -> Vec<NPC> {
        let mut npcs = Vec::new();
        for dat in &self.npcs {
            npcs.extend(Self::make_npcs_from_spawn_data(
                dat,
                entity_map,
                channel_num,
            ));
        }
        npcs
    }

    pub fn make_group_npcs(
        &self,
        entity_map: &mut EntityMap,
        channel_num: u8,
        group_id: i32,
    ) -> Vec<NPC> {
        let mut npcs = Vec::new();
        for dat in &self.npcs {
            // inefficient, but not worth having a separate data structure for
            if dat.group_id == Some(group_id) {
                npcs.extend(Self::make_npcs_from_spawn_data(
                    dat,
                    entity_map,
                    channel_num,
                ));
                break;
            }
        }
        npcs
    }

    pub fn make_eggs(&self, entity_map: &mut EntityMap, channel_num: u8) -> Vec<Egg> {
        let mut eggs = Vec::new();
        for dat in &self.egg_data.eggs {
            let egg = Egg::new(
                entity_map.gen_next_egg_id(),
                dat.egg_type,
                Position {
                    x: dat.pos.x,
                    y: dat.pos.y,
                    z: dat.pos.z,
                },
                InstanceID {
                    channel_num,
                    map_num: dat.map_num.unwrap_or(ID_OVERWORLD),
                    instance_num: None,
                },
                false,
            );
            eggs.push(egg);
        }
        eggs
    }

    pub fn get_nano_from_capsule(&self, crate_id: i16) -> Option<i16> {
        self.drop_data.nano_capsules.get(&crate_id).copied()
    }

    pub fn get_code_items(&self, code: &str) -> Option<&[Item]> {
        self.drop_data
            .code_items
            .get(&code.to_lowercase())
            .map(Vec::as_slice)
    }

    pub fn get_item_from_crate(&self, crate_id: i16, gender: i32) -> FFResult<Item> {
        let crate_data =
            self.drop_data
                .crate_data
                .get(&(crate_id as i32))
                .ok_or(FFError::build(
                    Severity::Warning,
                    format!("No C.R.A.T.E. data for id {}", crate_id),
                ))?;

        let item_set =
            self.drop_data
                .item_sets
                .get(&crate_data.ItemSetID)
                .ok_or(FFError::build(
                    Severity::Warning,
                    format!("No item set with id {}", crate_data.ItemSetID),
                ))?;

        let rarity_weights = self
            .drop_data
            .rarity_weights
            .get(&crate_data.RarityWeightID)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("No rarity data for id {}", crate_data.RarityWeightID),
            ))?;

        // generate a rarity from the rarity weights. rarities start at 1
        let mut available_rarities = Vec::new();

        // build a pool of eligible items
        let mut item_pool = Vec::new();
        for item_ref_id in &item_set.ItemReferenceIDs {
            let eligible: FFResult<bool> = (|| {
                let item_ref = self
                    .drop_data
                    .item_refs
                    .get(item_ref_id)
                    .ok_or(FFError::build(
                        Severity::Warning,
                        format!("No item ref with id {}", item_ref_id),
                    ))?;
                let item_stats = self
                    .get_item_stats(item_ref.ItemID as i16, (item_ref.Type as i16).try_into()?)?;
                let item_rarity = *item_set
                    .AlterRarityMap
                    .get(&item_ref_id.to_string())
                    .unwrap_or(&(item_stats.rarity.unwrap_or(0) as i32));
                let item_gender = *item_set
                    .AlterGenderMap
                    .get(&item_ref_id.to_string())
                    .unwrap_or(&(item_stats.gender.unwrap_or(0) as i32));

                // gender checks
                if item_gender != 0 && !item_set.IgnoreGender && gender != item_gender {
                    return Ok(false);
                }

                available_rarities.push(item_rarity);
                Ok(true)
            })();
            match eligible {
                Ok(eligible) => {
                    if eligible {
                        item_pool.push(*item_ref_id);
                    }
                }
                Err(e) => log(e.get_severity(), e.get_msg()),
            }
        }

        if item_pool.is_empty() {
            return Err(FFError::build(
                Severity::Warning,
                format!("Item pool was empty: id {}, gender {}", crate_id, gender),
            ));
        }

        // Only draw rarities represented by gender-compatible items, including overrides.
        let weights = eligible_rarity_weights(&rarity_weights.Weights, &available_rarities);
        let rarity = if weights.iter().any(|w| *w > 0) {
            (util::weighted_rand(&weights) + 1) as i32
        } else {
            1
        };
        item_pool = item_pool
            .into_iter()
            .zip(available_rarities)
            .filter_map(|(id, item_rarity)| {
                (item_set.IgnoreRarity || item_rarity == 0 || item_rarity == rarity).then_some(id)
            })
            .collect();
        if item_pool.is_empty() {
            return Err(FFError::build(
                Severity::Warning,
                format!("No eligible crate items for {}", crate_id),
            ));
        }

        // get the weights for each item
        let mut item_weights = vec![item_set.DefaultItemWeight; item_pool.len()];
        for (idx, item_ref_id) in item_pool.iter().enumerate() {
            let override_weight = item_set.AlterItemWeightMap.get(&item_ref_id.to_string());
            if let Some(weight) = override_weight {
                item_weights[idx] = *weight;
            }
        }

        // select an item
        let rolled_item_ref_id = item_pool[util::weighted_rand(&item_weights)];
        let rolled_item_ref = self.drop_data.item_refs.get(&rolled_item_ref_id).unwrap();

        Ok(Item::new(
            (rolled_item_ref.Type as i16).try_into()?,
            rolled_item_ref.ItemID as i16,
        ))
    }

    pub fn get_mob_reward(&self, mob_type: i32) -> FFResult<Reward> {
        let mapping = self
            .drop_data
            .mob_drop_data
            .get(&mob_type)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("No mob drop data for mob type {}", mob_type),
            ))?;

        let mut reward = self.roll_mob_drop(mapping.MobDropID)?;
        let event_id = config_get().shard.event_mode.get();
        if event_id != 0 {
            if let Some(event) = self.drop_data.events.get(&event_id) {
                match self.roll_mob_drop(event.MobDropID) {
                    Ok(extra) => {
                        reward.taros = reward.taros.saturating_add(extra.taros);
                        reward.fusion_matter = reward.fusion_matter.saturating_add(extra.fusion_matter);
                        reward.nano_potions = reward.nano_potions.saturating_add(extra.nano_potions);
                        reward.weapon_boosts = reward.weapon_boosts.saturating_add(extra.weapon_boosts);
                        reward.items.extend(extra.items);
                    }
                    Err(e) => log_error(e),
                }
            } else {
                log(
                    Severity::Warning,
                    &format!("Event {} has no mob drop assigned", event_id),
                );
            }
        }
        Ok(reward)
    }

    fn roll_mob_drop(&self, drop_id: i32) -> FFResult<Reward> {
        let mut rng = thread_rng();
        let mut reward = Reward::new(RewardCategory::Combat);
        let mob_drop = self
            .drop_data
            .mob_drops
            .get(&drop_id)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("No mob drop for mob drop id {}", drop_id),
            ))?;

        let apply_misc_drop = |rng: &mut ThreadRng, reward: &mut Reward| {
            let misc_drop_type = self
                .drop_data
                .misc_drop_types
                .get(&mob_drop.MiscDropTypeID)
                .ok_or(FFError::build(
                    Severity::Warning,
                    format!("No such misc drop type {}", mob_drop.MiscDropTypeID),
                ))?;
            let misc_drop_chance = self
                .drop_data
                .misc_drop_chances
                .get(&mob_drop.MiscDropChanceID)
                .ok_or(FFError::build(
                    Severity::Warning,
                    format!("No such misc drop chance {}", mob_drop.MiscDropChanceID),
                ))?;
            if rng.gen_range(0..misc_drop_chance.TaroDropChanceTotal)
                < misc_drop_chance.TaroDropChance
            {
                reward.taros = misc_drop_type.TaroAmount;
            }
            if rng.gen_range(0..misc_drop_chance.FMDropChanceTotal) < misc_drop_chance.FMDropChance
            {
                reward.fusion_matter = misc_drop_type.FMAmount;
            }
            if rng.gen_range(0..misc_drop_chance.PotionDropChanceTotal)
                < misc_drop_chance.PotionDropChance
            {
                reward.nano_potions = misc_drop_type.PotionAmount;
            }
            if rng.gen_range(0..misc_drop_chance.BoostDropChanceTotal)
                < misc_drop_chance.BoostDropChance
            {
                reward.weapon_boosts = misc_drop_type.BoostAmount;
            }
            Ok(())
        };

        let apply_crate_drop = |rng: &mut ThreadRng, reward: &mut Reward| {
            let crate_drop_type = self
                .drop_data
                .crate_drop_types
                .get(&mob_drop.CrateDropTypeID)
                .ok_or(FFError::build(
                    Severity::Warning,
                    format!("No such crate drop type {}", mob_drop.CrateDropTypeID),
                ))?;
            let crate_drop_chance = self
                .drop_data
                .crate_drop_chances
                .get(&mob_drop.CrateDropChanceID)
                .ok_or(FFError::build(
                    Severity::Warning,
                    format!("No such crate drop chance {}", mob_drop.CrateDropChanceID),
                ))?;
            if rng.gen_range(0..crate_drop_chance.DropChanceTotal) < crate_drop_chance.DropChance {
                let crate_id = crate_drop_type.CrateIDs
                    [util::weighted_rand(&crate_drop_chance.CrateTypeDropWeights)];
                let crate_item = Item::new(ItemType::Chest, crate_id as i16);
                reward.items.push(crate_item);
            }
            Ok(())
        };

        log_if_failed(apply_misc_drop(&mut rng, &mut reward));
        if let Err(e) = apply_crate_drop(&mut rng, &mut reward) {
            log_error(e);
            reward.items.push(util::get_random_gumball());
        }

        Ok(reward)
    }

    pub fn get_npc_path(&self, npc_type: i32) -> Option<Path> {
        self.path_data.npc_paths.get(&npc_type).cloned()
    }

    pub fn get_slider_path(&self) -> Path {
        self.path_data.slider_path.clone()
    }

    pub fn get_skyway_path(&self, path_id: i32) -> FFResult<Path> {
        self.path_data
            .skyway_paths
            .get(&path_id)
            .cloned()
            .ok_or(FFError::build(
                Severity::Warning,
                format!("No skyway path with id {}", path_id),
            ))
    }

    pub fn get_task_definition(&self, task_id: i32) -> FFResult<&TaskDefinition> {
        self.xdt_data
            .mission_data
            .task_definitions
            .get(&task_id)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Task with id {} doesn't exist", task_id),
            ))
    }

    pub fn get_mission_definition(&self, mission_id: i32) -> FFResult<&MissionDefinition> {
        self.xdt_data
            .mission_data
            .mission_definitions
            .get(&mission_id)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Mission with id {} doesn't exist", mission_id),
            ))
    }

    pub fn get_player_stats(&self, level: i16) -> FFResult<&PlayerStats> {
        self.xdt_data
            .player_data
            .get(&level)
            .map(|d| &d.stats)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Player stats for level {} don't exist", level),
            ))
    }

    pub fn get_npc_name(&self, npc_type: i32) -> FFResult<&str> {
        self.xdt_data
            .npc_data
            .get(&npc_type)
            .map(|d| d.name.as_str())
            .ok_or(FFError::build(
                Severity::Warning,
                format!("NPC name for type {} doesn't exist", npc_type),
            ))
    }

    pub fn get_npc_stats(&self, npc_type: i32) -> FFResult<&NPCStats> {
        self.xdt_data
            .npc_data
            .get(&npc_type)
            .map(|d| &d.stats)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("NPC stats for type {} don't exist", npc_type),
            ))
    }

    pub fn get_mission_reward(&self, reward_id: i32) -> FFResult<Reward> {
        self.xdt_data
            .mission_data
            .rewards
            .get(&reward_id)
            .cloned()
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Reward with id {} doesn't exist", reward_id),
            ))
    }

    pub fn get_nearest_respawn_point(&self, pos: Position, map_num: u32) -> Option<Position> {
        self.xdt_data
            .respawn_data
            .iter()
            .filter_map(|rd| {
                if rd.map_num == map_num {
                    Some(rd.pos)
                } else {
                    None
                }
            })
            .min_by_key(|p| {
                let dx = p.x - pos.x;
                let dy = p.y - pos.y;
                dx.abs() + dy.abs()
            })
    }

    pub fn get_world_name_data(&self, chunk: ChunkCoords) -> Option<&WorldNameData> {
        let map_num = self.world_name_data.lookup_grid.get(&(chunk.x, chunk.y))?;

        self.world_name_data.world_name_data.get(map_num)
    }
}

pub fn tdata_init() -> FFResult<&'static TableData> {
    if TABLE_DATA.get().is_none() {
        let load_start = SystemTime::now();
        let tdata = TableData::new()?;
        let load_time = load_start.elapsed().unwrap();

        let _ = TABLE_DATA.set(tdata);
        log(
            Severity::Info,
            &format!("Loaded TableData ({:.2}s)", load_time.as_secs_f32()),
        );
    }

    Ok(tdata_get())
}

pub fn tdata_get() -> &'static TableData {
    TABLE_DATA.get().expect("TableData not initialized")
}

fn load_json(filename: &str) -> Result<Map<String, Value>, String> {
    let config = &config_get().general;
    let tdata_path = config.table_data_path.get();
    let path = std::path::Path::new(&tdata_path).join(filename);

    let file = std::fs::read_to_string(path.clone())
        .map_err(|e| format!("Couldn't read file {:?}: {}", path, e))?;
    let mut json = serde_json::from_str(&file)
        .map_err(|e| format!("Couldn't parse {:?} as JSON: {}", path, e))?;

    let mut patches = match config.protocol.get() {
        FFProtocol::v0104 => vec!["0104-fixes"],
        FFProtocol::v1013 => vec!["1013", "1013-fixes"],
    }
    .into_iter()
    .map(|s| s.to_string())
    .collect::<Vec<String>>();

    patches.extend(config.enabled_patches.get());

    for patch in &patches {
        let patch_path = std::path::Path::new(&tdata_path)
            .join("patch/")
            .join(patch)
            .join(filename);

        if let Ok(file) = std::fs::read_to_string(&patch_path) {
            if let Ok(patch_json) = serde_json::from_str(&file) {
                if let Err(e) = apply_patch(&mut json, patch_json) {
                    return Err(format!("Failed to apply patch {:?}: {}", patch_path, e));
                } else {
                    log(
                        Severity::Info,
                        &format!("Applied tabledata patch {} to {}", patch, filename),
                    );
                }
            }
        }
    }

    let Value::Object(root) = json else {
        return Err(format!("Malformed {:?}", path));
    };

    Ok(root)
}

fn apply_patch(root: &mut Value, patch: Value) -> Result<(), String> {
    match (root, patch) {
        (Value::Bool(root_bool), Value::Bool(patch_bool)) => {
            *root_bool = patch_bool;
        }
        (Value::String(root_str), Value::String(patch_str)) => {
            *root_str = patch_str;
        }
        (Value::Number(root_num), Value::Number(patch_num)) => {
            let types_match = (root_num.is_i64() && patch_num.is_i64())
                || (root_num.is_u64() && patch_num.is_u64())
                || (root_num.is_f64() && patch_num.is_f64());

            if !types_match {
                return Err("Number types do not match".to_string());
            }

            *root_num = patch_num;
        }
        (Value::Array(root_arr), Value::Array(patch_arr)) => {
            root_arr.extend(patch_arr);
        }
        (Value::Object(root_obj), Value::Object(patch_obj)) => {
            for (k, v) in patch_obj {
                if let Some(key) = k.strip_prefix('!') {
                    // Forced replacement
                    root_obj.insert(key.to_string(), v);
                } else if let Value::Null = v {
                    // Removal
                    root_obj.remove(&k);
                } else if let Some(existing) = root_obj.get_mut(&k) {
                    // Recursive patch application
                    apply_patch(existing, v)?;
                } else {
                    // New key-value pair
                    root_obj.insert(k, v);
                }
            }
        }
        _ => return Err("Invalid patch".to_string()),
    }
    Ok(())
}

fn get_object<'a>(
    root: &'a Map<String, Value>,
    key: &'static str,
) -> Result<&'a Map<String, Value>, String> {
    root.get(key)
        .ok_or(format!("Key missing: {}", key))
        .and_then(|v| {
            if let Value::Object(v) = v {
                Ok(v)
            } else {
                Err(format!("Value is not an object: {}", key))
            }
        })
}

fn get_array<'a>(
    root: &'a Map<String, Value>,
    key: &'static str,
) -> Result<&'a Vec<Value>, String> {
    root.get(key)
        .ok_or(format!("Key missing: {}", key))
        .and_then(|v| {
            if let Value::Array(v) = v {
                Ok(v)
            } else {
                Err(format!("Value is not an array: {}", key))
            }
        })
}

fn load_item_data(
    root: &Map<String, Value>,
) -> Result<HashMap<(i16, ItemType), ItemStats>, String> {
    const ITEM_TABLE_ITEM_DATA_KEY: &str = "m_pItemData";

    fn load_item_data_for_type(
        root: &Map<String, Value>,
        map: &mut HashMap<(i16, ItemType), ItemStats>,
        item_type: ItemType,
    ) -> Result<(), String> {
        #[derive(Deserialize)]
        struct ItemDataEntry {
            m_iItemNumber: i32,
            m_iItemName: Option<i32>,
            m_iComment: Option<i32>,
            m_iTradeAble: i32,
            m_iItemPrice: i32,
            m_iItemSellPrice: i32,
            m_iSellAble: i32,
            m_iStackNumber: i16,
            m_iIcon: Option<i32>,
            m_fStyleMod_TrumpMonster: Option<f32>,
            m_fStyleMod_Trumped: Option<f32>,
            m_iEquipLoc: Option<i32>,
            m_iEquipType: Option<i32>,
            m_ibattery: Option<i32>,
            m_iBatteryDrain: Option<i32>,
            m_iMinReqLev: Option<i32>,
            m_iMentor: Option<i32>,
            m_iAtkRange: Option<i32>,
            m_iAtkAngle: Option<i32>,
            m_iAtkRate: Option<i32>,
            m_iEffectArea: Option<i32>,
            m_iTargetMode: Option<i32>,
            m_iTargetNumber: Option<i32>,
            m_iInitalTime: Option<i32>,
            m_iDeliverTime: Option<i32>,
            m_iDelayTime: Option<i32>,
            m_iDurationTime: Option<i32>,
            m_iUp_power: Option<i32>,
            m_iUp_accuracy: Option<i32>,
            m_iUp_protection: Option<i32>,
            m_iUp_dodge: Option<i32>,
            m_iUp_runSpeed: Option<i32>,
            m_iUp_swimSpeed: Option<i32>,
            m_iUp_jumpHeight: Option<i32>,
            m_iUp_jumpDistance: Option<i32>,
            m_iUp_atkRate: Option<i32>,
            m_iUp_effectArea: Option<i32>,
            m_iUp_addFusionMatter: Option<i32>,
            m_iUp_addCandy: Option<i32>,
            m_iUp_addItemfind: Option<i32>,
            m_iMesh: Option<i32>,
            m_iTexture: Option<i32>,
            m_iTexture2: Option<i32>,
            m_iEffect1: Option<i32>,
            m_iSound1: Option<i32>,
            m_iReqSex: Option<i32>,
            m_iRarity: Option<i32>,
            m_iPointRat: Option<i32>,
            m_iGroupRat: Option<i32>,
            m_iDefenseRat: Option<i32>,
            m_iEffect2: Option<i32>,
            m_iSound2: Option<i32>,
            m_iCashAble: Option<i32>,
        }

        let table_key = match item_type {
            ItemType::Hand => "m_pWeaponItemTable",
            ItemType::UpperBody => "m_pShirtsItemTable",
            ItemType::LowerBody => "m_pPantsItemTable",
            ItemType::Foot => "m_pShoesItemTable",
            ItemType::Head => "m_pHatItemTable",
            ItemType::Face => "m_pGlassItemTable",
            ItemType::Back => "m_pBackItemTable",
            ItemType::General => "m_pGeneralItemTable",
            ItemType::Quest => "m_pQuestItemTable",
            ItemType::Chest => "m_pChestItemTable",
            ItemType::Vehicle => "m_pVehicleItemTable",
            _ => unimplemented!(),
        };

        let item_table = get_object(root, table_key)?;
        let item_data = get_array(item_table, ITEM_TABLE_ITEM_DATA_KEY)?;
        for i in item_data {
            let data: ItemDataEntry = serde_json::from_value(i.clone())
                .map_err(|e| format!("Malformed item data entry ({:?}): {} {}", item_type, e, i))?;
            let key = (data.m_iItemNumber as i16, item_type);
            let data = ItemStats {
                buy_price: data.m_iItemPrice as u32,
                sell_price: data.m_iItemSellPrice as u32,
                sellable: data.m_iSellAble != 0,
                tradeable: data.m_iTradeAble != 0,
                max_stack_size: data.m_iStackNumber as u16,
                required_level: data.m_iMinReqLev.unwrap_or(0) as i16,
                rarity: data.m_iRarity.map(|v| v as i8),
                gender: data.m_iReqSex.map(|v| v as i8),
                single_power: data.m_iPointRat,
                multi_power: data.m_iGroupRat,
                defense: data.m_iDefenseRat,
                speed: data.m_iUp_runSpeed,
            };
            map.insert(key, data);
        }
        Ok(())
    }

    let mut map = HashMap::new();
    load_item_data_for_type(root, &mut map, ItemType::Hand)?;
    load_item_data_for_type(root, &mut map, ItemType::UpperBody)?;
    load_item_data_for_type(root, &mut map, ItemType::LowerBody)?;
    load_item_data_for_type(root, &mut map, ItemType::Foot)?;
    load_item_data_for_type(root, &mut map, ItemType::Head)?;
    load_item_data_for_type(root, &mut map, ItemType::Face)?;
    load_item_data_for_type(root, &mut map, ItemType::Back)?;
    load_item_data_for_type(root, &mut map, ItemType::Vehicle)?;
    load_item_data_for_type(root, &mut map, ItemType::General)?;
    load_item_data_for_type(root, &mut map, ItemType::Chest)?;
    Ok(map)
}

fn load_vendor_data(root: &Map<String, Value>) -> Result<HashMap<i32, VendorData>, String> {
    const VENDOR_TABLE_KEY: &str = "m_pVendorTable";
    const VENDOR_TABLE_ITEM_DATA_KEY: &str = "m_pItemData";

    #[derive(Deserialize)]
    struct VendorDataEntry {
        m_iNpcNumber: i32,
        m_iSortNumber: i32,
        m_iItemType: i16,
        m_iitemID: i16,
        m_iSellCost: i32,
    }

    let vendor_table = get_object(root, VENDOR_TABLE_KEY)?;
    let item_data = get_array(vendor_table, VENDOR_TABLE_ITEM_DATA_KEY)?;

    let mut vendor_data = HashMap::new();
    for v in item_data {
        let vendor_data_entry: VendorDataEntry = serde_json::from_value(v.clone())
            .map_err(|e| format!("Malformed vendor data entry: {} {}", e, v))?;
        let key = vendor_data_entry.m_iNpcNumber;
        let vendor_data_entry = VendorItem {
            sort_number: vendor_data_entry.m_iSortNumber,
            ty: vendor_data_entry
                .m_iItemType
                .try_into()
                .map_err(|e: FFError| e.get_msg().to_string())?,
            id: vendor_data_entry.m_iitemID,
        };

        vendor_data
            .entry(key)
            .or_insert_with(|| VendorData::new(key))
            .insert(vendor_data_entry);
    }
    Ok(vendor_data)
}

fn load_crocpot_data(root: &Map<String, Value>) -> Result<HashMap<i16, CrocPotData>, String> {
    const CROCPOT_TABLE_KEY: &str = "m_pCombiningTable";
    const CROCPOT_TABLE_CROCPOT_DATA_KEY: &str = "m_pCombiningData";

    #[derive(Deserialize)]
    struct CrocPotDataEntry {
        m_iLevelGap: i32,
        m_fSameGrade: f32,
        m_fOneGrade: f32,
        m_fTwoGrade: f32,
        m_fThreeGrade: f32,
        m_fLevelGapStandard: f32,
        m_iLookConstant: i32,
        m_iStatConstant: i32,
    }

    let crocpot_table = get_object(root, CROCPOT_TABLE_KEY)?;
    let crocpot_data = get_array(crocpot_table, CROCPOT_TABLE_CROCPOT_DATA_KEY)?;
    let mut crocpot_table = HashMap::new();
    for v in crocpot_data {
        let crocpot_data_entry: CrocPotDataEntry = serde_json::from_value(v.clone())
            .map_err(|e| format!("Malformed crocpot data entry: {} {}", e, v))?;
        let key = crocpot_data_entry.m_iLevelGap as i16;
        let crocpot_odds = CrocPotData {
            base_chance: crocpot_data_entry.m_fLevelGapStandard / 100.0,
            rarity_diff_multipliers: [
                crocpot_data_entry.m_fSameGrade / 100.0,
                crocpot_data_entry.m_fOneGrade / 100.0,
                crocpot_data_entry.m_fTwoGrade / 100.0,
                crocpot_data_entry.m_fThreeGrade / 100.0,
            ],
            price_multiplier_looks: crocpot_data_entry.m_iLookConstant as u32,
            price_multiplier_stats: crocpot_data_entry.m_iStatConstant as u32,
        };
        crocpot_table.insert(key, crocpot_odds);
    }
    Ok(crocpot_table)
}

fn load_transportation_data(root: &Map<String, Value>) -> Result<TransportationData, String> {
    const TRANSPORTATION_TABLE_KEY: &str = "m_pTransportationTable";

    fn load_trip_data(table: &Map<String, Value>) -> Result<HashMap<i32, TripData>, String> {
        const TRIP_DATA_KEY: &str = "m_pTransportationData";

        #[derive(Debug, Deserialize)]
        struct TripDataEntry {
            m_iVehicleID: i32,
            m_iNPCID: i32,
            m_iLocalString: i32,
            m_iMoveType: i32,
            m_iStartLocation: i32,
            m_iEndLocation: i32,
            m_iCost: i32,
            m_iSpeed: i32,
            m_iMesh: i32,
            m_iSound: i32,
            m_iRouteNum: i32,
        }

        let data = get_array(table, TRIP_DATA_KEY)?;
        let mut trip_map = HashMap::new();
        for v in data {
            let trip_entry: TripDataEntry = serde_json::from_value(v.clone())
                .map_err(|e| format!("Malformed trip data entry: {} {}", e, v))?;
            let key = trip_entry.m_iVehicleID;
            if key == 0 {
                continue;
            }
            let trip_entry = TripData {
                npc_id: trip_entry.m_iNPCID,
                start_location: trip_entry.m_iStartLocation,
                end_location: trip_entry.m_iEndLocation,
                cost: trip_entry.m_iCost as u32,
                speed: trip_entry.m_iSpeed,
                route_number: trip_entry.m_iRouteNum,
                transportation_type: trip_entry
                    .m_iMoveType
                    .try_into()
                    .map_err(|e: FFError| e.get_msg().to_string())?,
            };
            trip_map.insert(key, trip_entry);
        }
        Ok(trip_map)
    }

    fn load_transporter_data(
        table: &Map<String, Value>,
        data_key: &'static str,
    ) -> Result<HashMap<i32, TransporterData>, String> {
        #[derive(Debug, Deserialize)]
        struct TransporterDataEntry {
            m_iLocationID: i32,
            m_iNPCID: i32,
            m_iXpos: i32,
            m_iYpos: i32,
            m_iZpos: i32,
            m_iZone: i32,
        }

        let data = get_array(table, data_key)?;
        let mut scamper_map = HashMap::new();
        for v in data {
            let data_entry: TransporterDataEntry = serde_json::from_value(v.clone())
                .map_err(|e| format!("Malformed transporter data entry: {} {}", e, v))?;
            let key = data_entry.m_iLocationID;
            let data_entry = TransporterData {
                npc_type: data_entry.m_iNPCID,
                pos: Position {
                    x: data_entry.m_iXpos,
                    y: data_entry.m_iYpos,
                    z: data_entry.m_iZpos,
                },
            };
            scamper_map.insert(key, data_entry);
        }
        Ok(scamper_map)
    }

    let table = get_object(root, TRANSPORTATION_TABLE_KEY)?;
    Ok(TransportationData {
        trip_data: load_trip_data(table)?,
        scamper_data: load_transporter_data(table, "m_pTransportationWarpLocation")?,
        monkey_skyway_data: load_transporter_data(table, "m_pBroomstickLocation")?,
    })
}

fn load_instance_data(root: &Map<String, Value>) -> Result<InstanceData, String> {
    const INSTANCE_TABLE_KEY: &str = "m_pInstanceTable";

    fn load_warp_data(table: &Map<String, Value>) -> Result<HashMap<i32, WarpData>, String> {
        const WARP_DATA_KEY: &str = "m_pWarpData";

        #[derive(Debug, Deserialize)]
        struct WarpDataEntry {
            m_iWarpNumber: i32,
            m_iWarpGroupType: i32,
            m_iNpcNumber: i32,
            m_iIsInstance: i32,
            m_iToMapNum: i32,
            m_iToX: i32,
            m_iToY: i32,
            m_iToZ: i32,
            m_iLimit_Level: i32,
            m_iLimit_TaskID: i32,
            m_iMissionID: i32,
            m_iLimit_ItemType: i32,
            m_iLimit_ItemID: i32,
            m_iLimit_UseItemType: i32,
            m_iLimit_UseItemID: i32,
            m_iCost: i32,
        }

        let data = get_array(table, WARP_DATA_KEY)?;
        let mut warp_map = HashMap::new();
        for v in data {
            let warp_data_entry: WarpDataEntry = serde_json::from_value(v.clone())
                .map_err(|e| format!("Malformed warp data entry: {} {}", e, v))?;
            let key = warp_data_entry.m_iWarpNumber;
            let data_entry = WarpData {
                pos: Position {
                    x: warp_data_entry.m_iToX,
                    y: warp_data_entry.m_iToY,
                    z: warp_data_entry.m_iToZ,
                },
                npc_type: warp_data_entry.m_iNpcNumber,
                is_instance: warp_data_entry.m_iIsInstance != 0,
                is_group_warp: warp_data_entry.m_iWarpGroupType != 0,
                map_num: warp_data_entry.m_iToMapNum as u32,
                min_level: warp_data_entry.m_iLimit_Level as i16,
                req_task: if warp_data_entry.m_iMissionID == 0 {
                    None
                } else {
                    Some((
                        warp_data_entry.m_iMissionID,
                        warp_data_entry.m_iLimit_TaskID,
                    ))
                },
                req_item: if warp_data_entry.m_iLimit_ItemID == 0 {
                    None
                } else {
                    Some((
                        (warp_data_entry.m_iLimit_ItemType as i16)
                            .try_into()
                            .map_err(|e: FFError| e.get_msg().to_string())?,
                        warp_data_entry.m_iLimit_ItemID as i16,
                    ))
                },
                req_item_consumed: if warp_data_entry.m_iLimit_UseItemID == 0 {
                    None
                } else {
                    Some((
                        (warp_data_entry.m_iLimit_UseItemType as i16)
                            .try_into()
                            .map_err(|e: FFError| e.get_msg().to_string())?,
                        warp_data_entry.m_iLimit_UseItemID as i16,
                    ))
                },
                cost: warp_data_entry.m_iCost as u32,
            };
            warp_map.insert(key, data_entry);
        }
        Ok(warp_map)
    }

    fn load_map_data(table: &Map<String, Value>) -> Result<HashMap<u32, MapData>, String> {
        const INSTANCE_DATA_KEY: &str = "m_pInstanceData";

        #[derive(Debug, Deserialize)]
        struct MapDataEntry {
            m_iInstanceNameID: u32,
            m_iIsEP: u32,
            m_iZoneX: i32,
            m_iZoneY: i32,
        }

        let data = get_array(table, INSTANCE_DATA_KEY)?;
        let mut map_map = HashMap::new();
        for v in data {
            let map_data_entry: MapDataEntry = serde_json::from_value(v.clone())
                .map_err(|e| format!("Malformed map data entry: {} {}", e, v))?;
            let key = map_data_entry.m_iInstanceNameID;
            let map_data_entry = MapData {
                ep_id: if map_data_entry.m_iIsEP == 0 {
                    None
                } else {
                    Some(map_data_entry.m_iIsEP as u32)
                },
                map_square: (map_data_entry.m_iZoneX, map_data_entry.m_iZoneY),
            };
            map_map.insert(key, map_data_entry);
        }
        Ok(map_map)
    }

    let table = get_object(root, INSTANCE_TABLE_KEY)?;
    Ok(InstanceData {
        warp_data: load_warp_data(table)?,
        map_data: load_map_data(table)?,
    })
}

fn load_nano_data(root: &Map<String, Value>) -> Result<NanoData, String> {
    const NANO_TABLE_KEY: &str = "m_pNanoTable";

    fn load_stats(table: &Map<String, Value>) -> Result<HashMap<i16, NanoStats>, String> {
        const NANO_TABLE_NANO_DATA_KEY: &str = "m_pNanoData";

        #[derive(Debug, Deserialize)]
        struct NanoStatsEntry {
            m_iNanoNumber: i32,
            m_iNanoName: i32,
            m_iComment: i32,
            m_iNanoBattery1: i32,
            m_iNanoBattery2: i32,
            m_iNanoBattery3: i32,
            m_iNanoDrain: i32,
            m_iBatteryRecharge: i32,
            m_iStyle: i32,
            m_iNanoSet: i32,
            m_iPower: i32,
            m_iAccuracy: i32,
            m_iProtection: i32,
            m_iDodge: i32,
            m_iNeedQItemID: i32,
            m_iNeedFusionMatterCnt: i32,
            m_iTune: [i16; 3],
            m_iMesh: i32,
            m_iIcon1: i32,
            m_iEffect1: i32,
            m_iSound: i32,
        }

        let nano_data = get_array(table, NANO_TABLE_NANO_DATA_KEY)?;
        let mut nano_table = HashMap::new();
        for v in nano_data {
            let nano_data_entry: NanoStatsEntry = serde_json::from_value(v.clone())
                .map_err(|e| format!("Malformed nano data entry: {} {}", e, v))?;
            let key = nano_data_entry.m_iNanoNumber as i16;
            if key == 0 {
                continue;
            }
            let nano_data_entry = NanoStats {
                style: nano_data_entry
                    .m_iStyle
                    .try_into()
                    .map_err(|e: FFError| e.get_msg().to_string())?,
                skills: nano_data_entry.m_iTune,
            };
            nano_table.insert(key, nano_data_entry);
        }
        Ok(nano_table)
    }

    pub fn load_tunings(table: &Map<String, Value>) -> Result<HashMap<i16, NanoTuning>, String> {
        const NANO_TABLE_NANO_TUNE_DATA_KEY: &str = "m_pNanoTuneData";

        #[derive(Debug, Deserialize)]
        struct NanoTuningEntry {
            m_iTuneNumber: i32,
            m_iReqFusionMatter: i32,
            m_iReqItemID: i32,
            m_iReqItemCount: i32,
            m_iSkillID: i32,
        }

        let nano_tuning = get_array(table, NANO_TABLE_NANO_TUNE_DATA_KEY)?;
        let mut nano_tuning_table = HashMap::new();
        for v in nano_tuning {
            let nano_tuning_entry: NanoTuningEntry = serde_json::from_value(v.clone())
                .map_err(|e| format!("Malformed nano tuning entry: {} {}", e, v))?;
            let key = nano_tuning_entry.m_iTuneNumber as i16;
            if key == 0 {
                continue;
            }
            let nano_tuning_entry = NanoTuning {
                fusion_matter_cost: nano_tuning_entry.m_iReqFusionMatter as u32,
                req_item_id: nano_tuning_entry.m_iReqItemID as i16,
                req_item_quantity: nano_tuning_entry.m_iReqItemCount as u16,
                skill_id: nano_tuning_entry.m_iSkillID as i16,
            };
            nano_tuning_table.insert(key, nano_tuning_entry);
        }
        Ok(nano_tuning_table)
    }

    let table = get_object(root, NANO_TABLE_KEY)?;
    Ok(NanoData {
        nano_stats: load_stats(table)?,
        nano_tunings: load_tunings(table)?,
    })
}

fn load_skill_data(root: &Map<String, Value>) -> Result<SkillData, String> {
    const SKILL_TABLE_KEY: &str = "m_pSkillTable";
    const SKILL_TABLE_SKILL_DATA_KEY: &str = "m_pSkillData";

    #[derive(Debug, Deserialize)]
    struct SkillDataEntry {
        m_iSkillNumber: i32,
        m_iSkillType: i32,
        m_iEffectTarget: i32,
        m_iEffectType: i32,
        m_iTargetType: i32,
        m_iValueA_Type: i32,
        m_iValueA: [i32; 4],
        m_iValueB_Type: i32,
        m_iValueB: [i32; 4],
        m_iValueC_Type: i32,
        m_iValueC: [i32; 4],
        m_iEffectRange: i32,
        m_iEffectAngle: i32,
        m_iEffectArea: i32,
        m_iCoolTime: i32,
        m_iTargetNumber: i32,
        m_iBatteryDrainType: i32,
        m_iBatteryDrainUse: [i16; 4],
        m_iInitialTime: i32,
        m_iDeleverTime: i32,
        m_iDelayTime: i32,
        m_iDurationTime: [i32; 4],
        m_iDBType: i32,
        m_iIcon: i32,
        m_iEffect: i32,
        m_iTargetEffect: i32,
        m_iBuffEffect: i32,
        m_iSound: i32,
        m_iCoolType: i32,
    }

    let table = get_object(root, SKILL_TABLE_KEY)?;
    let skill_data = get_array(table, SKILL_TABLE_SKILL_DATA_KEY)?;
    let mut skill_table = HashMap::new();
    for v in skill_data {
        let skill_data_entry: SkillDataEntry = serde_json::from_value(v.clone())
            .map_err(|e| format!("Malformed skill data entry: {} {}", e, v))?;

        let key = skill_data_entry.m_iSkillNumber as i16;
        if key == 0 {
            continue;
        }

        let Ok(skill_type) = skill_data_entry.m_iSkillType.try_into() else {
            continue;
        };

        let Ok(targeting_type) = skill_data_entry.m_iEffectTarget.try_into() else {
            continue;
        };

        let Ok(target_type) = skill_data_entry.m_iTargetType.try_into() else {
            continue;
        };

        let skill = Skill {
            skill_type,
            targeting_type,
            target_type,
            passive: skill_data_entry.m_iBatteryDrainType == SKILL_DRAIN_TYPE_PASSIVE,
            range: skill_data_entry.m_iEffectArea as u32,
            values_a: skill_data_entry.m_iValueA,
            values_b: skill_data_entry.m_iValueB.map(|v| (v != 0).then_some(v)),
            values_c: skill_data_entry.m_iValueC.map(|v| (v != 0).then_some(v)),
            costs: skill_data_entry.m_iBatteryDrainUse,
            durations: skill_data_entry
                .m_iDurationTime
                .map(|v| (v != 0).then_some(Duration::from_millis(v as u64 * 100))),
        };
        skill_table.insert(key, skill);
    }
    Ok(SkillData {
        skills: skill_table,
    })
}

fn load_mission_data(root: &Map<String, Value>) -> Result<MissionData, String> {
    const MISSION_TABLE_KEY: &str = "m_pMissionTable";
    const MISSION_TABLE_MISSION_DATA_KEY: &str = "m_pMissionData";
    const MISSION_TABLE_MISSION_STRINGS_KEY: &str = "m_pMissionStringData";
    const MISSION_TABLE_REWARD_DATA_KEY: &str = "m_pRewardData";

    #[derive(Debug, Deserialize)]
    struct MissionDataEntry {
        m_iHTaskID: i32,
        m_iHMissionID: i32,
        m_iHMissionName: usize,
        m_iHNPCID: i32,
        m_iHMissionType: i32,
        m_iHTaskType: i32,
        m_iSUOutgoingTask: i32,
        m_iSUItem: [i16; MAX_NEED_SORT_OF_ITEM as usize],
        m_iSUInstancename: [isize; MAX_NEED_SORT_OF_ITEM as usize],
        m_iSUReward: i32,
        m_iFOutgoingTask: i32,
        m_iFItemID: [i16; MAX_NEED_SORT_OF_ITEM as usize],
        m_iFItemNumNeeded: [isize; MAX_NEED_SORT_OF_ITEM as usize],
        m_iCSTReqMission: [i32; MAX_REQUIRE_MISSION as usize],
        m_iCSTRReqNano: [i16; MAX_REQUIRE_NANO as usize],
        m_iCTRReqLvMin: i16,
        m_iCSTReqGuide: i16,
        m_iCSUDEFNPCID: i32,
        m_iCSUCheckTimer: u64,
        m_iHTerminatorNPCID: i32,
        m_iRequireInstanceID: u32,
        m_iCSUItemID: [i16; MAX_NEED_SORT_OF_ITEM as usize],
        m_iCSUItemNumNeeded: [usize; MAX_NEED_SORT_OF_ITEM as usize],
        m_iCSUEnemyID: [i32; MAX_NEED_SORT_OF_ENEMY as usize],
        m_iCSUNumToKill: [usize; MAX_NEED_SORT_OF_ENEMY as usize],
        m_iSTItemID: [i16; MAX_NEED_SORT_OF_ITEM as usize],
        m_iSTItemNumNeeded: [isize; MAX_NEED_SORT_OF_ITEM as usize],
        m_iSTItemDropRate: [i16; MAX_NEED_SORT_OF_ITEM as usize],
        m_iSTNanoID: i16,
        m_iDelItemID: [i16; 4],
        m_iHBarkerTextID: [i32; 4],
    }

    #[derive(Debug, Deserialize)]
    struct MissionStringEntry {
        m_pstrNameString: String,
    }

    #[derive(Debug, Deserialize)]
    struct MissionRewardEntry {
        m_iMissionRewardID: i32,
        m_iCash: u32,
        m_iFusionMatter: u32,
        m_iMissionRewardItemID: Vec<i16>,
        m_iMissionRewarItemType: Vec<i16>,
    }

    let table = get_object(root, MISSION_TABLE_KEY)?;
    let mission_data = get_array(table, MISSION_TABLE_MISSION_DATA_KEY)?;
    let mission_strings = get_array(table, MISSION_TABLE_MISSION_STRINGS_KEY)?;
    let reward_data = get_array(table, MISSION_TABLE_REWARD_DATA_KEY)?;

    let mut mission_defs = HashMap::new();
    let mut task_defs = HashMap::new();
    for v in mission_data.iter().skip(1) {
        let entry: MissionDataEntry = serde_json::from_value(v.clone())
            .map_err(|e| format!("Malformed mission data entry: {} {}", e, v))?;
        let task_id = entry.m_iHTaskID;
        let mission_id = entry.m_iHMissionID;
        let mission_type: MissionType = entry
            .m_iHMissionType
            .try_into()
            .map_err(|e: FFError| e.get_msg().to_string())?;
        let mission_name = match mission_strings.get(entry.m_iHMissionName) {
            Some(val) => {
                let mission_string_entry: MissionStringEntry = serde_json::from_value(val.clone())
                    .map_err(|e| format!("Malformed mission string entry: {} {}", e, val))?;
                mission_string_entry.m_pstrNameString
            }
            _ => format!("Mission #{}", mission_id),
        };

        // create mission entry if this is the first time we've seen it
        mission_defs
            .entry(mission_id)
            .or_insert_with(|| MissionDefinition {
                mission_id,
                mission_name,
                first_task_id: task_id,
                mission_type,
            });

        // create task definition
        let task_def = TaskDefinition {
            task_id,
            mission_id,
            task_type: entry
                .m_iHTaskType
                .try_into()
                .map_err(|e: FFError| e.get_msg().to_string())?,
            succ_task_id: match entry.m_iSUOutgoingTask {
                0 => None,
                x => Some(x),
            },
            fail_task_id: match entry.m_iFOutgoingTask {
                0 => None,
                x => Some(x),
            },
            prereq_completed_mission_ids: entry
                .m_iCSTReqMission
                .iter()
                .flat_map(|id| match id {
                    0 => None,
                    x => Some(*x),
                })
                .collect(),
            prereq_nano_ids: entry
                .m_iCSTRReqNano
                .iter()
                .flat_map(|id| match id {
                    0 => None,
                    x => Some(*x),
                })
                .collect(),
            prereq_level: match entry.m_iCTRReqLvMin {
                0 => None,
                x => Some(x),
            },
            prereq_guide: match entry.m_iCSTReqGuide {
                0 => None,
                x => Some(x.try_into().map_err(|e: FFError| e.get_msg().to_string())?),
            },
            obj_time_limit: match entry.m_iCSUCheckTimer {
                0 => None,
                x => Some(Duration::from_secs(x)),
            },
            obj_npc_type: match entry.m_iHTerminatorNPCID {
                0 => None,
                x => Some(x),
            },
            prereq_map_num: match entry.m_iRequireInstanceID {
                0 => None,
                x => Some(x),
            },
            obj_escort_npc_type: match entry.m_iCSUDEFNPCID {
                0 => None,
                x => Some(x),
            },
            prereq_npc_type: match entry.m_iHNPCID {
                0 => None,
                x => Some(x),
            },
            obj_qitems: entry
                .m_iCSUItemID
                .iter()
                .zip(entry.m_iCSUItemNumNeeded.iter())
                .flat_map(|(id, num)| match id {
                    0 => None,
                    x => Some((*x, *num)),
                })
                .collect(),
            obj_enemies: entry
                .m_iCSUEnemyID
                .iter()
                .zip(entry.m_iCSUNumToKill.iter())
                .flat_map(|(id, num)| match id {
                    0 => None,
                    x => Some((*x, *num)),
                })
                .collect(),
            obj_enemy_id_ordering: entry
                .m_iCSUEnemyID
                .iter()
                .flat_map(|id| match id {
                    0 => None,
                    x => Some(*x),
                })
                .collect(),
            fail_qitems: entry
                .m_iFItemID
                .iter()
                .zip(entry.m_iFItemNumNeeded.iter())
                .flat_map(|(id, num)| match id {
                    0 => None,
                    x => Some((*x, *num)),
                })
                .collect(),
            succ_qitems: entry
                .m_iSUItem
                .iter()
                .zip(entry.m_iSUInstancename.iter())
                .flat_map(|(id, num)| match id {
                    0 => None,
                    x => Some((*x, *num)),
                })
                .collect(),
            succ_reward: match entry.m_iSUReward {
                0 => None,
                x => Some(x),
            },
            succ_nano_id: match entry.m_iSTNanoID {
                0 => None,
                x => Some(x),
            },
            given_qitems: entry
                .m_iSTItemID
                .iter()
                .zip(entry.m_iSTItemNumNeeded.iter())
                .flat_map(|(id, num)| {
                    if *id != 0 && *num != 0 {
                        Some((*id, *num))
                    } else {
                        None
                    }
                })
                .collect(),
            dropped_qitems: entry
                .m_iSTItemID
                .iter()
                .zip(entry.m_iSTItemDropRate.iter())
                .flat_map(|(id, rate)| {
                    if *id != 0 && *rate != 0 {
                        let drop_rate = *rate as f32 / 100.0;
                        Some((*id, drop_rate))
                    } else {
                        None
                    }
                })
                .collect(),
            delete_qitems: entry
                .m_iDelItemID
                .iter()
                .flat_map(|id| match id {
                    0 => None,
                    x => Some(*x),
                })
                .collect(),
            barks: entry
                .m_iHBarkerTextID
                .iter()
                .flat_map(|id| if *id != 0 { Some(*id) } else { None })
                .collect(),
        };
        task_defs.insert(task_id, task_def);
    }

    let mut rewards = HashMap::new();
    for v in reward_data {
        let entry: MissionRewardEntry = serde_json::from_value(v.clone())
            .map_err(|e| format!("Malformed reward data entry: {} {}", e, v))?;
        let mut item_types = Vec::with_capacity(entry.m_iMissionRewarItemType.len());
        for ty in entry.m_iMissionRewarItemType.iter() {
            let item_type: ItemType = (*ty)
                .try_into()
                .map_err(|e: FFError| e.get_msg().to_string())?;
            item_types.push(item_type);
        }
        let reward_id = entry.m_iMissionRewardID;
        let mut reward = Reward::new(RewardCategory::Missions);
        reward.taros = entry.m_iCash;
        reward.fusion_matter = entry.m_iFusionMatter;
        for (id, ty) in entry
            .m_iMissionRewardItemID
            .iter()
            .zip(item_types.iter())
            .filter(|(id, _)| **id != 0)
        {
            let item = Item::new(*ty, *id);
            reward.items.push(item);
        }
        rewards.insert(reward_id, reward);
    }

    Ok(MissionData {
        mission_definitions: mission_defs,
        task_definitions: task_defs,
        rewards,
    })
}

fn load_respawn_data(root: &Map<String, Value>) -> Result<Vec<RespawnPoint>, String> {
    const RESPAWN_TABLE_KEY: &str = "m_pXComTable";
    const RESPAWN_TABLE_RESPAWN_DATA_KEY: &str = "m_pXComData";

    #[derive(Debug, Deserialize)]
    struct RespawnPointEntry {
        m_iXpos: i32,
        m_iYpos: i32,
        m_iZpos: i32,
        m_iZone: u32,
    }

    let table = get_object(root, RESPAWN_TABLE_KEY)?;
    let respawn_data = get_array(table, RESPAWN_TABLE_RESPAWN_DATA_KEY)?;
    let mut respawn_points = Vec::new();
    for v in respawn_data {
        let entry: RespawnPointEntry = serde_json::from_value(v.clone())
            .map_err(|e| format!("Malformed respawn data entry: {} {}", e, v))?;
        let respawn_point = RespawnPoint {
            pos: Position {
                x: entry.m_iXpos,
                y: entry.m_iYpos,
                z: entry.m_iZpos,
            },
            map_num: entry.m_iZone,
        };
        respawn_points.push(respawn_point);
    }
    Ok(respawn_points)
}

fn load_player_data(root: &Map<String, Value>) -> Result<HashMap<i16, PlayerData>, String> {
    const PLAYER_TABLE_KEY: &str = "m_pAvatarTable";
    const PLAYER_TABLE_PLAYER_DATA_KEY: &str = "m_pAvatarGrowData";

    #[derive(Debug, Deserialize)]
    struct PlayerStatsEntry {
        m_iLevel: i16,
        m_iHpUp: u32,
        m_iMaxHP: u32,
        m_iAccuracy: u32,
        m_iDodge: u32,
        m_iPower: u32,
        m_iProtection: u32,
        m_iReqBlob_NanoCreate: u32,
        m_iReqBlob_NanoTune: u32,
        m_iFMLimit: u32,
        m_iMobFM: u32,
        m_iNanoQuestTaskID: i32,
        m_iNanoID: i16,
        m_iBonusFM: u32,
        m_iDeathFM: u32,
    }

    let table = get_object(root, PLAYER_TABLE_KEY)?;
    let player_data = get_array(table, PLAYER_TABLE_PLAYER_DATA_KEY)?;
    let mut player_data_table = HashMap::new();
    for v in player_data {
        let entry: PlayerStatsEntry = serde_json::from_value(v.clone())
            .map_err(|e| format!("Malformed player data entry: {} {}", e, v))?;

        let key = entry.m_iLevel;
        let player_stats = PlayerStats {
            hp_up: entry.m_iHpUp,
            max_hp: entry.m_iMaxHP,
            req_fm_nano_create: entry.m_iReqBlob_NanoCreate,
            req_fm_nano_tune: entry.m_iReqBlob_NanoTune,
            fm_limit: entry.m_iFMLimit,
            nano_quest_task_id: match entry.m_iNanoQuestTaskID {
                0 => None,
                tid => Some(tid),
            },
            nano_id: entry.m_iNanoID,
        };

        let player_data = PlayerData {
            stats: player_stats,
        };

        player_data_table.insert(key, player_data);
    }

    Ok(player_data_table)
}

fn load_npc_data(root: &Map<String, Value>) -> Result<HashMap<i32, NPCData>, String> {
    const NPC_TABLE_KEY: &str = "m_pNpcTable";
    const NPC_TABLE_NPC_DATA_KEY: &str = "m_pNpcData";
    const NPC_TABLE_NPC_STRINGS_KEY: &str = "m_pNpcStringData";

    #[derive(Deserialize)]
    struct NPCStatsEntry {
        m_iNpcNumber: i32,
        m_iNpcName: i32,
        m_iTeam: i32,
        m_iNpcLevel: i16,
        m_iHP: u32,
        m_iPower: i32,
        m_iProtection: i32,
        m_iNpcStyle: i32,
        m_iRadius: u32,
        m_iWalkSpeed: i32,
        m_iRunSpeed: i32,
        m_iSightRange: u32,
        m_iIdleRange: u32,
        m_iCombatRange: u32,
        m_iAtkRange: u32,
        m_iRegenTime: u64,
        m_iDelayTime: u64,
        m_iAiType: u8,
        m_iBarkerType: usize,
    }

    #[derive(Deserialize)]
    struct NPCStringsEntry {
        m_strName: String,
    }

    let table = get_object(root, NPC_TABLE_KEY)?;
    let npc_data = get_array(table, NPC_TABLE_NPC_DATA_KEY)?;
    let npc_strings = get_array(table, NPC_TABLE_NPC_STRINGS_KEY)?;
    let mut npc_data_table = HashMap::new();
    for v in npc_data {
        let entry: NPCStatsEntry = serde_json::from_value(v.clone())
            .map_err(|e| format!("Malformed NPC data entry: {} {}", e, v))?;
        let key = entry.m_iNpcNumber;
        let npc_stats = NPCStats {
            team: entry
                .m_iTeam
                .try_into()
                .map_err(|e: FFError| e.get_msg().to_string())?,
            level: entry.m_iNpcLevel,
            max_hp: entry.m_iHP,
            power: entry.m_iPower,
            defense: entry.m_iProtection,
            radius: entry.m_iRadius,
            walk_speed: entry.m_iWalkSpeed,
            run_speed: entry.m_iRunSpeed,
            sight_range: entry.m_iSightRange,
            style: entry
                .m_iNpcStyle
                .try_into()
                .map_err(|e: FFError| e.get_msg().to_string())?,
            idle_range: entry.m_iIdleRange,
            combat_range: entry.m_iCombatRange,
            attack_range: entry.m_iAtkRange,
            regen_time: entry.m_iRegenTime,
            delay_time: entry.m_iDelayTime,
            ai_type: entry.m_iAiType,
            bark_type: match entry.m_iBarkerType {
                0 => None,
                x => Some(x),
            },
        };

        let npc_strings = npc_strings.get(entry.m_iNpcName as usize).ok_or(format!(
            "NPC string for ID {} doesn't exist",
            entry.m_iNpcName
        ))?;

        let npc_string_entry: NPCStringsEntry = serde_json::from_value(npc_strings.clone())
            .map_err(|e| format!("Malformed NPC string entry: {} {}", e, npc_strings))?;

        let npc_data = NPCData {
            stats: npc_stats,
            name: npc_string_entry.m_strName,
        };

        npc_data_table.insert(key, npc_data);
    }

    Ok(npc_data_table)
}

fn load_npcs() -> Result<Vec<NPCSpawnData>, String> {
    const NPC_TABLE_KEY: &str = "NPCs";
    const MOB_TABLE_KEY: &str = "mobs";
    const MOB_GROUP_TABLE_KEY: &str = "groups";

    fn load_npc_table(
        table: &Map<String, Value>,
        is_group: bool,
    ) -> Result<Vec<NPCSpawnData>, String> {
        #[derive(Deserialize)]
        struct FollowerDataEntry {
            iNPCType: i32,
            iOffsetX: i32,
            iOffsetY: i32,
        }

        #[derive(Deserialize)]
        struct NPCSpawnDataEntry {
            aFollowers: Option<Vec<FollowerDataEntry>>,
            iAngle: i32,
            iMapNum: Option<u32>,
            iNPCType: i32,
            iX: i32,
            iY: i32,
            iZ: i32,
        }

        let mut npc_data = Vec::new();
        for (k, v) in table {
            let npc_data_entry: NPCSpawnDataEntry = serde_json::from_value(v.clone())
                .map_err(|e| format!("Malformed NPC data entry: {}", e))?;

            if let FFProtocol::v1013 = config_get().general.protocol.get() {
                // Academy's map doesn't include the Future; skip all NPCs spawned there for parity with OpenFusion
                if npc_data_entry.iX > 512000 && npc_data_entry.iY < 256000 {
                    continue;
                }
            }

            let key: i32 = k.parse().map_err(|e| format!("Malformed NPC key: {}", e))?;
            let npc_data_entry = NPCSpawnData {
                group_id: if is_group { Some(key) } else { None },
                npc_type: npc_data_entry.iNPCType,
                pos: Position {
                    x: npc_data_entry.iX,
                    y: npc_data_entry.iY,
                    z: npc_data_entry.iZ,
                },
                angle: npc_data_entry.iAngle,
                map_num: npc_data_entry.iMapNum,
                followers: if let Some(followers) = npc_data_entry.aFollowers {
                    followers
                        .into_iter()
                        .map(|f| FollowerData {
                            npc_type: f.iNPCType,
                            offset: Position {
                                x: f.iOffsetX,
                                y: f.iOffsetY,
                                z: 0,
                            },
                        })
                        .collect()
                } else {
                    Vec::new()
                },
            };
            npc_data.push(npc_data_entry);
        }
        Ok(npc_data)
    }

    let mut npc_data = Vec::new();

    let npc_root = load_json("NPCs.json")?;
    let npc_table = get_object(&npc_root, NPC_TABLE_KEY)?;
    npc_data.extend(load_npc_table(npc_table, false)?);

    let mob_root = load_json("mobs.json")?;
    let mob_table = get_object(&mob_root, MOB_TABLE_KEY)?;
    npc_data.extend(load_npc_table(mob_table, false)?);
    let grouped_mob_table = get_object(&mob_root, MOB_GROUP_TABLE_KEY)?;
    npc_data.extend(load_npc_table(grouped_mob_table, true)?);

    Ok(npc_data)
}

fn load_drop_data() -> Result<DropData, String> {
    const CRATE_DROP_CHANCES_TABLE_KEY: &str = "CrateDropChances";
    const CRATE_DROP_TYPES_TABLE_KEY: &str = "CrateDropTypes";
    const CRATE_DATA_TABLE_KEY: &str = "Crates";
    const MISC_DROP_CHANCES_TABLE_KEY: &str = "MiscDropChances";
    const MISC_DROP_TYPES_TABLE_KEY: &str = "MiscDropTypes";
    const MOB_DROPS_TABLE_KEY: &str = "MobDrops";
    const MOB_DROP_DATA_TABLE_KEY: &str = "Mobs";
    const RARITY_WEIGHTS_TABLE_KEY: &str = "RarityWeights";
    const ITEM_SETS_TABLE_KEY: &str = "ItemSets";
    const ITEM_REFERENCES_TABLE_KEY: &str = "ItemReferences";
    const NANO_CAPSULES_TABLE_KEY: &str = "NanoCapsules";
    const CODE_ITEMS_TABLE_KEY: &str = "CodeItems";

    const CRATE_DROP_CHANCES_ID_KEY: &str = "CrateDropChanceID";
    const CRATE_DROP_TYPES_ID_KEY: &str = "CrateDropTypeID";
    const CRATE_DATA_ID_KEY: &str = "CrateID";
    const MISC_DROP_CHANCES_ID_KEY: &str = "MiscDropChanceID";
    const MISC_DROP_TYPES_ID_KEY: &str = "MiscDropTypeID";
    const MOB_DROP_ID_KEY: &str = "MobDropID";
    const MOB_DROP_DATA_ID_KEY: &str = "MobID";
    const RARITY_WEIGHTS_ID_KEY: &str = "RarityWeightID";
    const ITEM_SETS_ID_KEY: &str = "ItemSetID";
    const ITEM_REFERENCES_ID_KEY: &str = "ItemReferenceID";

    fn load_nano_capsules(table: &Map<String, Value>) -> Result<HashMap<i16, i16>, String> {
        #[derive(Deserialize)]
        struct NanoCapsuleEntry {
            CrateID: i16,
            Nano: i16,
        }

        let mut capsules = HashMap::new();
        for (_, v) in table {
            let entry: NanoCapsuleEntry = serde_json::from_value(v.clone())
                .map_err(|e| format!("Malformed nano capsule entry: {}", e))?;
            capsules.insert(entry.CrateID, entry.Nano);
        }
        Ok(capsules)
    }

    fn load_code_items(
        table: &Map<String, Value>,
        item_refs: &HashMap<i32, ItemReference>,
    ) -> Result<HashMap<String, Vec<Item>>, String> {
        #[derive(Deserialize)]
        struct CodeItemEntry {
            Code: String,
            ItemReferenceIDs: Vec<i32>,
        }

        let mut code_items = HashMap::new();
        for (_, v) in table {
            let entry: CodeItemEntry = serde_json::from_value(v.clone())
                .map_err(|e| format!("Malformed code item entry: {}", e))?;

            let mut items = Vec::with_capacity(entry.ItemReferenceIDs.len());
            for item_ref_id in entry.ItemReferenceIDs {
                let item = item_refs.get(&item_ref_id).and_then(|item_ref| {
                    let ty: ItemType = (item_ref.Type as i16).try_into().ok()?;
                    let id = item_ref.ItemID as i16;
                    (id != 0).then(|| Item::new(ty, id))
                });

                match item {
                    Some(item) => items.push(item),
                    None => log(
                        Severity::Warning,
                        &format!(
                            "Invalid item reference {} for code {}, skipping",
                            item_ref_id, entry.Code
                        ),
                    ),
                }
            }

            if !items.is_empty() {
                code_items.insert(entry.Code.to_lowercase(), items);
            }
        }
        Ok(code_items)
    }

    fn load_drop_table<T: DeserializeOwned>(
        table: &Map<String, Value>,
        id_key: &str,
    ) -> Result<HashMap<i32, T>, String> {
        let mut data_map = HashMap::new();
        for (_, v) in table {
            let key = v.get(id_key).ok_or(format!("Key missing: {}", id_key))?;
            let Value::Number(key) = key else {
                return Err(format!("Key not numeric: {}", key));
            };
            let key = key.as_i64().ok_or(format!("Key not an integer: {}", key))?;
            let val: T = serde_json::from_value(v.clone())
                .map_err(|e| format!("Malformed drops data entry: {}", e))?;
            data_map.insert(key as i32, val);
        }
        Ok(data_map)
    }

    let drop_root = load_json("drops.json")?;

    let crate_drop_chances_table = get_object(&drop_root, CRATE_DROP_CHANCES_TABLE_KEY)?;
    let crate_drop_types_table = get_object(&drop_root, CRATE_DROP_TYPES_TABLE_KEY)?;
    let crate_data_table = get_object(&drop_root, CRATE_DATA_TABLE_KEY)?;

    let misc_drop_chances_table = get_object(&drop_root, MISC_DROP_CHANCES_TABLE_KEY)?;
    let misc_drop_types_table = get_object(&drop_root, MISC_DROP_TYPES_TABLE_KEY)?;

    let mob_drops_table = get_object(&drop_root, MOB_DROPS_TABLE_KEY)?;
    let mob_drop_data_table = get_object(&drop_root, MOB_DROP_DATA_TABLE_KEY)?;

    let rarity_weights_table = get_object(&drop_root, RARITY_WEIGHTS_TABLE_KEY)?;
    let item_sets_table = get_object(&drop_root, ITEM_SETS_TABLE_KEY)?;
    let item_references_table = get_object(&drop_root, ITEM_REFERENCES_TABLE_KEY)?;

    let events = match drop_root.get("Events") {
        Some(Value::Object(table)) => load_drop_table(table, "EventID")?,
        None => HashMap::new(),
        _ => return Err("Events must be an object".to_string()),
    };
    // only Academy tabledata defines nano capsules
    let nano_capsules = match drop_root.get(NANO_CAPSULES_TABLE_KEY) {
        Some(Value::Object(table)) => load_nano_capsules(table)?,
        _ => HashMap::new(),
    };

    let item_refs = load_drop_table(item_references_table, ITEM_REFERENCES_ID_KEY)?;

    // only Academy tabledata defines code items
    let code_items = match drop_root.get(CODE_ITEMS_TABLE_KEY) {
        Some(Value::Object(table)) => load_code_items(table, &item_refs)?,
        _ => HashMap::new(),
    };

    Ok(DropData {
        events,
        crate_drop_chances: load_drop_table(crate_drop_chances_table, CRATE_DROP_CHANCES_ID_KEY)?,
        crate_drop_types: load_drop_table(crate_drop_types_table, CRATE_DROP_TYPES_ID_KEY)?,
        crate_data: load_drop_table(crate_data_table, CRATE_DATA_ID_KEY)?,

        misc_drop_chances: load_drop_table(misc_drop_chances_table, MISC_DROP_CHANCES_ID_KEY)?,
        misc_drop_types: load_drop_table(misc_drop_types_table, MISC_DROP_TYPES_ID_KEY)?,

        mob_drops: load_drop_table(mob_drops_table, MOB_DROP_ID_KEY)?,
        mob_drop_data: load_drop_table(mob_drop_data_table, MOB_DROP_DATA_ID_KEY)?,

        rarity_weights: load_drop_table(rarity_weights_table, RARITY_WEIGHTS_ID_KEY)?,
        item_sets: load_drop_table(item_sets_table, ITEM_SETS_ID_KEY)?,
        item_refs,
        nano_capsules,
        code_items,
    })
}

fn load_egg_data() -> Result<EggData, String> {
    const EGG_TYPES_TABLE_KEY: &str = "EggTypes";
    const EGG_TABLE_KEY: &str = "Eggs";

    fn load_egg_stats(table: &Map<String, Value>) -> Result<HashMap<i32, EggStats>, String> {
        #[derive(Deserialize)]
        struct EggStatsEntry {
            Id: i32,
            DropCrateId: i16,
            EffectId: i32,
            Duration: usize,
            Regen: usize,
        }

        let mut egg_stats = HashMap::new();
        for (_, v) in table {
            let egg_stats_entry: EggStatsEntry = serde_json::from_value(v.clone())
                .map_err(|e| format!("Malformed egg stats entry: {} {}", e, v))?;
            let key = egg_stats_entry.Id;
            let egg_stats_entry = EggStats {
                crate_id: match egg_stats_entry.DropCrateId {
                    0 => None,
                    x => Some(x),
                },
                effect_id: match egg_stats_entry.EffectId {
                    0 => None,
                    x => Some(x),
                },
                effect_duration: Duration::from_secs(egg_stats_entry.Duration as u64),
                respawn_time: Duration::from_secs(egg_stats_entry.Regen as u64),
            };
            egg_stats.insert(key, egg_stats_entry);
        }
        Ok(egg_stats)
    }

    fn load_eggs(table: &Map<String, Value>) -> Result<Vec<EggSpawnData>, String> {
        #[derive(Deserialize)]
        struct EggSpawnDataEntry {
            iType: i32,
            iX: i32,
            iY: i32,
            iZ: i32,
            iMapNum: Option<u32>,
        }

        let mut eggs = Vec::new();
        for (_, v) in table {
            let egg_data_entry: EggSpawnDataEntry = serde_json::from_value(v.clone())
                .map_err(|e| format!("Malformed egg data entry: {} {}", e, v))?;
            let egg_data_entry = EggSpawnData {
                egg_type: egg_data_entry.iType,
                pos: Position {
                    x: egg_data_entry.iX,
                    y: egg_data_entry.iY,
                    z: egg_data_entry.iZ,
                },
                map_num: egg_data_entry.iMapNum,
            };
            eggs.push(egg_data_entry);
        }
        Ok(eggs)
    }

    let egg_root = load_json("eggs.json")?;

    let egg_types_table = get_object(&egg_root, EGG_TYPES_TABLE_KEY)?;
    let eggs_table = get_object(&egg_root, EGG_TABLE_KEY)?;

    Ok(EggData {
        egg_stats: load_egg_stats(egg_types_table)?,
        eggs: load_eggs(eggs_table)?,
    })
}

fn load_path_data() -> Result<PathData, String> {
    #[derive(Deserialize)]
    struct PathPointEntry {
        iX: i32,
        iY: i32,
        iZ: i32,
        bStop: Option<bool>,
        iStopTicks: Option<usize>,
    }

    fn load_skyway_paths(root: &Map<String, Value>) -> Result<HashMap<i32, Path>, String> {
        const SKYWAY_TABLE_KEY: &str = "skyway";

        #[derive(Deserialize)]
        struct SkywayPathEntry {
            iRouteID: i32,
            iMonkeySpeed: i32,
            aPoints: Vec<PathPointEntry>,
        }

        let skyway_table = get_object(root, SKYWAY_TABLE_KEY)?;
        let mut skyway_paths = HashMap::new();
        for (_, v) in skyway_table {
            let skyway_path_entry: SkywayPathEntry = serde_json::from_value(v.clone())
                .map_err(|e| format!("Malformed skyway path entry: {} {}", e, v))?;
            let key = skyway_path_entry.iRouteID;
            let speed = skyway_path_entry.iMonkeySpeed;
            let mut points = Vec::new();
            for point in &skyway_path_entry.aPoints {
                points.push(PathPoint {
                    pos: Position {
                        x: point.iX,
                        y: point.iY,
                        z: point.iZ,
                    },
                    speed,
                    stop_ticks: 0,
                });
            }
            let skyway_path = Path::new(points, false);
            skyway_paths.insert(key, skyway_path);
        }
        Ok(skyway_paths)
    }

    fn load_slider_path(root: &Map<String, Value>) -> Result<Path, String> {
        const SLIDER_TABLE_KEY: &str = "slider";
        const SLIDER_SPEED: i32 = 1200;
        const SLIDER_SPEED_SLOW: i32 = 450;
        const SLIDER_STOP_TICKS: usize = 16;

        let slider_table = get_object(root, SLIDER_TABLE_KEY)?;
        let mut points = Vec::new();
        let mut was_stop = false;
        for (_, v) in slider_table {
            let point: PathPointEntry = serde_json::from_value(v.clone())
                .map_err(|e| format!("Malformed slider path entry: {} {}", e, v))?;
            let is_stop = point.bStop.unwrap();
            points.push(PathPoint {
                pos: Position {
                    x: point.iX,
                    y: point.iY,
                    z: point.iZ,
                },
                // we go slow if we're approaching or leaving a stop.
                speed: if is_stop || was_stop {
                    SLIDER_SPEED_SLOW
                } else {
                    SLIDER_SPEED
                },
                stop_ticks: if is_stop { SLIDER_STOP_TICKS } else { 0 },
            });
            was_stop = is_stop;
        }
        Ok(Path::new(points, true))
    }

    fn load_npc_paths(root: &Map<String, Value>) -> Result<HashMap<i32, Path>, String> {
        const NPC_TABLE_KEY: &str = "npc";

        #[derive(Deserialize)]
        struct NPCPathEntry {
            aNPCTypes: Option<Vec<i32>>,
            aNPCIDs: Option<Vec<i64>>,
            iBaseSpeed: i32,
            aPoints: Vec<PathPointEntry>,
        }

        let npc_table = get_object(root, NPC_TABLE_KEY)?;
        let mut npc_paths = HashMap::new();
        for (_, v) in npc_table {
            let npc_path_entry: NPCPathEntry = serde_json::from_value(v.clone())
                .map_err(|e| format!("Malformed NPC path entry: {} {}", e, v))?;
            let mut points = Vec::new();
            for point in &npc_path_entry.aPoints {
                points.push(PathPoint {
                    pos: Position {
                        x: point.iX,
                        y: point.iY,
                        z: point.iZ,
                    },
                    speed: npc_path_entry.iBaseSpeed,
                    stop_ticks: point.iStopTicks.unwrap(),
                });
            }
            let cycle = if points[0] == points[points.len() - 1] {
                // cyclic NPC paths in tdata have the starting point
                // duplicated at the end, but this messes up our math.
                points.pop();
                true
            } else {
                false
            };
            let npc_path = Path::new(points, cycle);
            for npc_type in &npc_path_entry.aNPCTypes.unwrap_or_default() {
                // currently, OpenFusion tabledata for paths does not
                // have a field for initial path state; however,
                // we really only want non-cyclic paths to wait,
                // so we can auto-start the rest.
                let mut path_cloned = npc_path.clone();
                if cycle {
                    path_cloned.start();
                }
                npc_paths.insert(*npc_type, path_cloned);
            }
        }
        Ok(npc_paths)
    }

    let paths_root = load_json("paths.json")?;
    Ok(PathData {
        skyway_paths: load_skyway_paths(&paths_root)?,
        slider_path: load_slider_path(&paths_root)?,
        npc_paths: load_npc_paths(&paths_root)?,
    })
}

fn load_world_name_data() -> Result<WorldNameDataContainer, String> {
    const WORLD_NAME_TABLE_KEY: &str = "m_pWorldNameData";

    #[derive(Deserialize)]
    struct Area {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    }

    #[derive(Deserialize)]
    struct WorldNameDataEntry {
        Area: Area,
        DongName: String,
        ZoneName: String,
    }

    let world_name_root = load_json("worldnames.json")?;
    let world_name_table = get_object(&world_name_root, WORLD_NAME_TABLE_KEY)?;
    let mut world_name_data = HashMap::new();
    let mut lookup_grid = HashMap::new();
    for (k, v) in world_name_table.iter().skip(1) {
        let entry: WorldNameDataEntry = serde_json::from_value(v.clone())
            .map_err(|e| format!("Malformed world name data entry: {} {}", e, v))?;

        let id = k
            .parse::<i32>()
            .map_err(|e| format!("Invalid key: {}", e))?;
        world_name_data.insert(
            id,
            WorldNameData {
                area_name: entry.DongName,
                zone_name: entry.ZoneName,
            },
        );

        let pos1 = Position::from_client_coords(entry.Area.x, entry.Area.y, 0.0);
        let (chunk1x, chunk1y) = world_pos_to_chunk_pos(pos1);

        let pos2 = Position::from_client_coords(
            entry.Area.x + entry.Area.width,
            entry.Area.y + entry.Area.height,
            0.0,
        );
        let (chunk2x, chunk2y) = world_pos_to_chunk_pos(pos2);

        for x in chunk1x..=chunk2x {
            for y in chunk1y..=chunk2y {
                lookup_grid.insert((x, y), id);
            }
        }
    }

    Ok(WorldNameDataContainer {
        world_name_data,
        lookup_grid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load() {
        tdata_init().expect("Failed to load tabledata");
    }
}

fn eligible_rarity_weights(weights: &[i32], rarities: &[i32]) -> Vec<i32> {
    weights
        .iter()
        .enumerate()
        .map(|(index, weight)| {
            if rarities.contains(&((index + 1) as i32)) {
                (*weight).max(0)
            } else {
                0
            }
        })
        .collect()
}

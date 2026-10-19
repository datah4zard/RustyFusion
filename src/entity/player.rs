use std::{
    any::Any,
    collections::HashMap,
    fmt::Display,
    time::{Duration, SystemTime},
};

use crate::{
    chunk::{ChunkCoords, InstanceID},
    config::config_get,
    defines::*,
    entity::{Combatant, Entity, EntityID},
    enums::{
        BuffID, BuffType, CharType, CombatStyle, CombatantTeam, ItemLocation, ItemType,
        PlayerGuide, PlayerNameStatus, RewardCategory, RewardType, RideType, TaskType,
    },
    error::{codes, log, log_if_failed, FFError, FFResult, Severity},
    helpers,
    item::{Item, RewardItem},
    mission::{MissionJournal, Task, TaskDefinition},
    nano::Nano,
    net::{
        packet::{
            PacketID::{self, *},
            *,
        },
        FFClient, FFProtocol,
    },
    path::Path,
    skills::{BuffContainer, BuffInstance},
    state::ShardServerState,
    tabledata::{tdata_get, TripData},
    util::{self, clamp, clamp_max, clamp_min, Bitfield},
    Position,
};

use rand::Rng;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct PlayerMetadata {
    pub first_name: String,
    pub last_name: String,
    pub x_coord: i32,
    pub y_coord: i32,
    pub z_coord: i32,
    pub channel: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct PlayerStyle {
    pub gender: i8,
    pub face_style: i8,
    pub hair_style: i8,
    pub hair_color: i8,
    pub skin_color: i8,
    pub eye_color: i8,
    pub height: i8,
    pub body: i8,
}
impl TryFrom<sPCStyle> for PlayerStyle {
    type Error = FFError;

    fn try_from(style: sPCStyle) -> FFResult<Self> {
        // TODO style validation
        Ok(Self {
            gender: style.iGender,
            face_style: style.iFaceStyle,
            hair_style: style.iHairStyle,
            hair_color: style.iHairColor,
            skin_color: style.iSkinColor,
            eye_color: style.iEyeColor,
            height: style.iHeight,
            body: style.iBody,
        })
    }
}
impl Default for PlayerStyle {
    fn default() -> Self {
        Self {
            gender: 1,
            face_style: 1,
            hair_style: 1,
            hair_color: 1,
            skin_color: 1,
            eye_color: 1,
            height: 0,
            body: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlayerFlags {
    pub name_check: PlayerNameStatus,
    pub tutorial_flag: bool,
    pub payzone_flag: bool,
    pub tip_flags: Bitfield<i64>,
    pub scamper_flags: Bitfield<i32>,
    pub skyway_flags: Bitfield<i64>,
}
impl Default for PlayerFlags {
    fn default() -> Self {
        Self {
            name_check: PlayerNameStatus::Pending,
            tutorial_flag: false,
            payzone_flag: false,
            tip_flags: Bitfield::new(SIZEOF_TIP_FLAGS),
            scamper_flags: Bitfield::new(SIZEOF_SCAMPER_FLAGS),
            skyway_flags: Bitfield::new(WYVERN_LOCATION_FLAG_SIZE as usize),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct GuideData {
    current_guide: PlayerGuide,
    total_guides: usize,
}
impl Default for GuideData {
    fn default() -> Self {
        Self {
            current_guide: PlayerGuide::Computress,
            total_guides: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SkywayRideState {
    trip_data: &'static TripData,
    path: Path,
    monkey_pos: Position,
    resume_time: SystemTime,
}

#[derive(Debug, Clone)]
pub struct Nanocom {
    nano_inventory: HashMap<i16, Nano>,
    equipped_ids: [Option<i16>; SIZEOF_NANO_CARRY_SLOT as usize],
    active_slot: Option<usize>,
}
impl Nanocom {
    pub fn as_bank(&self) -> [sNano; SIZEOF_NANO_BANK_SLOT as usize] {
        let mut bank = [None.into_proto(); SIZEOF_NANO_BANK_SLOT as usize];
        for (id, nano) in &self.nano_inventory {
            let idx = *id as usize;
            if idx < SIZEOF_NANO_BANK_SLOT as usize {
                bank[idx] = Some(nano).into_proto();
            }
        }
        bank
    }

    pub fn as_slots(&self) -> [i16; SIZEOF_NANO_CARRY_SLOT as usize] {
        let mut slots = [0; SIZEOF_NANO_CARRY_SLOT as usize];
        for (idx, nano_id) in self.equipped_ids.iter().enumerate() {
            if let Some(nano_id) = nano_id {
                slots[idx] = *nano_id;
            }
        }
        slots
    }

    pub fn as_carried(&self) -> [sNano; SIZEOF_NANO_CARRY_SLOT as usize] {
        let mut carried = [None.into_proto(); SIZEOF_NANO_CARRY_SLOT as usize];
        for (idx, nano_id) in self.equipped_ids.iter().enumerate() {
            if let Some(nano_id) = nano_id {
                carried[idx] = Some(self.nano_inventory.get(nano_id).unwrap()).into_proto();
            }
        }
        carried
    }
}
impl Default for Nanocom {
    fn default() -> Self {
        Self {
            nano_inventory: HashMap::new(),
            equipped_ids: [None; SIZEOF_NANO_CARRY_SLOT as usize],
            active_slot: None,
        }
    }
}

#[derive(Debug, Clone)]
struct PlayerInventory {
    main: [Option<Item>; SIZEOF_INVEN_SLOT as usize],
    equipped: [Option<Item>; MAX_SIZEOF_EQUIP_SLOT],
    quest: [Option<(i16, usize)>; SIZEOF_QINVEN_SLOT as usize],
    bank: [Option<Item>; MAX_SIZEOF_BANK_SLOT],
}
impl Default for PlayerInventory {
    fn default() -> Self {
        Self {
            main: [None; SIZEOF_INVEN_SLOT as usize],
            equipped: [None; MAX_SIZEOF_EQUIP_SLOT],
            quest: [None; SIZEOF_QINVEN_SLOT as usize],
            bank: [None; MAX_SIZEOF_BANK_SLOT],
        }
    }
}
impl PlayerInventory {
    fn get_quest_item_arr_104(&self) -> [v0104::sItemBase; SIZEOF_QINVEN_SLOT as usize] {
        self.quest.map(|vals| {
            let mut item_raw = v0104::sItemBase::default();
            if let Some((id, count)) = vals {
                item_raw.iType = ItemType::Quest as i16;
                item_raw.iID = id;
                item_raw.iOpt = count as i32;
            }
            item_raw
        })
    }

    fn get_quest_item_arr_1013(&self) -> [v1013::sItemBase; SIZEOF_QINVEN_SLOT as usize] {
        self.quest.map(|vals| {
            let mut item_raw = v1013::sItemBase::default();
            if let Some((id, count)) = vals {
                item_raw.iType = ItemType::Quest as i16;
                item_raw.iID = id;
                item_raw.iOpt = count as i32;
            }
            item_raw
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct RewardRates {
    combat: f32,
    missions: f32,
    eggs: f32,
    racing: f32,
}
impl Default for RewardRates {
    fn default() -> Self {
        Self {
            combat: 1.0,
            missions: 1.0,
            eggs: 1.0,
            racing: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RewardData {
    taros: RewardRates,
    fusion_matter: RewardRates,
}
impl RewardData {
    pub fn set_reward_rate(&mut self, reward_type: RewardType, category: RewardCategory, val: f32) {
        let reward_rates = match reward_type {
            RewardType::Taros => &mut self.taros,
            RewardType::FusionMatter => &mut self.fusion_matter,
        };
        let rate = match category {
            RewardCategory::Combat => &mut reward_rates.combat,
            RewardCategory::Missions => &mut reward_rates.missions,
            RewardCategory::Eggs => &mut reward_rates.eggs,
            RewardCategory::Racing => &mut reward_rates.racing,
            RewardCategory::All => {
                for cat in 1..5 {
                    let category: RewardCategory = cat.try_into().unwrap();
                    self.set_reward_rate(reward_type, category, val);
                }
                return;
            }
        };
        *rate = val / 100.0; // val is in percent
    }

    pub fn get_reward_rate(&self, reward_type: RewardType, category: usize) -> FFResult<f32> {
        let reward_rates = match reward_type {
            RewardType::Taros => &self.taros,
            RewardType::FusionMatter => &self.fusion_matter,
        };
        match category {
            1 => Ok(reward_rates.combat),
            2 => Ok(reward_rates.missions),
            3 => Ok(reward_rates.eggs),
            4 => Ok(reward_rates.racing),
            _ => Err(FFError::build(
                Severity::Warning,
                format!("Invalid reward rate category: {}", category),
            )),
        }
    }

    pub fn get_rates_as_array(&self, reward_type: RewardType) -> [f32; 5] {
        let reward_rates = match reward_type {
            RewardType::Taros => &self.taros,
            RewardType::FusionMatter => &self.fusion_matter,
        };
        [
            unused!(),
            reward_rates.combat,
            reward_rates.missions,
            reward_rates.eggs,
            reward_rates.racing,
        ]
    }
}

#[derive(Debug, Clone, Default)]
pub struct PreWarpData {
    pub instance_id: InstanceID,
    pub position: Position,
}

#[derive(Debug, Clone)]
pub struct BuddyListEntry {
    pub pc_uid: i64,
    pub first_name: String,
    pub last_name: String,
    pub style: PlayerStyle,
    pub name_check: PlayerNameStatus,
    pub free_chat: bool,
    pub blocked: bool,
}
impl BuddyListEntry {
    pub fn new(player: &Player) -> Self {
        Self {
            pc_uid: player.uid,
            first_name: player.first_name.clone(),
            last_name: player.last_name.clone(),
            style: player.style.unwrap(),
            name_check: player.flags.name_check,
            free_chat: true,
            blocked: false,
        }
    }
}
impl From<BuddyListEntry> for sBuddyBaseInfo {
    fn from(value: BuddyListEntry) -> Self {
        Self {
            iID: unused!(),      // updated later
            iPCState: unused!(), // updated later
            iPCUID: value.pc_uid,
            bBlocked: value.blocked as i8,
            bFreeChat: value.free_chat as i8,
            szFirstName: util::encode_utf16(&value.first_name).unwrap(),
            szLastName: util::encode_utf16(&value.last_name).unwrap(),
            iGender: value.style.gender,
            iNameCheckFlag: value.name_check as i8,
        }
    }
}

#[derive(Debug, Clone)]
struct BuddyList {
    slots: [Option<Box<BuddyListEntry>>; SIZEOF_BUDDYLIST_SLOT as usize],
}
impl Default for BuddyList {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
        }
    }
}
impl BuddyList {
    fn get_buddy_slot_number(&self, pc_uid: i64) -> Option<usize> {
        self.slots
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|entry| entry.pc_uid == pc_uid))
    }

    fn is_buddies_with(&self, pc_uid: i64) -> bool {
        self.get_buddy_slot_number(pc_uid).is_some()
    }

    fn insert_buddy(&mut self, buddy: BuddyListEntry) -> FFResult<usize> {
        if self.is_buddies_with(buddy.pc_uid) {
            return Err(FFError::build(
                Severity::Warning,
                format!("Player {} is already on the buddy list", buddy.pc_uid),
            ));
        }

        for (idx, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(Box::new(buddy));
                return Ok(idx);
            }
        }

        Err(FFError::build(
            Severity::Warning,
            "No free buddy slots".to_string(),
        ))
    }

    fn erase_buddy(&mut self, pc_uid: i64) -> FFResult<usize> {
        let idx = self.get_buddy_slot_number(pc_uid).ok_or(FFError::build(
            Severity::Warning,
            format!("Player {} is not on the buddy list", pc_uid),
        ))?;
        self.slots[idx] = None;
        Ok(idx)
    }

    fn block_buddy(&mut self, pc_uid: i64) -> FFResult<usize> {
        let idx = self.get_buddy_slot_number(pc_uid).ok_or(FFError::build(
            Severity::Warning,
            format!("Player {} is not on the buddy list", pc_uid),
        ))?;
        self.slots[idx].as_mut().unwrap().blocked = true;
        Ok(idx)
    }

    fn get_num_buddies(&self) -> usize {
        self.slots.iter().filter(|entry| entry.is_some()).count()
    }

    fn get_all_entries(&self) -> Vec<BuddyListEntry> {
        self.slots
            .iter()
            .filter_map(|entry| entry.as_ref().map(|entry| entry.as_ref().clone()))
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Player {
    id: Option<i32>,
    slot_num: usize,
    uid: i64,
    pub first_name: String,
    pub last_name: String,
    client: Option<FFClient>,
    pub perms: i16,
    pub show_gm_marker: bool,
    pub invisible: bool,
    pub invulnerable: bool,
    pub in_menu: bool,
    pub in_combat: bool,
    pub last_attacked_by: Option<EntityID>,
    pub target: Option<EntityID>,
    pub freechat_muted: bool,
    pub reward_data: RewardData,
    position: Position,
    rotation: i32,
    buffs: BuffContainer,
    pub instance_id: InstanceID,
    pub style: Option<PlayerStyle>,
    pub flags: PlayerFlags,
    level: i16,
    hp: i32,
    guide_data: GuideData,
    pub nano_data: Nanocom,
    pub mission_journal: MissionJournal,
    inventory: PlayerInventory,
    taros: u32,
    fusion_matter: u32,
    nano_potions: u32,
    weapon_boosts: u32,
    pub buddy_list_synced: bool,
    buddy_list: BuddyList,
    pub buddy_offered_to: Option<i64>,
    pub buddy_warp_available_at: Option<u32>,
    last_heal_time: Option<SystemTime>,
    pub last_warp_away_time: Option<SystemTime>,
    pub skyway_ride: Option<SkywayRideState>,
    pub trade_id: Option<Uuid>,
    pub trade_offered_to: Option<i32>,
    pub group_id: Option<Uuid>,
    pub group_offered_to: Option<i32>,
    pub vehicle_speed: Option<i32>,
    pre_warp_data: PreWarpData,
}
impl Player {
    pub fn new(uid: i64, slot_num: usize) -> Self {
        let start_level = 1;
        let stats = tdata_get().get_player_stats(start_level).unwrap();
        let mut reward_data = RewardData::default();
        reward_data.set_reward_rate(
            RewardType::Taros,
            RewardCategory::All,
            config_get().shard.taro_rate.get() as f32,
        );
        reward_data.set_reward_rate(
            RewardType::FusionMatter,
            RewardCategory::All,
            config_get().shard.fusion_matter_rate.get() as f32,
        );
        Self {
            uid,
            slot_num,
            reward_data,
            level: start_level,
            hp: stats.max_hp as i32,
            perms: CN_ACCOUNT_LEVEL__USER as i16,
            ..Default::default()
        }
    }

    pub fn get_uid(&self) -> i64 {
        self.uid
    }

    pub fn get_slot_num(&self) -> usize {
        self.slot_num
    }

    pub fn get_player_id(&self) -> i32 {
        self.id.expect("Player should always have an ID")
    }

    pub fn get_player_uid(&self) -> i64 {
        self.uid
    }

    pub fn set_player_id(&mut self, pc_id: i32) {
        self.id = Some(pc_id);
    }

    pub fn set_client(&mut self, client: FFClient) {
        self.client = Some(client);
    }

    pub fn get_style(&self) -> sPCStyle {
        let style = self.style.unwrap_or_default();
        sPCStyle {
            iPC_UID: self.uid,
            iNameCheck: self.flags.name_check as i8,
            szFirstName: util::encode_utf16(&self.first_name).unwrap(),
            szLastName: util::encode_utf16(&self.last_name).unwrap(),
            iGender: style.gender,
            iFaceStyle: style.face_style,
            iHairStyle: style.hair_style,
            iHairColor: style.hair_color,
            iSkinColor: style.skin_color,
            iEyeColor: style.eye_color,
            iHeight: style.height,
            iBody: style.body,
            iClass: unused!(),
        }
    }

    pub fn get_style_2(&self) -> sPCStyle2 {
        sPCStyle2 {
            iAppearanceFlag: if self.style.is_some() { 1 } else { 0 },
            iTutorialFlag: if self.flags.tutorial_flag { 1 } else { 0 },
            iPayzoneFlag: if self.flags.payzone_flag { 1 } else { 0 },
        }
    }

    pub fn get_mapnum(&self) -> u32 {
        self.instance_id.map_num
    }

    pub fn get_instance_id(&self) -> InstanceID {
        self.instance_id
    }

    pub fn set_instance_id(&mut self, instance_id: InstanceID) {
        self.instance_id = instance_id;
    }

    pub fn change_nano(&mut self, slot: usize, nano_id: Option<i16>) -> FFResult<()> {
        if !(0..SIZEOF_NANO_CARRY_SLOT as usize).contains(&slot) {
            return Err(FFError::build(
                Severity::Warning,
                format!("Invalid nano slot: {}", slot),
            ));
        }
        self.nano_data.equipped_ids[slot] = nano_id;
        Ok(())
    }

    pub fn set_nano(&mut self, nano: Nano) {
        self.nano_data.nano_inventory.insert(nano.get_id(), nano);
    }

    pub fn get_active_nano_slot(&self) -> Option<usize> {
        self.nano_data.active_slot
    }

    fn set_active_nano_slot(&mut self, slot: Option<usize>) -> FFResult<()> {
        if let Some(slot) = slot {
            if !(0..SIZEOF_NANO_CARRY_SLOT as usize).contains(&slot) {
                return Err(FFError::build(
                    Severity::Warning,
                    format!("Invalid nano slot: {}", slot),
                ));
            }
        }
        self.nano_data.active_slot = slot;
        Ok(())
    }

    pub fn deactivate_nano(&mut self) {
        if let Some(nano) = self.get_active_nano() {
            if let Some(skill) = nano.get_skill() {
                if skill.passive {
                    if let Some(buff_id) = skill.get_buff_id() {
                        self.remove_buff(buff_id, Some(BuffType::Nano));
                    }
                }
            }
        }
        self.nano_data.active_slot = None;
    }

    pub fn activate_nano(&mut self, slot: usize) -> FFResult<bool> {
        self.deactivate_nano();
        self.set_active_nano_slot(Some(slot))?;
        let mut buff_applied = false;
        if let Some(skill) = self.get_active_nano().and_then(|n| n.get_skill()) {
            if skill.passive {
                if let Some(buff_id) = skill.get_buff_id() {
                    let level = placeholder!(0);
                    let buff = skill.make_buff_instance(BuffType::Nano, level).unwrap();
                    let id = self.get_id();
                    self.apply_buff(buff_id, buff, Some(id));
                    buff_applied = true;
                }
            }
        }
        Ok(buff_applied)
    }

    pub fn get_active_nano(&self) -> Option<&Nano> {
        match self.nano_data.active_slot {
            Some(active_slot) => {
                let nano_id =
                    self.nano_data.equipped_ids[active_slot].expect("Empty nano equipped");
                let nano = self.nano_data.nano_inventory.get(&nano_id);
                Some(nano.expect("Locked nano equipped"))
            }
            None => None,
        }
    }

    pub fn get_active_nano_mut(&mut self) -> Option<&mut Nano> {
        match self.nano_data.active_slot {
            Some(active_slot) => {
                let nano_id =
                    self.nano_data.equipped_ids[active_slot].expect("Empty nano equipped");
                let nano = self.nano_data.nano_inventory.get_mut(&nano_id);
                Some(nano.expect("Locked nano equipped"))
            }
            None => None,
        }
    }

    pub fn unlock_nano(&mut self, nano_id: i16) -> FFResult<&mut Nano> {
        if self.nano_data.nano_inventory.contains_key(&nano_id) {
            return Err(FFError::build(
                Severity::Warning,
                format!("Nano {} is already unlocked", nano_id),
            ));
        }

        self.nano_data
            .nano_inventory
            .insert(nano_id, Nano::new(nano_id));
        Ok(self.get_nano_mut(nano_id).unwrap())
    }

    pub fn get_nano(&self, nano_id: i16) -> Option<&Nano> {
        self.nano_data.nano_inventory.get(&nano_id)
    }

    pub fn get_nano_mut(&mut self, nano_id: i16) -> Option<&mut Nano> {
        self.nano_data.nano_inventory.get_mut(&nano_id)
    }

    pub fn tune_nano(&mut self, nano_id: i16, skill_selection: Option<i16>) -> FFResult<()> {
        let nano = self.get_nano_mut(nano_id).ok_or(FFError::build(
            Severity::Warning,
            format!("Nano {} is locked", nano_id),
        ))?;

        let stats = tdata_get().get_nano_stats(nano_id).unwrap();

        if let Some(skill_id) = skill_selection {
            if !stats.skills.contains(&skill_id) {
                return Err(FFError::build(
                    Severity::Warning,
                    format!("Invalid skill id {} for nano {}", skill_id, nano.get_id()),
                ));
            }
        }

        nano.tune(skill_selection);
        Ok(())
    }

    pub fn get_nano_iter(&self) -> impl Iterator<Item = &Nano> {
        self.nano_data.nano_inventory.values()
    }

    pub fn get_load_data_104(&self) -> v0104::sPCLoadData2CL {
        v0104::sPCLoadData2CL {
            iUserLevel: 0, // allow anyone to send GM commands; we'll validate perms
            PCStyle: self.get_style(),
            PCStyle2: self.get_style_2(),
            iLevel: self.level,
            iMentor: self.guide_data.current_guide as i16,
            iMentorCount: self.guide_data.total_guides as i16,
            iHP: self.hp,
            iBatteryW: self.weapon_boosts as i32,
            iBatteryN: self.nano_potions as i32,
            iCandy: self.taros as i32,
            iFusionMatter: self.fusion_matter as i32,
            iSpecialState: self.get_special_state_bit_flag(),
            iMapNum: self.get_mapnum() as i32,
            iX: self.position.x,
            iY: self.position.y,
            iZ: self.position.z,
            iAngle: self.rotation,
            aEquip: self.get_equip_arr_104(),
            aInven: self.inventory.main.map(Option::<Item>::into_proto),
            aQInven: self.inventory.get_quest_item_arr_104(),
            aNanoBank: self.nano_data.as_bank(),
            aNanoSlots: self.nano_data.as_slots(),
            iActiveNanoSlotNum: match self.nano_data.active_slot {
                Some(active_slot) => active_slot as i16,
                None => -1,
            },
            iConditionBitFlag: self.get_condition_bit_flag(),
            eCSTB___Add: placeholder!(0),
            TimeBuff: v0104::sTimeBuff {
                iTimeLimit: placeholder!(0),
                iTimeDuration: placeholder!(0),
                iTimeRepeat: placeholder!(0),
                iValue: placeholder!(0),
                iConfirmNum: placeholder!(0),
            },
            aQuestFlag: self
                .mission_journal
                .completed_mission_flags
                .to_array()
                .unwrap(),
            aRepeatQuestFlag: unused!(),
            aRunningQuest: self.mission_journal.get_running_quests(),
            iCurrentMissionID: self.mission_journal.get_active_mission_id().unwrap_or(0),
            iWarpLocationFlag: self.flags.scamper_flags.get_chunk(0).unwrap(),
            aWyvernLocationFlag: self.flags.skyway_flags.to_array().unwrap(),
            iBuddyWarpTime: self
                .buddy_warp_available_at
                .map_or(0, |available_at| available_at as i32),
            iFatigue: unused!(),
            iFatigue_Level: unused!(),
            iFatigueRate: unused!(),
            iFirstUseFlag1: self.flags.tip_flags.get_chunk(0).unwrap(),
            iFirstUseFlag2: self.flags.tip_flags.get_chunk(1).unwrap(),
            aiPCSkill: [unused!(); 33],
        }
    }

    pub fn get_load_data_1013(&self) -> v1013::sPCLoadData2CL {
        v1013::sPCLoadData2CL {
            iUserLevel: 0, // allow anyone to send GM commands; we'll validate perms
            PCStyle: self.get_style(),
            PCStyle2: self.get_style_2(),
            iLevel: self.level,
            iMentor: self.guide_data.current_guide as i16,
            iMentorCount: self.guide_data.total_guides as i16,
            iHP: self.hp,
            iBatteryW: self.weapon_boosts as i32,
            iBatteryN: self.nano_potions as i32,
            iCandy: self.taros as i32,
            iFusionMatter: self.fusion_matter as i32,
            iSpecialState: self.get_special_state_bit_flag(),
            iMapNum: self.get_mapnum() as i32,
            iX: self.position.x,
            iY: self.position.y,
            iZ: self.position.z,
            iAngle: self.rotation,
            aEquip: self.get_equip_arr_1013(),
            aInven: self.inventory.main.map(Option::<Item>::into_proto),
            aQInven: self.inventory.get_quest_item_arr_1013(),
            aNanoBank: self.nano_data.as_bank(),
            aNanoSlots: self.nano_data.as_slots(),
            iActiveNanoSlotNum: match self.nano_data.active_slot {
                Some(active_slot) => active_slot as i16,
                None => -1,
            },
            iConditionBitFlag: self.get_condition_bit_flag(),
            eCSTB___Add: placeholder!(0),
            TimeBuff: v1013::sTimeBuff {
                iTimeLimit: placeholder!(0),
                iTimeDuration: placeholder!(0),
                iTimeRepeat: placeholder!(0),
                iValue: placeholder!(0),
                iConfirmNum: placeholder!(0),
            },
            aQuestFlag: self
                .mission_journal
                .completed_mission_flags
                .to_array()
                .unwrap(),
            aRepeatQuestFlag: unused!(),
            aRunningQuest: self.mission_journal.get_running_quests(),
            iCurrentMissionID: self.mission_journal.get_active_mission_id().unwrap_or(0),
            iWarpLocationFlag: self.flags.scamper_flags.get_chunk(0).unwrap(),
            aWyvernLocationFlag: self.flags.skyway_flags.to_array().unwrap(),
            iBuddyWarpTime: self
                .buddy_warp_available_at
                .map_or(0, |available_at| available_at as i32),
            iFatigue: unused!(),
            iFatigue_Level: unused!(),
            iFatigueRate: unused!(),
            iFirstUseFlag1: self.flags.tip_flags.get_chunk(0).unwrap(),
            iFirstUseFlag2: self.flags.tip_flags.get_chunk(1).unwrap(),
            aiPCSkill: [unused!(); 33],
        }
    }

    pub fn get_regen_data(&self) -> (sPCRegenData, sPCRegenDataForOtherPC) {
        let regen_data = sPCRegenData {
            iHP: self.hp,
            iMapNum: self.instance_id.map_num as i32,
            iX: self.position.x,
            iY: self.position.y,
            iZ: self.position.z,
            iActiveNanoSlotNum: match self.get_active_nano_slot() {
                Some(slot) => slot as i16,
                None => -1,
            },
            Nanos: self.nano_data.as_carried(),
        };
        let regen_data_other = sPCRegenDataForOtherPC {
            iPC_ID: self.id.unwrap(),
            iHP: self.hp,
            iX: self.position.x,
            iY: self.position.y,
            iZ: self.position.z,
            iAngle: 0,
            iConditionBitFlag: self.get_condition_bit_flag(),
            iPCState: self.get_state_bit_flag(),
            iSpecialState: self.get_special_state_bit_flag(),
            Nano: self.get_active_nano().into_proto(),
        };
        (regen_data, regen_data_other)
    }

    pub fn get_group_member_info(&self) -> sPCGroupMemberInfo {
        sPCGroupMemberInfo {
            iPC_ID: self.get_player_id(),
            iPCUID: self.uid as u64,
            iNameCheck: self.flags.name_check as i8,
            szFirstName: util::encode_utf16(&self.first_name).unwrap(),
            szLastName: util::encode_utf16(&self.last_name).unwrap(),
            iSpecialState: self.get_special_state_bit_flag(),
            iLv: self.level,
            iHP: self.hp,
            iMaxHP: self.get_max_hp(),
            iMapType: unused!(),
            iMapNum: self.instance_id.map_num as i32,
            iX: self.position.x,
            iY: self.position.y,
            iZ: self.position.z,
            bNano: match self.nano_data.active_slot {
                Some(_) => 1,
                None => 0,
            },
            Nano: self.get_active_nano().into_proto(),
        }
    }

    pub fn get_state_bit_flag(&self) -> i8 {
        let mut flags = 0;
        if self.vehicle_speed.is_some() {
            flags |= FLAG_PC_STATE_VEHICLE;
        }
        flags
    }

    pub fn get_special_state_bit_flag(&self) -> i8 {
        let mut flags = 0;
        if self.show_gm_marker {
            flags |= CN_SPECIAL_STATE_FLAG__PRINT_GM;
        }
        if self.invulnerable {
            flags |= CN_SPECIAL_STATE_FLAG__INVULNERABLE;
        }
        if self.invisible {
            flags |= CN_SPECIAL_STATE_FLAG__INVISIBLE;
        }
        if self.in_menu {
            flags |= CN_SPECIAL_STATE_FLAG__FULL_UI;
        }
        if self.in_combat {
            flags |= CN_SPECIAL_STATE_FLAG__COMBAT;
        }
        if self.freechat_muted {
            flags |= CN_SPECIAL_STATE_FLAG__MUTE_FREECHAT;
        }
        flags as i8
    }

    pub fn get_appearance_data_104(&self) -> v0104::sPCAppearanceData {
        v0104::sPCAppearanceData {
            iID: self.id.unwrap_or_default(),
            PCStyle: self.get_style(),
            iConditionBitFlag: self.get_condition_bit_flag(),
            iPCState: self.get_state_bit_flag(),
            iSpecialState: self.get_special_state_bit_flag(),
            iLv: self.level,
            iHP: self.hp,
            iMapNum: self.get_mapnum() as i32,
            iX: self.position.x,
            iY: self.position.y,
            iZ: self.position.z,
            iAngle: self.rotation,
            ItemEquip: self.get_equip_arr_104(),
            Nano: self.get_active_nano().into_proto(),
            eRT: unused!(),
        }
    }

    pub fn get_appearance_data_1013(&self) -> v1013::sPCAppearanceData {
        v1013::sPCAppearanceData {
            iID: self.id.unwrap_or_default(),
            PCStyle: self.get_style(),
            iConditionBitFlag: self.get_condition_bit_flag(),
            iPCState: self.get_state_bit_flag(),
            iSpecialState: self.get_special_state_bit_flag(),
            iLv: self.level,
            iHP: self.hp,
            iMapNum: self.get_mapnum() as i32,
            iX: self.position.x,
            iY: self.position.y,
            iZ: self.position.z,
            iAngle: self.rotation,
            ItemEquip: self.get_equip_arr_1013(),
            Nano: self.get_active_nano().into_proto(),
            eRT: unused!(),
        }
    }

    pub fn get_item(&self, location: ItemLocation, slot_num: usize) -> FFResult<&Option<Item>> {
        let err = Err(FFError::build(
            Severity::Warning,
            format!("Bad slot number: {slot_num} (location {:?})", location),
        ));
        let protocol = config_get().general.protocol.get();
        match location {
            ItemLocation::Equip => {
                if slot_num < sizeof_equip_slot(&protocol) {
                    Ok(&self.inventory.equipped[slot_num])
                } else {
                    err
                }
            }
            ItemLocation::Inven => {
                if slot_num < SIZEOF_INVEN_SLOT as usize {
                    Ok(&self.inventory.main[slot_num])
                } else {
                    err
                }
            }
            ItemLocation::QInven => unimplemented!("Quest items not accessible by slot number"),
            ItemLocation::Bank => {
                if slot_num < sizeof_bank_slot(&protocol) {
                    Ok(&self.inventory.bank[slot_num])
                } else {
                    err
                }
            }
        }
    }

    pub fn get_item_mut(
        &mut self,
        location: ItemLocation,
        slot_num: usize,
    ) -> FFResult<&mut Option<Item>> {
        let err_oob = Err(FFError::build(
            Severity::Warning,
            format!("Bad slot number: {slot_num} (location {:?})", location),
        ));
        let err_trading = Err(FFError::build(
            Severity::Warning,
            format!(
                "Can't mutate inventory. Player {} trading",
                self.id.unwrap_or_default()
            ),
        ));
        let protocol = config_get().general.protocol.get();

        let res = match location {
            ItemLocation::Equip => {
                if slot_num < sizeof_equip_slot(&protocol) {
                    Ok(&mut self.inventory.equipped[slot_num])
                } else {
                    err_oob
                }
            }
            ItemLocation::Inven => {
                if slot_num < SIZEOF_INVEN_SLOT as usize {
                    Ok(&mut self.inventory.main[slot_num])
                } else {
                    err_oob
                }
            }
            ItemLocation::QInven => unimplemented!("Quest items not accessible by slot number"),
            ItemLocation::Bank => {
                if slot_num < sizeof_bank_slot(&protocol) {
                    Ok(&mut self.inventory.bank[slot_num])
                } else {
                    err_oob
                }
            }
        };

        if res.as_ref().is_ok_and(|v| v.is_some()) && self.trade_id.is_some() {
            return err_trading;
        }

        res
    }

    pub fn set_item(
        &mut self,
        location: ItemLocation,
        slot_num: usize,
        item: Option<Item>,
    ) -> FFResult<Option<Item>> {
        let slot_from = self.get_item_mut(location, slot_num)?;
        let old_item = slot_from.take();
        *slot_from = item;
        Ok(old_item)
    }

    pub fn get_quest_item_count(&self, item_id: i16) -> usize {
        self.inventory
            .quest
            .iter()
            .flatten()
            .find(|(qitem_id, _)| *qitem_id == item_id)
            .map(|(_, count)| *count)
            .unwrap_or(0)
    }

    pub fn set_quest_item_count(&mut self, item_id: i16, count: usize) -> FFResult<usize> {
        let new_qitem = if count == 0 {
            None
        } else {
            Some((item_id, count))
        };
        for (idx, slot) in self.inventory.quest.iter_mut().enumerate() {
            if let Some((qitem_id, _)) = slot {
                if *qitem_id == item_id {
                    *slot = new_qitem;
                    return Ok(idx);
                }
            } else {
                *slot = new_qitem;
                return Ok(idx);
            }
        }
        Err(FFError::build(
            Severity::Warning,
            format!(
                "No free quest item slots for player {}",
                self.get_player_id()
            ),
        ))
    }

    pub fn get_free_slots(&self, location: ItemLocation) -> usize {
        match location {
            ItemLocation::Equip => self
                .inventory
                .equipped
                .iter()
                .filter(|slot| slot.is_none())
                .count(),
            ItemLocation::Inven => self
                .inventory
                .main
                .iter()
                .filter(|slot| slot.is_none())
                .count(),
            ItemLocation::QInven => self
                .inventory
                .quest
                .iter()
                .filter(|slot| slot.is_none())
                .count(),
            ItemLocation::Bank => self
                .inventory
                .bank
                .iter()
                .filter(|slot| slot.is_none())
                .count(),
        }
    }

    pub fn find_free_slot(&self, location: ItemLocation) -> FFResult<usize> {
        let inven = match location {
            ItemLocation::Equip => self.inventory.equipped.as_slice(),
            ItemLocation::Inven => self.inventory.main.as_slice(),
            ItemLocation::QInven => unimplemented!("Quest item inventory not searchable"),
            ItemLocation::Bank => self.inventory.bank.as_slice(),
        };

        for (slot_num, slot) in inven.iter().enumerate() {
            if slot.is_none() {
                return Ok(slot_num);
            }
        }
        Err(FFError::build(
            Severity::Warning,
            format!(
                "Player {} has no free slots in {:?}",
                self.get_player_id(),
                location
            ),
        ))
    }

    pub fn find_items_any(&self, f: impl Fn(&Item) -> bool) -> Vec<(ItemLocation, usize)> {
        let mut found = Vec::new();
        found.extend(
            self.find_items(ItemLocation::Equip, &f)
                .iter()
                .map(|slot_num| (ItemLocation::Equip, *slot_num)),
        );
        found.extend(
            self.find_items(ItemLocation::Inven, &f)
                .iter()
                .map(|slot_num| (ItemLocation::Inven, *slot_num)),
        );
        found.extend(
            self.find_items(ItemLocation::Bank, &f)
                .iter()
                .map(|slot_num| (ItemLocation::Bank, *slot_num)),
        );
        found
    }

    pub fn find_items(&self, location: ItemLocation, f: impl Fn(&Item) -> bool) -> Vec<usize> {
        let inven = match location {
            ItemLocation::Equip => self.inventory.equipped.as_slice(),
            ItemLocation::Inven => self.inventory.main.as_slice(),
            ItemLocation::QInven => unimplemented!("Quest item inventory not searchable"),
            ItemLocation::Bank => self.inventory.bank.as_slice(),
        };

        inven
            .iter()
            .enumerate()
            .filter_map(|(slot_num, slot)| {
                if let Some(item) = slot {
                    if f(item) {
                        Some(slot_num)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn get_item_iter(&self) -> impl Iterator<Item = (usize, &Item)> {
        let protocol = config_get().general.protocol.get();
        let inv_slot_max =
            sizeof_equip_slot(&protocol) + SIZEOF_INVEN_SLOT as usize + sizeof_bank_slot(&protocol);
        (0..inv_slot_max).filter_map(move |slot_num| {
            let (loc, slot_num_loc) = util::slot_num_to_loc_and_slot_num(slot_num).unwrap();
            let item = self.get_item(loc, slot_num_loc).unwrap();
            item.as_ref().map(|item| (slot_num, item))
        })
    }

    pub fn get_quest_item_iter(&self) -> impl Iterator<Item = (i16, usize)> + '_ {
        self.inventory
            .quest
            .iter()
            .flatten()
            .map(|(id, count)| (*id, *count))
    }

    pub fn get_equipped(&self) -> &[Option<Item>; MAX_SIZEOF_EQUIP_SLOT] {
        &self.inventory.equipped
    }

    pub fn get_equip_arr_104<const N: usize>(&self) -> [v0104::sItemBase; N] {
        std::array::from_fn(|i| {
            self.inventory
                .equipped
                .get(i)
                .copied()
                .flatten()
                .into_proto()
        })
    }

    pub fn get_equip_arr_1013<const N: usize>(&self) -> [v1013::sItemBase; N] {
        std::array::from_fn(|i| {
            self.inventory
                .equipped
                .get(i)
                .copied()
                .flatten()
                .into_proto()
        })
    }

    pub fn get_taros(&self) -> u32 {
        self.taros
    }

    pub fn get_fusion_matter(&self) -> u32 {
        self.fusion_matter
    }

    pub fn update_first_use_flag(&mut self, num: i32) -> FFResult<()> {
        self.flags.tip_flags.set((num - 1) as usize, true)?;
        Ok(())
    }

    pub fn unlock_scamper_location(&mut self, location_id: i32) -> FFResult<()> {
        self.flags
            .scamper_flags
            .set((location_id - 1) as usize, true)?;
        Ok(())
    }

    pub fn is_scamper_location_unlocked(&self, location_id: i32) -> FFResult<bool> {
        self.flags.scamper_flags.get((location_id - 1) as usize)
    }

    pub fn unlock_skyway_location(&mut self, location_id: i32) -> FFResult<()> {
        self.flags
            .skyway_flags
            .set((location_id - 1) as usize, true)?;
        Ok(())
    }

    pub fn is_skyway_location_unlocked(&self, location_id: i32) -> FFResult<bool> {
        self.flags.skyway_flags.get((location_id - 1) as usize)
    }

    pub fn set_tutorial_done(&mut self) {
        self.flags.tutorial_flag = true;
        let protocol = config_get().general.protocol.get();
        if let FFProtocol::v0104 = protocol {
            // unlock buttercup
            let buttercup_stats = tdata_get().get_nano_stats(ID_BUTTERCUP).unwrap();
            self.unlock_nano(ID_BUTTERCUP).unwrap();
            self.tune_nano(ID_BUTTERCUP, Some(buttercup_stats.skills[0]))
                .unwrap();
            self.change_nano(0, Some(ID_BUTTERCUP)).unwrap();
            // equip lightning gun
            self.set_item(
                ItemLocation::Equip,
                EQUIP_SLOT_HAND as usize,
                Some(Item::new(ItemType::Hand, ID_LIGHTNING_GUN)),
            )
            .unwrap();
        }

        let (spawn_x, spawn_y, spawn_z) = match protocol {
            FFProtocol::v0104 => (632032, 187177, -5500), // Sector V future
            FFProtocol::v1013 => (19835, 108682, 8450),   // Null Void
        };

        let mut rand = rand::thread_rng();
        let range = 0; //PC_START_LOCATION_RANDOM_RANGE as i32 / 2;
        self.position = Position {
            x: spawn_x + rand.gen_range(-range..=range),
            y: spawn_y + rand.gen_range(-range..=range),
            z: spawn_z,
        }
    }

    pub fn get_guide(&self) -> PlayerGuide {
        self.guide_data.current_guide
    }

    pub fn update_guide(&mut self, guide: PlayerGuide) -> usize {
        self.guide_data.current_guide = guide;
        self.guide_data.total_guides =
            clamp_max(self.guide_data.total_guides + 1, i16::MAX as usize);
        self.guide_data.total_guides
    }

    pub fn set_future_done(&mut self) {
        self.flags.payzone_flag = true;
    }

    pub fn is_future_done(&self) -> bool {
        self.get_payzone_flag()
    }

    pub fn set_taros(&mut self, taros: u32) -> u32 {
        self.taros = clamp(taros, 0, PC_CANDY_MAX);
        self.taros
    }

    pub fn set_hp(&mut self, hp: i32) -> i32 {
        let hp_max = if self.perms <= CN_ACCOUNT_LEVEL__DEVELOPER as i16 {
            i32::MAX // allow overflow for high perms
        } else {
            self.get_max_hp()
        };
        self.hp = clamp(hp, 0, hp_max);
        self.hp
    }

    pub fn set_level(&mut self, level: i16) -> FFResult<i16> {
        let new_level = clamp(level, 1, PC_LEVEL_MAX as i16);

        tdata_get().get_player_stats(new_level)?; // validate

        self.level = new_level;
        Ok(self.level)
    }

    pub fn set_fusion_matter(&mut self, fusion_matter: u32) -> u32 {
        let player_stats = tdata_get().get_player_stats(self.level).unwrap();
        let fm_max = if self.perms <= CN_ACCOUNT_LEVEL__DEVELOPER as i16 {
            PC_FUSIONMATTER_MAX
        } else {
            player_stats.fm_limit
        };

        self.fusion_matter = clamp(fusion_matter, 0, fm_max);

        let level_up_fusion_matter = player_stats.req_fm_nano_create;
        let Some(level_up_task_id) = player_stats.nano_quest_task_id else {
            // no level up task
            return self.fusion_matter;
        };

        if let Some(client) = self.get_client() {
            if self.fusion_matter >= level_up_fusion_matter {
                match config_get().general.protocol.get() {
                    FFProtocol::v0104 => {
                        // Blossom's nano mission is withheld until the player reaches the Past
                        if self.level >= 4 && !self.is_future_done() {
                            return self.fusion_matter;
                        }

                        if !self.mission_journal.has_nano_mission() {
                            let Ok(level_up_task_def) =
                                tdata_get().get_task_definition(level_up_task_id)
                            else {
                                log(
                                    Severity::Warning,
                                    &format!(
                                        "Level up task with ID {} doesn't exist!",
                                        level_up_task_id
                                    ),
                                );
                                return self.fusion_matter;
                            };

                            let level_up_mission_def = tdata_get()
                                .get_mission_definition(level_up_task_def.mission_id)
                                .unwrap();

                            log(
                                Severity::Info,
                                &format!(
                                    "{} started nano mission: {} [{}]",
                                    self,
                                    level_up_mission_def.mission_name,
                                    level_up_mission_def.mission_id
                                ),
                            );

                            self.mission_journal
                                .start_task(level_up_task_def.into(), self.level)
                                .unwrap();

                            let pkt = sP_FE2CL_REP_PC_TASK_START_SUCC {
                                iTaskNum: level_up_task_id,
                                iRemainTime: level_up_task_def
                                    .obj_time_limit
                                    .map(|d| d.as_secs() as i32)
                                    .unwrap_or(unused!()),
                            };

                            client.send_packet(P_FE2CL_REP_PC_TASK_START_SUCC, &pkt);
                        }
                    }
                    FFProtocol::v1013 => {
                        if self.set_level(self.level + 1).is_ok() {
                            self.fusion_matter -= level_up_fusion_matter;
                            let pkt = sP_FE2CL_REP_PC_CHANGE_LEVEL_SUCC {
                                iLevel: self.level as i32,
                                iFusionMatter: self.fusion_matter as i32,
                            };

                            client.send_packet(P_FE2CL_REP_PC_CHANGE_LEVEL_SUCC, &pkt);
                        }
                    }
                }
            }
        }

        self.fusion_matter
    }

    pub fn get_weapon_boosts(&self) -> u32 {
        self.weapon_boosts
    }

    pub fn get_nano_potions(&self) -> u32 {
        self.nano_potions
    }

    pub fn set_weapon_boosts(&mut self, weapon_boosts: u32) -> u32 {
        self.weapon_boosts = clamp(weapon_boosts, 0, PC_BATTERY_MAX);
        self.weapon_boosts
    }

    pub fn set_nano_potions(&mut self, nano_potions: u32) -> u32 {
        self.nano_potions = clamp(nano_potions, 0, PC_BATTERY_MAX);
        self.nano_potions
    }

    pub fn set_pre_warp(&mut self) {
        // we only save pre-warp when we're not in an instance
        if self.instance_id.instance_num.is_none() {
            self.pre_warp_data = PreWarpData {
                instance_id: self.instance_id,
                position: self.position,
            }
        }
    }

    pub fn get_pre_warp(&self) -> &PreWarpData {
        &self.pre_warp_data
    }

    pub fn start_skyway_ride(&mut self, trip_data: &'static TripData, mut path: Path) {
        path.start();
        self.skyway_ride = Some(SkywayRideState {
            trip_data,
            path,
            monkey_pos: self.position,
            resume_time: SystemTime::now(),
        });
    }

    pub fn get_skyway_ride(&self) -> Option<&SkywayRideState> {
        self.skyway_ride.as_ref()
    }

    pub fn do_revive(&mut self) {
        self.hp = self.get_max_hp() / 2;
        for nano_id in self.nano_data.equipped_ids.into_iter().flatten() {
            self.get_nano_mut(nano_id)
                .unwrap()
                .set_stamina(NANO_STAMINA_MAX / 2);
        }
        self.reset();
    }

    pub fn is_buddies_with(&self, pc_uid: i64) -> bool {
        self.buddy_list.is_buddies_with(pc_uid)
    }

    pub fn add_buddy(&mut self, buddy_info: BuddyListEntry) -> FFResult<usize> {
        self.buddy_list.insert_buddy(buddy_info)
    }

    pub fn remove_buddy(&mut self, pc_uid: i64) -> FFResult<usize> {
        self.buddy_list.erase_buddy(pc_uid)
    }

    pub fn block_player(&mut self, pc_uid: i64) -> FFResult<usize> {
        self.buddy_list.block_buddy(pc_uid)
    }

    pub fn get_num_buddies(&self) -> usize {
        self.buddy_list.get_num_buddies()
    }

    pub fn get_all_buddy_info(&self) -> Vec<BuddyListEntry> {
        self.buddy_list.get_all_entries()
    }

    pub fn get_buddy_uids(&self) -> Vec<i64> {
        self.get_all_buddy_info()
            .iter()
            .filter_map(|b| if !b.blocked { Some(b.pc_uid) } else { None })
            .collect()
    }

    pub fn get_blocked_uids(&self) -> Vec<i64> {
        self.get_all_buddy_info()
            .iter()
            .filter_map(|b| if b.blocked { Some(b.pc_uid) } else { None })
            .collect()
    }

    pub fn get_payzone_flag(&self) -> bool {
        self.flags.payzone_flag
    }

    pub fn is_warp_on_cooldown(&self) -> bool {
        self.buddy_warp_available_at
            .is_some_and(|available_at| util::get_timestamp_sec(SystemTime::now()) < available_at)
    }

    pub fn remove_from_state(pc_id: i32, state: &mut ShardServerState) -> Player {
        let player = state.get_player(pc_id).unwrap();
        log(
            Severity::Info,
            &format!(
                "{} left (channel {})",
                player, player.instance_id.channel_num
            ),
        );

        let uid = player.get_uid();
        let player_snapshot = player.clone();

        state.player_uid_to_id.remove(&uid);

        let id = EntityID::Player(pc_id);
        let entity_map = &mut state.entity_map;
        entity_map.update(id, None, true);
        let player = entity_map.untrack(id);
        player.cleanup(state);
        player_snapshot
    }

    fn tick_skyway_ride(
        state: &mut ShardServerState,
        pc_id: i32,
        time: &SystemTime,
        mut ride: SkywayRideState,
    ) -> Option<SkywayRideState> {
        if &ride.resume_time > time {
            return Some(ride);
        }

        if ride.path.is_done() {
            // we're done!
            let final_pos = ride.monkey_pos;
            let cost = ride.trip_data.cost;
            let player = state.get_player_mut(pc_id).unwrap();
            player.set_taros(player.get_taros() - cost);
            player.set_position(final_pos);
            helpers::broadcast_monkey(pc_id, RideType::None, state);
            return None;
        }

        // N.B. the client doesn't treat monkey movement like every other movement.
        // instead of using the speed value from the packet, it uses the distance between the
        // current position and the target position. so we can only send move packets once we've
        // covered about the same distance as the speed.
        // 100% causes the client to go too fast and pause, but 80% seems to work fine.
        const SPEED_TO_DISTANCE_FACTOR: f32 = 1.0;

        // tick the path until we've covered the same distance as the speed
        let speed = ride.path.get_speed() as u32;
        let distance_to_cover = (speed as f32 * SPEED_TO_DISTANCE_FACTOR) as u32;
        let mut distance = 0;
        while distance < distance_to_cover {
            let old_pos = ride.monkey_pos;
            ride.path.tick(&mut ride.monkey_pos);
            distance += old_pos.distance_to(&ride.monkey_pos);
            if ride.path.is_done() {
                break;
            }
        }

        // update the player's chunk.
        // We don't actually update their position until they land
        let player = state.get_player(pc_id).unwrap();
        let player_eid = player.get_id();
        let chunk_coords = ChunkCoords::from_pos_inst(ride.monkey_pos, player.instance_id);
        state
            .entity_map
            .update(player_eid, Some(chunk_coords), true);

        // send the move packet
        let pkt = sP_FE2CL_PC_BROOMSTICK_MOVE {
            iPC_ID: pc_id,
            iToX: ride.monkey_pos.x,
            iToY: ride.monkey_pos.y,
            iToZ: ride.monkey_pos.z,
            iSpeed: unused!(),
        };

        state.entity_map.for_each_around(player_eid, |c| {
            c.send_packet(PacketID::P_FE2CL_PC_BROOMSTICK_MOVE, &pkt)
        });

        // wait for the client to catch up. in theory, takes one second.
        ride.resume_time = *time + Duration::from_secs(1);
        Some(ride)
    }

    fn check_task_failure(
        state: &ShardServerState,
        player: &Player,
        task: &Task,
        task_def: &TaskDefinition,
        time: &SystemTime,
    ) -> Option<codes::TaskEndErr> {
        if task_def.obj_time_limit.is_some() {
            match task.fail_time {
                Some(fail_time) => {
                    if time > &fail_time {
                        return Some(codes::TaskEndErr::TimeLimitExceeded);
                    }
                }
                None => {
                    // user re-logged; auto-fail
                    return Some(codes::TaskEndErr::TimeLimitExceeded);
                }
            }
        }

        if let Some(req_map_num) = task_def.prereq_map_num {
            if player.get_mapnum() != req_map_num {
                return Some(codes::TaskEndErr::InstanceLeft);
            }
        }

        if let Some(escort_npc_id) = task.escort_npc_id {
            if let Ok(escort_npc) = state.get_npc(escort_npc_id) {
                if escort_npc.is_dead() {
                    return Some(codes::TaskEndErr::EscortFailed);
                }
            } else {
                return Some(codes::TaskEndErr::EscortFailed);
            }
        }

        None
    }

    fn check_task_repair(player: &Player, task: &Task, task_def: &TaskDefinition) -> Option<i16> {
        // There are rare cases where the clients qitem state gets corrupted, usually due to XDT bugs
        // (e.g. tasks that don't clean up quest items properly due to a missing iDelItemID entry).
        // We can attempt to fix this by checking if the player has the required quest items for the
        // current task and, if so, re-sending one to force a completion request out of the client.
        // This check is pretty strict to avoid false positives.

        if task.pending_repair {
            // already sent repair packet
            return None;
        }

        if task_def.task_type != TaskType::Defeat {
            // only "get X item from Y mob" tasks have this issue
            return None;
        }

        for (qitem_id, req_count) in &task_def.obj_qitems {
            if player.get_quest_item_count(*qitem_id) < *req_count {
                return None;
            }
        }

        let (qitem_id, _) = task_def.dropped_qitems.iter().next()?;
        Some(*qitem_id)
    }

    fn tick_missions(state: &mut ShardServerState, pc_id: i32, time: &SystemTime) {
        let player = state.get_player(pc_id).unwrap();
        let client = player.get_client().unwrap();
        let active_tasks = player.mission_journal.get_current_tasks();
        for task in active_tasks {
            let task_def = tdata_get().get_task_definition(task.get_task_id()).unwrap();

            // check for task failure
            let fail_code = {
                let player = state.get_player(pc_id).unwrap();
                Self::check_task_failure(state, player, &task, task_def, time)
            };
            if let Some(fail_code) = fail_code {
                let player = state.get_player_mut(pc_id).unwrap();
                player
                    .mission_journal
                    .fail_task(task.get_task_id())
                    .unwrap();

                // failure qitem changes
                if !task_def.fail_qitems.is_empty() {
                    let qitem_pkt = sP_FE2CL_REP_REWARD_ITEM {
                        m_iCandy: player.get_taros() as i32,
                        m_iFusionMatter: player.get_fusion_matter() as i32,
                        m_iBatteryN: player.get_nano_potions() as i32,
                        m_iBatteryW: player.get_weapon_boosts() as i32,
                        iItemCnt: task_def.fail_qitems.len() as i8,
                        iFatigue: 100,
                        iFatigue_Level: 1,
                        iNPC_TypeID: 0,
                        iTaskID: task_def.task_id,
                    };

                    let mut pkt = PacketBuilder::new(P_FE2CL_REP_REWARD_ITEM).with(&qitem_pkt);

                    for (qitem_id, qitem_count_mod) in &task_def.succ_qitems {
                        let curr_count = player.get_quest_item_count(*qitem_id) as isize;
                        let new_count = (curr_count + *qitem_count_mod) as usize;
                        let qitem_slot = player.set_quest_item_count(*qitem_id, new_count).unwrap();
                        let qitem_reward = RewardItem::Quest {
                            id: *qitem_id,
                            count: new_count,
                            slot_num: qitem_slot,
                        };
                        helpers::push_item_reward(&mut pkt, qitem_reward);
                    }

                    if let Some(pkt) = log_if_failed(pkt.build()) {
                        client.send_payload(pkt);
                    }
                }

                let pkt = sP_FE2CL_REP_PC_TASK_END_FAIL {
                    iTaskNum: task.get_task_id(),
                    iErrorCode: fail_code as i32,
                };
                client.send_packet(P_FE2CL_REP_PC_TASK_END_FAIL, &pkt);
                continue;
            }

            // check for repair
            let repair_qitem_id = {
                let player = state.get_player(pc_id).unwrap();
                Self::check_task_repair(player, &task, task_def)
            };
            if let Some(repair_qitem_id) = repair_qitem_id {
                log(
                    Severity::Warning,
                    &format!("Detected desync on task {}; repairing...", task_def.task_id),
                );
                let player = state.get_player_mut(pc_id).unwrap();
                player
                    .mission_journal
                    .repair_task(task_def.task_id)
                    .unwrap();

                let mut reward_pkt =
                    PacketBuilder::new(P_FE2CL_REP_REWARD_ITEM).with(&sP_FE2CL_REP_REWARD_ITEM {
                        m_iCandy: player.get_taros() as i32,
                        m_iFusionMatter: player.get_fusion_matter() as i32,
                        m_iBatteryN: player.get_nano_potions() as i32,
                        m_iBatteryW: player.get_weapon_boosts() as i32,
                        iItemCnt: 1,
                        iFatigue: 100,
                        iFatigue_Level: 1,
                        iNPC_TypeID: 0,
                        iTaskID: task_def.task_id,
                    });

                let qitem_amt = player.get_quest_item_count(repair_qitem_id);
                let qitem_slot = player
                    .set_quest_item_count(repair_qitem_id, qitem_amt)
                    .unwrap(); // no-op

                helpers::push_item_reward(
                    &mut reward_pkt,
                    RewardItem::Quest {
                        id: repair_qitem_id,
                        count: qitem_amt,
                        slot_num: qitem_slot,
                    },
                );

                if let Some(reward_pkt) = log_if_failed(reward_pkt.build()) {
                    client.send_payload(reward_pkt);
                }
            }
        }
    }

    fn tick_nanos(&mut self) -> bool {
        let mut changed = false;

        let active_nano_id = self.get_active_nano().map(|n| n.get_id());
        let equipped_nano_ids = self.nano_data.equipped_ids;
        for nano_id in equipped_nano_ids.into_iter().flatten() {
            if active_nano_id.is_some_and(|id| id == nano_id) {
                // don't regen active nano
                continue;
            }

            let nano = self.get_nano_mut(nano_id).unwrap();
            changed |= nano.tick_regen();
        }

        if let Some(nano) = self.get_active_nano_mut() {
            let level = placeholder!(1);
            changed |= nano.tick_wear(level);
        }

        changed
    }

    fn tick_regen(&mut self, time: &SystemTime) -> bool {
        const REGEN_INTERVAL: Duration = Duration::from_secs(4);

        if self.in_combat {
            return false;
        }

        if self.hp >= self.get_max_hp() {
            return false;
        }

        if self
            .last_heal_time
            .is_some_and(|t| time.duration_since(t).unwrap_or_default() < REGEN_INTERVAL)
        {
            return false;
        }

        let max_hp = self.get_max_hp();
        let heal_amt = max_hp / 5;
        self.hp = clamp_max(self.hp + heal_amt, max_hp);
        self.last_heal_time = Some(*time);
        true
    }

    pub fn tick(state: &mut ShardServerState, pc_id: i32, time: &SystemTime) {
        let player_eid = EntityID::Player(pc_id);
        if state.get_player(pc_id).unwrap().is_dead() {
            return;
        }

        if let Some(ride) = state.get_player_mut(pc_id).unwrap().skyway_ride.take() {
            let ride = Self::tick_skyway_ride(state, pc_id, time, ride);
            state.get_player_mut(pc_id).unwrap().skyway_ride = ride;
        }

        Self::tick_missions(state, pc_id, time);

        let mut pending_buff_effects = std::mem::take(&mut state.pending_buff_effects);
        let buff_updates = state
            .get_player_mut(pc_id)
            .unwrap()
            .buffs
            .tick(player_eid, &mut pending_buff_effects);
        state.pending_buff_effects = pending_buff_effects;

        let player = state.get_player_mut(pc_id).unwrap();
        let condition_bit_flag = player.buffs.get_bit_flags();
        if let Some(client) = player.get_client() {
            for update in buff_updates {
                let mut pkt: v0104::sP_FE2CL_PC_BUFF_UPDATE = update.into();
                pkt.iConditionBitFlag = condition_bit_flag;
                client.send_packet(P_FE2CL_PC_BUFF_UPDATE, &pkt);
            }
        }

        let mut transmit = false;
        transmit |= player.tick_regen(time);
        transmit |= player.tick_nanos();

        if player
            .get_active_nano()
            .is_some_and(|n| n.get_stamina() == 0)
        {
            // nano is exhausted.
            player.deactivate_nano();

            // PC_TICK will handle the clientside deactivation for the owning player,
            // but we still need to broadcast to other players.
            let pkt = sP_FE2CL_NANO_ACTIVE {
                iPC_ID: pc_id,
                Nano: None.into_proto(),
                iConditionBitFlag: condition_bit_flag,
                eCSTB___Add: false as i32,
            };

            state.entity_map.for_each_around(player_eid, |c| {
                c.send_packet(P_FE2CL_NANO_ACTIVE, &pkt);
            });

            transmit = true; // just in case
        }

        if !transmit {
            return;
        }

        let player = state.get_player(pc_id).unwrap(); // re-borrow
        let pkt = sP_FE2CL_REP_PC_TICK {
            iHP: player.hp,
            aNano: player.nano_data.as_carried(),
            iBatteryN: player.nano_potions as i32,
            bResetMissionFlag: unused!(),
        };

        player
            .get_client()
            .unwrap()
            .send_packet(P_FE2CL_REP_PC_TICK, &pkt);
    }
}
impl Combatant for Player {
    fn get_condition_bit_flag(&self) -> i32 {
        self.buffs.get_bit_flags()
    }

    fn get_group_id(&self) -> Option<Uuid> {
        self.group_id
    }

    fn get_level(&self) -> i16 {
        self.level
    }

    fn get_hp(&self) -> i32 {
        self.hp
    }

    fn get_max_hp(&self) -> i32 {
        tdata_get().get_player_stats(self.level).unwrap().max_hp as i32
    }

    fn get_style(&self) -> Option<CombatStyle> {
        self.get_active_nano().map(|n| n.get_stats().unwrap().style)
    }

    fn get_team(&self) -> CombatantTeam {
        CombatantTeam::Friendly
    }

    fn get_char_type(&self) -> CharType {
        CharType::Player
    }

    fn get_aggro_factor(&self) -> f32 {
        if self.invisible {
            0.0
        } else {
            // TODO check for sneak or active IZ race
            1.0
        }
    }

    fn get_target(&self) -> Option<EntityID> {
        self.target.or(self.last_attacked_by)
    }

    fn is_dead(&self) -> bool {
        self.hp <= 0
    }

    fn has_buff(&self, buff_id: BuffID, buff_type: Option<BuffType>) -> bool {
        self.buffs.has_buff(buff_id, buff_type)
    }

    fn get_single_power(&self) -> i32 {
        let base_power = self.level as i32 * 2 + 8;
        let weapon = self
            .get_item(ItemLocation::Equip, EQUIP_SLOT_HAND as usize)
            .unwrap();
        base_power
            + match weapon {
                Some(weapon) => weapon.get_stats().unwrap().single_power.unwrap_or(0),
                None => 0,
            }
    }

    fn get_multi_power(&self) -> i32 {
        let base_power = self.level as i32 * 2 + 8;
        let weapon = self
            .get_item(ItemLocation::Equip, EQUIP_SLOT_HAND as usize)
            .unwrap();
        base_power
            + match weapon {
                Some(weapon) => weapon.get_stats().unwrap().multi_power.unwrap_or(0),
                None => 0,
            }
    }

    fn get_defense(&self) -> i32 {
        let base_defense = self.level as i32 * 2 + 16;
        let mut total_from_armor = 0;
        for item in self.get_equipped().iter().flatten() {
            total_from_armor += item.get_stats().unwrap().defense.unwrap_or(0);
        }
        base_defense + total_from_armor
    }

    fn take_damage(&mut self, damage: i32, source: Option<EntityID>) -> i32 {
        if self.invulnerable {
            return 0;
        }

        if let Some(source) = source {
            self.last_attacked_by = Some(source);
        }

        let init_hp = self.hp;
        self.hp = clamp_min(self.hp - damage, 0);
        init_hp - self.hp
    }

    fn heal(&mut self, amount: i32) -> i32 {
        let init_hp = self.hp;
        self.hp = clamp_max(self.hp + amount, self.get_max_hp());
        self.hp - init_hp
    }

    fn apply_buff(
        &mut self,
        buff_id: BuffID,
        buff: BuffInstance,
        source: Option<EntityID>,
    ) -> bool {
        self.buffs.add_buff(buff_id, buff, source)
    }

    fn remove_buff(&mut self, buff_id: BuffID, buff_type: Option<BuffType>) -> bool {
        self.buffs.remove_buff(buff_id, buff_type)
    }

    fn reset(&mut self) {
        self.target = None;
        self.last_attacked_by = None;
        self.last_heal_time = Some(SystemTime::now());
    }
}
impl Entity for Player {
    fn get_client(&self) -> Option<FFClient> {
        self.client.clone()
    }

    fn get_id(&self) -> EntityID {
        EntityID::Player(self.get_player_id())
    }

    fn get_position(&self) -> Position {
        self.position
    }

    fn get_rotation(&self) -> i32 {
        self.rotation
    }

    fn get_speed(&self, _running: bool) -> i32 {
        if let Some(vehicle_speed) = self.vehicle_speed {
            vehicle_speed
        } else {
            let buffed_run_speed = self.buffs.get_buff_value(BuffID::UpMoveSpeed).unwrap_or(0);
            PLAYER_RUN_SPEED + buffed_run_speed
        }
    }

    fn get_chunk_coords(&self) -> ChunkCoords {
        ChunkCoords::from_pos_inst(self.position, self.instance_id)
    }

    fn set_position(&mut self, pos: Position) {
        self.position = pos;
    }

    fn set_rotation(&mut self, rotation: i32) {
        self.rotation = rotation.rem_euclid(360);
    }

    fn send_enter(&self, client: &FFClient) {
        match config_get().general.protocol.get() {
            FFProtocol::v0104 => {
                let pkt = v0104::sP_FE2CL_PC_NEW {
                    PCAppearanceData: self.get_appearance_data_104(),
                };
                client.send_packet(PacketID::P_FE2CL_PC_NEW, &pkt);
            }
            FFProtocol::v1013 => {
                let pkt = v1013::sP_FE2CL_PC_NEW {
                    PCAppearanceData: self.get_appearance_data_1013(),
                };
                client.send_packet(PacketID::P_FE2CL_PC_NEW, &pkt);
            }
        }
    }

    fn send_exit(&self, client: &FFClient) {
        let pkt = sP_FE2CL_PC_EXIT {
            iID: self.get_player_id(),
            iExitType: unused!(),
        };
        client.send_packet(PacketID::P_FE2CL_PC_EXIT, &pkt);
    }

    fn cleanup(self: Box<Self>, state: &mut ShardServerState) {
        let pc_id = self.get_player_id();

        // cleanup the buyback list
        if state.buyback_lists.contains_key(&pc_id) {
            state.buyback_lists.remove(&pc_id);
        }

        // cleanup ongoing trade
        if let Some(trade_id) = self.trade_id {
            let trade = state.ongoing_trades.remove(&trade_id).unwrap();
            let pc_id_other = trade.get_other_id(pc_id);
            let player_other = state.get_player_mut(pc_id_other).unwrap();
            player_other.trade_id = None;
            let client_other = player_other.get_client().unwrap();
            let pkt_cancel = sP_FE2CL_REP_PC_TRADE_CONFIRM_CANCEL {
                iID_Request: pc_id,
                iID_From: trade.get_id_from(),
                iID_To: trade.get_id_to(),
            };

            client_other.send_packet(P_FE2CL_REP_PC_TRADE_CONFIRM_CANCEL, &pkt_cancel);
        }

        // cleanup group
        if let Some(group_id) = self.group_id {
            helpers::remove_group_member(EntityID::Player(pc_id), group_id, state).unwrap();
        }
    }

    fn as_combatant(&self) -> Option<&dyn Combatant> {
        Some(self)
    }

    fn as_combatant_mut(&mut self) -> Option<&mut dyn Combatant> {
        Some(self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
impl Display for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let acc_level_to_title = |acc_level| {
            if acc_level <= CN_ACCOUNT_LEVEL__MASTER as i16 {
                return Some("Master");
            } else if acc_level <= CN_ACCOUNT_LEVEL__GM as i16 {
                return Some("GM");
            } else if acc_level <= CN_ACCOUNT_LEVEL__CS as i16 {
                return Some("Mod");
            }
            None
        };

        let title = acc_level_to_title(self.perms);
        let title = match title {
            Some(title) => format!("[{}] ", title),
            None => String::new(),
        };
        write!(
            f,
            "{}{} {} ({})",
            title,
            self.first_name,
            self.last_name,
            self.id
                .map(|id| id.to_string())
                .unwrap_or("???".to_string()),
        )
    }
}

#[derive(Debug)]
pub enum PlayerSearchQuery {
    ByID(i32),
    ByUID(i64),
    ByName(String, String),
}
impl PlayerSearchQuery {
    pub fn execute(&self, state: &ShardServerState) -> Option<i32> {
        match self {
            PlayerSearchQuery::ByID(pc_id) => {
                if state.get_player(*pc_id).is_ok() {
                    Some(*pc_id)
                } else {
                    None
                }
            }
            PlayerSearchQuery::ByUID(pc_uid) => {
                state.get_player_by_uid(*pc_uid).map(|p| p.get_player_id())
            }
            PlayerSearchQuery::ByName(first_name, last_name) => state
                .entity_map
                .find_players(|player| {
                    player.first_name.eq_ignore_ascii_case(first_name)
                        && player.last_name.eq_ignore_ascii_case(last_name)
                })
                .first()
                .copied(),
        }
    }
}

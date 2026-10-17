use std::{cmp::min, time::SystemTime};

use crate::{
    defines::*,
    entity::RewardData,
    enums::{BuffID, ItemLocation, ItemType, RewardCategory, RewardType},
    error::{FFError, FFResult},
    net::packet::*,
    tabledata::tdata_get,
    util,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Item {
    pub ty: ItemType,
    pub id: i16,
    appearance_id: Option<i16>,
    pub quantity: u16,
    expiry_time: Option<SystemTime>,
}
impl Item {
    pub fn new(ty: ItemType, id: i16) -> Self {
        if id == 0 {
            panic!("Item ID cannot be 0");
        }

        Self {
            ty,
            id,
            appearance_id: None,
            quantity: 1,
            expiry_time: None,
        }
    }

    pub fn get_stats(&self) -> FFResult<&ItemStats> {
        tdata_get().get_item_stats(self.id, self.ty)
    }

    pub fn has_expired(&self, now: SystemTime) -> bool {
        self.expiry_time.is_some_and(|expiry| expiry < now)
    }

    pub fn get_expiry_time(&self) -> Option<SystemTime> {
        self.expiry_time
    }

    pub fn set_expiry_time(&mut self, time: SystemTime) {
        self.expiry_time = Some(time);
    }

    pub fn set_appearance(&mut self, looks_item: &Item) {
        self.appearance_id = Some(looks_item.id);
    }

    pub fn split_items(from: &mut Option<Item>, mut quantity: u16) -> Option<Item> {
        if from.is_none() || quantity == 0 {
            return None;
        }

        let from_stack = from.as_mut().unwrap();
        quantity = min(quantity, from_stack.quantity);
        from_stack.quantity -= quantity;

        let mut result_stack = *from_stack;
        result_stack.quantity = quantity;

        if from_stack.quantity == 0 {
            *from = None;
        }
        Some(result_stack)
    }

    pub fn transfer_items(from: &mut Option<Item>, to: &mut Option<Item>) -> FFResult<()> {
        if from.is_none() {
            return Ok(());
        }

        if to.is_none() {
            *to = *from;
            *from = None;
            return Ok(());
        }

        let (from_stack, to_stack) = (from.as_mut().unwrap(), to.as_mut().unwrap());
        if from_stack.id != to_stack.id || from_stack.ty != to_stack.ty {
            std::mem::swap(from, to);
            return Ok(());
        }

        let max_stack_size = to_stack.get_stats()?.max_stack_size;
        let num_to_move = min(max_stack_size - to_stack.quantity, from_stack.quantity);
        to_stack.quantity += num_to_move;
        from_stack.quantity -= num_to_move;
        if from_stack.quantity == 0 {
            *from = None;
        }
        Ok(())
    }
}

#[derive(Debug, Copy, Clone)]
pub enum RewardItem {
    Normal {
        item: Option<Item>,
        slot_num: usize,
    },
    Quest {
        id: i16,
        count: usize,
        slot_num: usize,
    },
}
impl RewardItem {
    fn location(&self) -> ItemLocation {
        match self {
            Self::Normal { .. } => ItemLocation::Inven,
            Self::Quest { .. } => ItemLocation::QInven,
        }
    }

    fn slot_num(&self) -> usize {
        match self {
            Self::Normal { slot_num, .. } | Self::Quest { slot_num, .. } => *slot_num,
        }
    }
}
impl FromProto<RewardItem> for v0104::sItemReward {
    fn from_proto(reward: RewardItem) -> Self {
        Self {
            sItem: match reward {
                RewardItem::Normal { item, .. } => item.into_proto(),
                RewardItem::Quest { id, count, .. } => v0104::sItemBase {
                    iType: ItemType::Quest as i16,
                    iID: id,
                    iOpt: count as i32,
                    iTimeLimit: unused!(),
                },
            },
            eIL: reward.location() as i32,
            iSlotNum: reward.slot_num() as i32,
        }
    }
}
impl FromProto<RewardItem> for v1013::sItemReward {
    fn from_proto(reward: RewardItem) -> Self {
        Self {
            sItem: match reward {
                RewardItem::Normal { item, .. } => item.into_proto(),
                RewardItem::Quest { id, count, .. } => v1013::sItemBase {
                    iType: ItemType::Quest as i16,
                    iID: id,
                    iOpt: count as i32,
                    iTimeLimit: unused!(),
                    iSerial: 0,
                },
            },
            eIL: reward.location() as i32,
            iSlotNum: reward.slot_num() as i32,
        }
    }
}

// 104
impl TryFromProto<v0104::sItemBase> for Option<Item> {
    type Error = FFError;
    fn try_from_proto(value: v0104::sItemBase) -> FFResult<Self> {
        if value.iID == 0 || value.iOpt == 0 {
            Ok(None)
        } else {
            Ok(Some(Item {
                ty: value.iType.try_into()?,
                id: value.iID,
                appearance_id: {
                    let id = (value.iOpt >> 16) as i16;
                    if id == 0 {
                        None
                    } else {
                        Some(id)
                    }
                },
                quantity: value.iOpt as u16,
                expiry_time: if value.iTimeLimit == 0 {
                    None
                } else {
                    Some(util::get_systime_from_sec(value.iTimeLimit as u64))
                },
            }))
        }
    }
}
impl FromProto<Option<Item>> for v0104::sItemBase {
    fn from_proto(value: Option<Item>) -> Self {
        if let Some(value) = value {
            Self {
                iType: value.ty as i16,
                iID: value.id,
                iOpt: (value.quantity as i32) | ((value.appearance_id.unwrap_or(0) as i32) << 16),
                iTimeLimit: match value.expiry_time {
                    Some(time) => util::get_timestamp_sec(time) as i32,
                    None => 0,
                },
            }
        } else {
            Self::default()
        }
    }
}

// Academy
impl TryFromProto<v1013::sItemBase> for Option<Item> {
    type Error = FFError;
    fn try_from_proto(value: v1013::sItemBase) -> FFResult<Self> {
        if value.iID == 0 || value.iOpt == 0 {
            Ok(None)
        } else {
            Ok(Some(Item {
                ty: value.iType.try_into()?,
                id: value.iID,
                appearance_id: {
                    let id = (value.iOpt >> 16) as i16;
                    if id == 0 {
                        None
                    } else {
                        Some(id)
                    }
                },
                quantity: value.iOpt as u16,
                expiry_time: if value.iTimeLimit == 0 {
                    None
                } else {
                    Some(util::get_systime_from_sec(value.iTimeLimit as u64))
                },
            }))
        }
    }
}
impl FromProto<Option<Item>> for v1013::sItemBase {
    fn from_proto(value: Option<Item>) -> Self {
        if let Some(value) = value {
            Self {
                iType: value.ty as i16,
                iID: value.id,
                iOpt: (value.quantity as i32) | ((value.appearance_id.unwrap_or(0) as i32) << 16),
                iTimeLimit: match value.expiry_time {
                    Some(time) => util::get_timestamp_sec(time) as i32,
                    None => 0,
                },
                iSerial: 0,
            }
        } else {
            Self::default()
        }
    }
}

pub struct ItemStats {
    pub buy_price: u32,
    pub sell_price: u32,
    pub sellable: bool,
    pub tradeable: bool,
    pub max_stack_size: u16,
    pub required_level: i16,
    pub rarity: Option<i8>,
    pub gender: Option<i8>,
    pub single_power: Option<i32>,
    pub multi_power: Option<i32>,
    pub defense: Option<i32>,
    pub speed: Option<i32>,
}

pub struct VendorItem {
    pub sort_number: i32,
    pub ty: ItemType,
    pub id: i16,
}

pub struct VendorData {
    vendor_id: i32,
    items: Vec<VendorItem>,
}
impl VendorData {
    pub fn new(vendor_id: i32) -> Self {
        Self {
            vendor_id,
            items: Vec::new(),
        }
    }

    pub fn insert(&mut self, item: VendorItem) {
        self.items.push(item);
    }

    pub fn as_arr_104(&self) -> FFResult<[v0104::sItemVendor; SIZEOF_VENDOR_TABLE_SLOT as usize]> {
        let mut vendor_item_structs = Vec::new();
        for item in &self.items {
            vendor_item_structs.push(v0104::sItemVendor {
                iVendorID: self.vendor_id,
                fBuyCost: tdata_get().get_item_stats(item.id, item.ty)?.buy_price as f32,
                item: v0104::sItemBase {
                    iType: item.ty as i16,
                    iID: item.id,
                    iOpt: 1,
                    iTimeLimit: 0,
                },
                iSortNum: item.sort_number,
            });
        }
        vendor_item_structs.resize(
            SIZEOF_VENDOR_TABLE_SLOT as usize,
            v0104::sItemVendor {
                iVendorID: 0,
                fBuyCost: 0.0,
                item: v0104::sItemBase::default(),
                iSortNum: 0,
            },
        );
        Ok(vendor_item_structs.try_into().unwrap())
    }

    pub fn as_arr_1013(&self) -> FFResult<[v1013::sItemVendor; SIZEOF_VENDOR_TABLE_SLOT as usize]> {
        let mut vendor_item_structs = Vec::new();
        for item in &self.items {
            vendor_item_structs.push(v1013::sItemVendor {
                iVendorID: self.vendor_id,
                fBuyCost: tdata_get().get_item_stats(item.id, item.ty)?.buy_price as f32,
                item: v1013::sItemBase {
                    iType: item.ty as i16,
                    iID: item.id,
                    iOpt: 1,
                    iTimeLimit: 0,
                    iSerial: 0,
                },
                iSortNum: item.sort_number,
            });
        }
        vendor_item_structs.resize(
            SIZEOF_VENDOR_TABLE_SLOT as usize,
            v1013::sItemVendor {
                iVendorID: 0,
                fBuyCost: 0.0,
                item: v1013::sItemBase::default(),
                iSortNum: 0,
            },
        );
        Ok(vendor_item_structs.try_into().unwrap())
    }

    pub fn has_item(&self, item_id: i16, item_type: ItemType) -> bool {
        self.items
            .iter()
            .any(|item| item_id == item.id && item_type == item.ty)
    }
}

pub struct CrocPotData {
    pub base_chance: f32,
    pub rarity_diff_multipliers: [f32; 4],
    pub price_multiplier_looks: u32,
    pub price_multiplier_stats: u32,
}

#[derive(Debug, Clone)]
pub struct Reward {
    category: RewardCategory,
    pub taros: u32,
    pub fusion_matter: u32,
    pub weapon_boosts: u32,
    pub nano_potions: u32,
    pub items: Vec<Item>,
}
impl Reward {
    pub fn new(category: RewardCategory) -> Self {
        Self {
            category,
            taros: 0,
            fusion_matter: 0,
            weapon_boosts: 0,
            nano_potions: 0,
            items: Vec::new(),
        }
    }

    pub fn with_player_rates(self, player: &crate::entity::Player) -> Self {
        self.apply_player_rates(player, 1.0, 1)
    }

    pub fn with_combat_rewards(
        self,
        player: &crate::entity::Player,
        level_difference: i16,
        group_size: usize,
    ) -> Self {
        self.apply_player_rates(player, mob_level_fm_factor(level_difference), group_size)
    }

    fn apply_player_rates(
        mut self,
        player: &crate::entity::Player,
        level_effect: f64,
        group_size: usize,
    ) -> Self {
        let bonus = nano_reward_multiplier(player, BuffID::RewardCash);
        let scavenge = nano_reward_multiplier(player, BuffID::RewardBlob);
        let group_reduction = !crate::config::config_get()
            .shard
            .disable_group_reward_reduction
            .get();
        let category = self.category as usize;
        let taro_rate = player
            .reward_data
            .get_reward_rate(RewardType::Taros, category)
            .unwrap_or(1.0);
        let fm_rate = player
            .reward_data
            .get_reward_rate(RewardType::FusionMatter, category)
            .unwrap_or(1.0);

        let group_effect = if group_reduction {
            1.0 / group_size.max(1) as f64
        } else {
            1.0
        };
        self.taros = (self.taros as f64 * taro_rate as f64 * bonus * group_effect) as u32;

        let group_effect = if group_reduction {
            group_fm_factor(group_size)
        } else {
            1.0
        };
        self.fusion_matter =
            (self.fusion_matter as f64 * fm_rate as f64 * scavenge * level_effect * group_effect)
                as u32;
        // OpenFusion applies the Nanocom booster after truncating the base reward.
        self.fusion_matter = (self.fusion_matter as f64
            * crate::boosters::factor(player, self.category, SystemTime::now()))
            as u32;
        self
    }

    pub fn with_rates(mut self, reward_data: &RewardData) -> Self {
        let category = self.category as usize;
        let taros_factor = reward_data.get_reward_rate(RewardType::Taros, category);
        let fm_factor = reward_data.get_reward_rate(RewardType::FusionMatter, category);
        if let Ok(factor) = taros_factor {
            self.taros = (self.taros as f32 * factor) as u32;
        }
        if let Ok(factor) = fm_factor {
            self.fusion_matter = (self.fusion_matter as f32 * factor) as u32;
        }
        self
    }
}

fn nano_reward_multiplier(player: &crate::entity::Player, buff_id: BuffID) -> f64 {
    use crate::entity::Combatant;

    if !player.has_buff(buff_id, None) {
        return 1.0;
    }

    let boosted = player.get_active_nano_slot().is_some_and(|slot| {
        let buff = match slot {
            0 => BuffID::StimPakSlot1,
            1 => BuffID::StimPakSlot2,
            _ => BuffID::StimPakSlot3,
        };
        player.has_buff(buff, None)
    });
    if boosted {
        1.23
    } else {
        1.2
    }
}

fn mob_level_fm_factor(difference: i16) -> f64 {
    match difference {
        i16::MIN..=-3 => 1.2,
        -2 => 1.1,
        -1..=1 => 1.0,
        2 => 0.899,
        3 => 0.75,
        4 => 0.5,
        5 => 0.25,
        _ => 0.0,
    }
}

fn group_fm_factor(size: usize) -> f64 {
    match size {
        2 => 0.875,
        3 => 0.75,
        4 => 0.688,
        _ => 1.0,
    }
}

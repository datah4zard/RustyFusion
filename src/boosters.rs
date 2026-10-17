//! Academy Nanocom booster activation and Fusion Matter multipliers.
use std::time::{Duration, SystemTime};

use crate::{
    config::config_get,
    entity::Player,
    enums::{ItemLocation, ItemType, RewardCategory},
    error::*,
    helpers,
    item::Item,
    net::{
        packet::{PacketID::*, *},
        ClientMap, FFProtocol,
    },
    state::ShardServerState,
};

pub const DURATION: Duration = Duration::from_secs(604800);
const FUSION_HUNTER_BOOSTER: i16 = 153;
const IZ_RACER_BOOSTER: i16 = 154;
const QUESTER_BOOSTER: i16 = 155;
const SUPER_BOOSTER_DX: i16 = 156;

fn slot(item_id: i16) -> usize {
    // quester 155 -> 9, hunter 153 -> 10, racer 154 -> 11
    9 + ((item_id - FUSION_HUNTER_BOOSTER + 1) % 3) as usize
}

pub fn factor(player: &Player, category: RewardCategory, now: SystemTime) -> f64 {
    if config_get().general.protocol.get() != FFProtocol::v1013 {
        return 1.0;
    }
    let active = [QUESTER_BOOSTER, FUSION_HUNTER_BOOSTER, IZ_RACER_BOOSTER].map(|id| {
        player
            .get_item(ItemLocation::Equip, slot(id))
            .ok()
            .copied()
            .flatten()
            .is_some_and(|item| {
                item.ty == ItemType::General
                    && item.id == id
                    && item.quantity > 0
                    && !item.has_expired(now)
            })
    });
    let relevant = match category {
        RewardCategory::Missions => active[0],
        RewardCategory::Combat => active[1],
        RewardCategory::Racing => active[2],
        _ => false,
    };
    if !relevant {
        1.0
    } else if active.into_iter().all(|enabled| enabled) {
        1.75
    } else {
        1.5
    }
}

pub fn use_item(pkt: Packet, clients: &ClientMap, state: &mut ShardServerState) -> FFResult<()> {
    let client = clients.get_sender();
    (|| {
        let request: &sP_CL2FE_REQ_ITEM_USE = pkt.get()?;
        if config_get().general.protocol.get() != FFProtocol::v1013 {
            return Err(FFError::build(
                Severity::Warning,
                "Nanocom booster use is not supported in this version".into(),
            ));
        }
        if request.eIL != ItemLocation::Inven as i32 {
            return Err(FFError::build(
                Severity::Warning,
                "Nanocom booster must be in the inventory".into(),
            ));
        }
        let player = state.get_player_mut(client.get_player_id()?)?;
        if player.trade_id.is_some() {
            return Err(FFError::build(
                Severity::Warning,
                "Cannot use Nanocom boosters while trading".into(),
            ));
        }
        let now = SystemTime::now();
        let item = player
            .get_item(ItemLocation::Inven, request.iSlotNum as usize)?
            .ok_or_else(|| {
                FFError::build(Severity::Warning, "No booster in inventory slot".into())
            })?;
        if item.ty != ItemType::General
            || !(FUSION_HUNTER_BOOSTER..=SUPER_BOOSTER_DX).contains(&item.id)
            || item.quantity == 0
            || item.has_expired(now)
        {
            return Err(FFError::build(
                Severity::Warning,
                "Item is not a usable Nanocom booster".into(),
            ));
        }

        let booster_ids = if item.id == SUPER_BOOSTER_DX {
            vec![FUSION_HUNTER_BOOSTER, IZ_RACER_BOOSTER, QUESTER_BOOSTER]
        } else {
            vec![item.id]
        };
        let mut remaining = Some(item);
        Item::split_items(&mut remaining, 1);
        player.set_item(ItemLocation::Inven, request.iSlotNum as usize, remaining)?;

        // RustyFusion's server time is Unix milliseconds, so the stored expiry
        // timestamp also provides the client display time without an offset.
        let time_limit = now + DURATION;
        for item_id in booster_ids {
            let equip_slot = slot(item_id);
            let mut booster_item = Item::new(ItemType::General, item_id);
            booster_item.set_expiry_time(time_limit);
            // Refresh an existing booster instead of adding to its duration.
            player.set_item(ItemLocation::Equip, equip_slot, Some(booster_item))?;
            let response = v1013::sP_FE2CL_REP_PC_GIVE_ITEM_SUCC {
                eIL: ItemLocation::Equip as i32,
                iSlotNum: equip_slot as i32,
                Item: Some(booster_item).into_proto(),
            };
            client.send_packet(P_FE2CL_REP_PC_GIVE_ITEM_SUCC, &response);
            helpers::send_equip_change(
                client,
                player.get_player_id(),
                equip_slot as i32,
                Some(booster_item),
            );
        }
        let response = v1013::sP_FE2CL_REP_PC_ITEM_USE_SUCC {
            iPC_ID: player.get_player_id(),
            eIL: request.eIL,
            iSlotNum: request.iSlotNum,
            RemainItem: remaining.into_proto(),
            ..Default::default()
        };
        client.send_packet(P_FE2CL_REP_PC_ITEM_USE_SUCC, &response);
        Ok(())
    })()
    .catch_fail(|| {
        client.send_packet(
            P_FE2CL_REP_PC_ITEM_USE_FAIL,
            &sP_FE2CL_REP_PC_ITEM_USE_FAIL::default(),
        );
    })
}

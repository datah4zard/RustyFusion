use std::{
    cmp::min,
    time::{Duration, SystemTime},
};

use rand::random;

use crate::{
    config::config_get,
    defines::*,
    entity::{Entity, EntityID},
    enums::*,
    error::*,
    helpers,
    item::{Item, RewardItem},
    net::{
        packet::{PacketID::*, *},
        ClientMap, FFClient, FFProtocol,
    },
    state::ShardServerState,
    tabledata::tdata_get,
    util,
};

pub fn item_move(pkt: Packet, clients: &ClientMap, state: &mut ShardServerState) -> FFResult<()> {
    let client = clients.get_sender();
    let pkt: &sP_CL2FE_REQ_ITEM_MOVE = pkt.get()?;

    let pc_id = client.get_player_id()?;
    let player = state.get_player_mut(pc_id)?;

    let location_from = pkt.eFrom.try_into()?;
    let location_to = pkt.eTo.try_into()?;
    if (location_from == ItemLocation::Equip && pkt.iFromSlotNum >= 9)
        || (location_to == ItemLocation::Equip && pkt.iToSlotNum >= 9)
    {
        return Err(FFError::build(
            Severity::Warning,
            "Nanocom booster slots cannot be moved".into(),
        ));
    }
    let mut item_from = player.set_item(location_from, pkt.iFromSlotNum as usize, None)?;

    let mut item_to = player.set_item(location_to, pkt.iToSlotNum as usize, None)?;

    Item::transfer_items(&mut item_from, &mut item_to)?;
    player.set_item(location_from, pkt.iFromSlotNum as usize, item_from)?;
    player.set_item(location_to, pkt.iToSlotNum as usize, item_to)?;

    match config_get().general.protocol.get() {
        FFProtocol::v0104 => client.send_packet(
            P_FE2CL_PC_ITEM_MOVE_SUCC,
            &v0104::sP_FE2CL_PC_ITEM_MOVE_SUCC {
                eFrom: pkt.eFrom,
                iFromSlotNum: pkt.iFromSlotNum,
                FromSlotItem: item_from.into_proto(),
                eTo: pkt.eTo,
                iToSlotNum: pkt.iToSlotNum,
                ToSlotItem: item_to.into_proto(),
            },
        ),
        FFProtocol::v1013 => client.send_packet(
            P_FE2CL_PC_ITEM_MOVE_SUCC,
            &v1013::sP_FE2CL_PC_ITEM_MOVE_SUCC {
                eFrom: pkt.eFrom,
                iFromSlotNum: pkt.iFromSlotNum,
                FromSlotItem: item_from.into_proto(),
                eTo: pkt.eTo,
                iToSlotNum: pkt.iToSlotNum,
                ToSlotItem: item_to.into_proto(),
            },
        ),
    }

    let entity_id = player.get_id();
    if location_from == ItemLocation::Equip {
        state.entity_map.for_each_around(entity_id, |c| {
            helpers::send_equip_change(c, pc_id, pkt.iFromSlotNum, item_to);
        });
    }

    if location_to == ItemLocation::Equip {
        state.entity_map.for_each_around(entity_id, |c| {
            helpers::send_equip_change(c, pc_id, pkt.iToSlotNum, item_from);
        });
    }

    // dismount vehicle
    let player = state.get_player_mut(pc_id).unwrap();
    if ((location_from == ItemLocation::Equip && pkt.iFromSlotNum == EQUIP_SLOT_VEHICLE as i32)
        || (location_to == ItemLocation::Equip && pkt.iToSlotNum == EQUIP_SLOT_VEHICLE as i32))
        && player.vehicle_speed.is_some()
    {
        player.vehicle_speed = None;
        helpers::broadcast_state(pc_id, player.get_state_bit_flag(), state);
        let pkt = sP_FE2CL_PC_VEHICLE_OFF_SUCC::default();
        clients
            .get_sender()
            .send_packet(P_FE2CL_PC_VEHICLE_OFF_SUCC, &pkt);
    }

    state.check_and_remove_expired_items(pc_id, SystemTime::now());
    Ok(())
}

pub fn item_delete(pkt: Packet, client: &FFClient, state: &mut ShardServerState) -> FFResult<()> {
    let pc_id = client.get_player_id()?;
    let pkt: &sP_CL2FE_REQ_PC_ITEM_DELETE = pkt.get()?;
    let player = state.get_player_mut(pc_id)?;
    let location = pkt.eIL.try_into()?;
    if location != ItemLocation::Inven {
        return Err(FFError::build(
            Severity::Warning,
            format!(
                "Tried to delete item from invalid inventory: {:?}",
                location
            ),
        ));
    }

    player.set_item(location, pkt.iSlotNum as usize, None)?;

    let resp = sP_FE2CL_REP_PC_ITEM_DELETE_SUCC {
        eIL: pkt.eIL,
        iSlotNum: pkt.iSlotNum,
    };
    client.send_packet(P_FE2CL_REP_PC_ITEM_DELETE_SUCC, &resp);
    Ok(())
}

pub fn item_combination(
    pkt: Packet,
    client: &FFClient,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let pkt: &sP_CL2FE_REQ_PC_ITEM_COMBINATION = pkt.get()?;
    (|| {
        let player = state.get_player_mut(client.get_player_id()?)?;

        let looks_item = player
            .get_item(ItemLocation::Inven, pkt.iCostumeItemSlot as usize)?
            .as_ref()
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Costume item (slot {}) empty", pkt.iCostumeItemSlot),
            ))?;
        let looks_item_stats = looks_item.get_stats()?;
        let looks_item_rarity = looks_item_stats.rarity.ok_or(FFError::build(
            Severity::Warning,
            format!("Costume item has no rarity: {:?}", looks_item),
        ))?;

        let stats_item = player
            .get_item(ItemLocation::Inven, pkt.iStatItemSlot as usize)?
            .as_ref()
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Stats item (slot {}) empty", pkt.iStatItemSlot),
            ))?;
        let stats_item_stats = stats_item.get_stats()?;
        let stats_item_rarity = stats_item_stats.rarity.ok_or(FFError::build(
            Severity::Warning,
            format!("Stats item has no rarity: {:?}", stats_item),
        ))?;

        let level_gap = (looks_item_stats.required_level - stats_item_stats.required_level).abs();
        let rarity_gap = (looks_item_rarity - stats_item_rarity).unsigned_abs();
        if rarity_gap > 3 {
            return Err(FFError::build(
                Severity::Warning,
                format!("Rarity gap {} larger than 3", rarity_gap),
            ));
        }

        let crocpot_data = tdata_get().get_crocpot_data(level_gap)?;
        let cost = (looks_item_stats.buy_price * crocpot_data.price_multiplier_looks)
            + (stats_item_stats.buy_price * crocpot_data.price_multiplier_stats);
        if player.get_taros() < cost {
            return Err(FFError::build(
                Severity::Warning,
                format!(
                    "Not enough taros to perform combination ({} < {})",
                    player.get_taros(),
                    cost
                ),
            ));
        }
        let taros_left = player.set_taros(player.get_taros() - cost);

        let looks_item = player
            .set_item(ItemLocation::Inven, pkt.iCostumeItemSlot as usize, None)
            .unwrap()
            .unwrap();
        let mut stats_item = player
            .set_item(ItemLocation::Inven, pkt.iStatItemSlot as usize, None)
            .unwrap()
            .unwrap();

        let success_chance =
            crocpot_data.base_chance * crocpot_data.rarity_diff_multipliers[rarity_gap as usize];
        let roll: f32 = random();
        let succeeded = roll < success_chance;
        if succeeded {
            // set the appearance of the stats item
            stats_item.set_appearance(&looks_item);

            // put it back (where the looks item came from, since that's what the client expects)
            player
                .set_item(
                    ItemLocation::Inven,
                    pkt.iCostumeItemSlot as usize,
                    Some(stats_item),
                )
                .unwrap();
        } else {
            // put the items back
            player
                .set_item(
                    ItemLocation::Inven,
                    pkt.iCostumeItemSlot as usize,
                    Some(looks_item),
                )
                .unwrap();
            player
                .set_item(
                    ItemLocation::Inven,
                    pkt.iStatItemSlot as usize,
                    Some(stats_item),
                )
                .unwrap();
        }

        match config_get().general.protocol.get() {
            FFProtocol::v0104 => client.send_packet(
                P_FE2CL_REP_PC_ITEM_COMBINATION_SUCC,
                &v0104::sP_FE2CL_REP_PC_ITEM_COMBINATION_SUCC {
                    iNewItemSlot: pkt.iCostumeItemSlot,
                    sNewItem: Some(stats_item).into_proto(),
                    iStatItemSlot: pkt.iStatItemSlot,
                    iCashItemSlot1: pkt.iCashItemSlot1,
                    iCashItemSlot2: pkt.iCashItemSlot2,
                    iCandy: taros_left as i32,
                    iSuccessFlag: if succeeded { 1 } else { 0 },
                },
            ),
            FFProtocol::v1013 => client.send_packet(
                P_FE2CL_REP_PC_ITEM_COMBINATION_SUCC,
                &v1013::sP_FE2CL_REP_PC_ITEM_COMBINATION_SUCC {
                    iNewItemSlot: pkt.iCostumeItemSlot,
                    sNewItem: Some(stats_item).into_proto(),
                    iStatItemSlot: pkt.iStatItemSlot,
                    iCashItemSlot1: pkt.iCashItemSlot1,
                    iCashItemSlot2: pkt.iCashItemSlot2,
                    iCandy: taros_left as i32,
                    iSuccessFlag: if succeeded { 1 } else { 0 },
                },
            ),
        }
        Ok(())
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_REP_PC_ITEM_COMBINATION_FAIL {
            iErrorCode: unused!(),
            iCostumeItemSlot: pkt.iCostumeItemSlot,
            iStatItemSlot: pkt.iStatItemSlot,
            iCashItemSlot1: pkt.iCashItemSlot1,
            iCashItemSlot2: pkt.iCashItemSlot2,
        };

        client.send_packet(P_FE2CL_REP_PC_ITEM_COMBINATION_FAIL, &resp);
    })
}

pub fn item_chest_open(
    pkt: Packet,
    client: &FFClient,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let (location, slot_num) = match config_get().general.protocol.get() {
        FFProtocol::v0104 => {
            let pkt: &v0104::sP_CL2FE_REQ_ITEM_CHEST_OPEN = pkt.get()?;
            (pkt.eIL, pkt.iSlotNum)
        }
        FFProtocol::v1013 => {
            let pkt: &v1013::sP_CL2FE_REQ_ITEM_CHEST_OPEN = pkt.get()?;
            (pkt.eIL, pkt.iSlotNum)
        }
    };
    (|| {
        let player = state.get_player_mut(client.get_player_id()?)?;
        let location: ItemLocation = location.try_into()?;
        if location != ItemLocation::Inven {
            return Err(FFError::build(
                Severity::Warning,
                format!("C.R.A.T.E. not in main inventory: {:?}", location),
            ));
        }

        let chest = player
            .set_item(location, slot_num as usize, None)?
            .ok_or(FFError::build(
                Severity::Warning,
                format!("C.R.A.T.E. in empty slot: {}", slot_num),
            ))?;

        if chest.ty != ItemType::Chest {
            return Err(FFError::build(
                Severity::Warning,
                format!("Item is not a C.R.A.T.E.: {:?}", chest),
            ));
        }

        if let Some(nano_id) = tdata_get().get_nano_from_capsule(chest.id) {
            return helpers::open_nano_capsule(client, player, nano_id, slot_num);
        }

        let reward_item = tdata_get()
            .get_item_from_crate(chest.id, player.get_style().iGender as i32)
            .unwrap_or_else(|e| {
                // If for some reason we can't find a valid drop for the crate,
                // give the player a random gumball instead.
                // This idea was taken from OpenFusion <3
                log_error(e);
                util::get_random_gumball()
            });

        player.set_item(location, slot_num as usize, Some(reward_item))?;

        let mut reward_pkt =
            PacketBuilder::new(P_FE2CL_REP_REWARD_ITEM).with(&sP_FE2CL_REP_REWARD_ITEM {
                m_iCandy: player.get_taros() as i32,
                m_iFusionMatter: player.get_fusion_matter() as i32,
                m_iBatteryN: player.get_nano_potions() as i32,
                m_iBatteryW: player.get_weapon_boosts() as i32,
                iItemCnt: 1,
                iFatigue: 100,
                iFatigue_Level: 1,
                iNPC_TypeID: unused!(),
                iTaskID: unused!(),
            });
        helpers::push_item_reward(
            &mut reward_pkt,
            RewardItem::Normal {
                item: Some(reward_item),
                slot_num: slot_num as usize,
            },
        );
        let reward_pkt = reward_pkt.build()?;

        client.send_payload(reward_pkt);

        let resp = sP_FE2CL_REP_ITEM_CHEST_OPEN_SUCC { iSlotNum: slot_num };

        client.send_packet(P_FE2CL_REP_ITEM_CHEST_OPEN_SUCC, &resp);
        Ok(())
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_REP_ITEM_CHEST_OPEN_FAIL {
            iSlotNum: slot_num,
            iErrorCode: unused!(),
        };
        client.send_packet(P_FE2CL_REP_ITEM_CHEST_OPEN_FAIL, &resp);
    })
}

pub fn vendor_start(pkt: Packet, client: &FFClient, state: &mut ShardServerState) -> FFResult<()> {
    let pkt: &sP_CL2FE_REQ_PC_VENDOR_START = pkt.get()?;
    (|| {
        validate_vendor(client, state, pkt.iNPC_ID, pkt.iVendorID)?;
        let resp = sP_FE2CL_REP_PC_VENDOR_START_SUCC {
            iNPC_ID: pkt.iNPC_ID,
            iVendorID: pkt.iVendorID,
        };

        client.send_packet(P_FE2CL_REP_PC_VENDOR_START_SUCC, &resp);
        Ok(())
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_REP_PC_VENDOR_START_FAIL {
            iErrorCode: unused!(),
        };

        client.send_packet(P_FE2CL_REP_PC_VENDOR_START_FAIL, &resp);
    })
}

pub fn vendor_table_update(pkt: Packet, client: &FFClient) -> FFResult<()> {
    (|| {
        let pkt: &sP_CL2FE_REQ_PC_VENDOR_TABLE_UPDATE = pkt.get()?;

        let vendor_data = tdata_get().get_vendor_data(pkt.iVendorID)?;

        match config_get().general.protocol.get() {
            FFProtocol::v0104 => {
                let resp = v0104::sP_FE2CL_REP_PC_VENDOR_TABLE_UPDATE_SUCC {
                    item: vendor_data.as_arr_104()?,
                };

                client.send_packet(P_FE2CL_REP_PC_VENDOR_TABLE_UPDATE_SUCC, &resp);
            }
            FFProtocol::v1013 => {
                let resp = v1013::sP_FE2CL_REP_PC_VENDOR_TABLE_UPDATE_SUCC {
                    item: vendor_data.as_arr_1013()?,
                };

                client.send_packet(P_FE2CL_REP_PC_VENDOR_TABLE_UPDATE_SUCC, &resp);
            }
        }

        Ok(())
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_REP_PC_VENDOR_TABLE_UPDATE_FAIL {
            iErrorCode: unused!(),
        };

        client.send_packet(P_FE2CL_REP_PC_VENDOR_TABLE_UPDATE_FAIL, &resp);
    })
}

pub fn vendor_item_buy(
    pkt: Packet,
    client: &FFClient,
    state: &mut ShardServerState,
    time: SystemTime,
) -> FFResult<()> {
    (|| {
        let (npc_id, vendor_id, item_bought, inven_slot_num) =
            match config_get().general.protocol.get() {
                FFProtocol::v0104 => {
                    let pkt: &v0104::sP_CL2FE_REQ_PC_VENDOR_ITEM_BUY = pkt.get()?;
                    let item: Option<Item> = pkt.Item.try_into_proto()?;
                    (pkt.iNPC_ID, pkt.iVendorID, item, pkt.iInvenSlotNum)
                }
                FFProtocol::v1013 => {
                    let pkt: &v1013::sP_CL2FE_REQ_PC_VENDOR_ITEM_BUY = pkt.get()?;
                    let item: Option<Item> = pkt.Item.try_into_proto()?;
                    (pkt.iNPC_ID, pkt.iVendorID, item, pkt.iInvenSlotNum)
                }
            };

        validate_vendor(client, state, npc_id, vendor_id)?;

        let mut item = item_bought.ok_or(FFError::build(
            Severity::Warning,
            "Tried to buy nothing".to_string(),
        ))?;

        if item.ty == ItemType::Vehicle {
            // set expiration date
            let duration_min = config_get().shard.vehicle_duration.get();
            let duration_sec = Duration::from_secs(duration_min * 60);
            let expires = time + duration_sec;
            item.set_expiry_time(expires);
        }

        let vendor_data = tdata_get().get_vendor_data(vendor_id)?;
        if !vendor_data.has_item(item.id, item.ty) {
            return Err(FFError::build(
                Severity::Warning,
                format!(
                    "Vendor {} doesn't sell item ({}, {:?})",
                    vendor_id, item.id, item.ty
                ),
            ));
        }

        let stats = item.get_stats()?;
        let price = stats.buy_price * item.quantity as u32;
        let player = state.get_player_mut(client.get_player_id()?)?;
        if player.get_taros() < price {
            Err(FFError::build(
                Severity::Warning,
                format!(
                    "Not enough taros to buy item ({} < {})",
                    player.get_taros(),
                    price
                ),
            ))
        } else {
            player.set_item(ItemLocation::Inven, inven_slot_num as usize, Some(item))?;
            player.set_taros(player.get_taros() - price);

            match config_get().general.protocol.get() {
                FFProtocol::v0104 => client.send_packet(
                    P_FE2CL_REP_PC_VENDOR_ITEM_BUY_SUCC,
                    &v0104::sP_FE2CL_REP_PC_VENDOR_ITEM_BUY_SUCC {
                        iCandy: player.get_taros() as i32,
                        iInvenSlotNum: inven_slot_num,
                        Item: Some(item).into_proto(),
                    },
                ),
                FFProtocol::v1013 => client.send_packet(
                    P_FE2CL_REP_PC_VENDOR_ITEM_BUY_SUCC,
                    &v1013::sP_FE2CL_REP_PC_VENDOR_ITEM_BUY_SUCC {
                        iCandy: player.get_taros() as i32,
                        iInvenSlotNum: inven_slot_num,
                        Item: Some(item).into_proto(),
                    },
                ),
            }
            Ok(())
        }
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_REP_PC_VENDOR_ITEM_BUY_FAIL {
            iErrorCode: unused!(),
        };
        client.send_packet(P_FE2CL_REP_PC_VENDOR_ITEM_BUY_FAIL, &resp);
    })
}

pub fn vendor_item_sell(
    pkt: Packet,
    client: &FFClient,
    state: &mut ShardServerState,
) -> FFResult<()> {
    (|| {
        let pkt: &sP_CL2FE_REQ_PC_VENDOR_ITEM_SELL = pkt.get()?;
        let pc_id = client.get_player_id()?;
        let player = state.get_player_mut(pc_id)?;

        let item = player
            .get_item(ItemLocation::Inven, pkt.iInvenSlotNum as usize)?
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Tried to sell what's in empty slot {}", pkt.iInvenSlotNum),
            ))?;
        let stats = item.get_stats()?;

        if !stats.sellable {
            return Err(FFError::build(
                Severity::Warning,
                format!("Item not sellable: {:?}", item),
            ));
        }

        let mut remaining_item =
            player.set_item(ItemLocation::Inven, pkt.iInvenSlotNum as usize, None)?;
        let quantity = pkt.iItemCnt as u16;
        let item = Item::split_items(&mut remaining_item, quantity);
        player
            .set_item(
                ItemLocation::Inven,
                pkt.iInvenSlotNum as usize,
                remaining_item,
            )
            .unwrap();

        let sell_price = stats.sell_price * quantity as u32;
        let new_taros = player.set_taros(player.get_taros() + sell_price);
        let buyback_list = state.buyback_lists.entry(pc_id).or_default();
        buyback_list.push(item.unwrap());

        match config_get().general.protocol.get() {
            FFProtocol::v0104 => client.send_packet(
                P_FE2CL_REP_PC_VENDOR_ITEM_SELL_SUCC,
                &v0104::sP_FE2CL_REP_PC_VENDOR_ITEM_SELL_SUCC {
                    iCandy: new_taros as i32,
                    iInvenSlotNum: pkt.iInvenSlotNum,
                    Item: item.into_proto(),
                    ItemStay: remaining_item.into_proto(),
                },
            ),
            FFProtocol::v1013 => client.send_packet(
                P_FE2CL_REP_PC_VENDOR_ITEM_SELL_SUCC,
                &v1013::sP_FE2CL_REP_PC_VENDOR_ITEM_SELL_SUCC {
                    iCandy: new_taros as i32,
                    iInvenSlotNum: pkt.iInvenSlotNum,
                    Item: item.into_proto(),
                    ItemStay: remaining_item.into_proto(),
                },
            ),
        }
        Ok(())
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_REP_PC_VENDOR_ITEM_SELL_FAIL {
            iErrorCode: unused!(),
        };

        client.send_packet(P_FE2CL_REP_PC_VENDOR_ITEM_SELL_FAIL, &resp);
    })
}

pub fn vendor_item_restore_buy(
    pkt: Packet,
    client: &FFClient,
    state: &mut ShardServerState,
) -> FFResult<()> {
    (|| {
        let pc_id = client.get_player_id()?;
        let (npc_id, vendor_id, item_requested, inven_slot_num) =
            match config_get().general.protocol.get() {
                FFProtocol::v0104 => {
                    let pkt: &v0104::sP_CL2FE_REQ_PC_VENDOR_ITEM_RESTORE_BUY = pkt.get()?;
                    let item: Option<Item> = pkt.Item.try_into_proto()?;
                    (pkt.iNPC_ID, pkt.iVendorID, item, pkt.iInvenSlotNum)
                }
                FFProtocol::v1013 => {
                    let pkt: &v1013::sP_CL2FE_REQ_PC_VENDOR_ITEM_RESTORE_BUY = pkt.get()?;
                    let item: Option<Item> = pkt.Item.try_into_proto()?;
                    (pkt.iNPC_ID, pkt.iVendorID, item, pkt.iInvenSlotNum)
                }
            };
        validate_vendor(client, state, npc_id, vendor_id)?;

        let item: Item = item_requested.ok_or(FFError::build(
            Severity::Warning,
            "Bad item for buyback".to_string(),
        ))?;
        let buyback_list = state.buyback_lists.get_mut(&pc_id).ok_or(FFError::build(
            Severity::Warning,
            format!("Player {} has not sold any items", pc_id),
        ))?;

        let mut found_idx = None;
        for (i, list_item) in buyback_list.iter().enumerate() {
            if *list_item == item {
                found_idx = Some(i);
                break;
            }
        }
        let found_idx = found_idx.ok_or(FFError::build(
            Severity::Warning,
            format!(
                "Player tried to buyback an item they didn't sell: {:?}",
                item
            ),
        ))?;

        let item = buyback_list.remove(found_idx);
        let cost = item.get_stats()?.sell_price * item.quantity as u32; // sell price is cost for buyback
        let player = state.get_player_mut(pc_id)?;

        if player.get_taros() < cost {
            Err(FFError::build(
                Severity::Warning,
                format!(
                    "Not enough taros to buyback item ({} < {})",
                    player.get_taros(),
                    cost
                ),
            ))
        } else {
            player.set_item(ItemLocation::Inven, inven_slot_num as usize, Some(item))?;
            let new_taros = player.set_taros(player.get_taros() - cost);

            match config_get().general.protocol.get() {
                FFProtocol::v0104 => client.send_packet(
                    P_FE2CL_REP_PC_VENDOR_ITEM_RESTORE_BUY_SUCC,
                    &v0104::sP_FE2CL_REP_PC_VENDOR_ITEM_RESTORE_BUY_SUCC {
                        iCandy: new_taros as i32,
                        iInvenSlotNum: inven_slot_num,
                        Item: Some(item).into_proto(),
                    },
                ),
                FFProtocol::v1013 => client.send_packet(
                    P_FE2CL_REP_PC_VENDOR_ITEM_RESTORE_BUY_SUCC,
                    &v1013::sP_FE2CL_REP_PC_VENDOR_ITEM_RESTORE_BUY_SUCC {
                        iCandy: new_taros as i32,
                        iInvenSlotNum: inven_slot_num,
                        Item: Some(item).into_proto(),
                    },
                ),
            }
            Ok(())
        }
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_REP_PC_VENDOR_ITEM_RESTORE_BUY_FAIL {
            iErrorCode: unused!(),
        };

        client.send_packet(P_FE2CL_REP_PC_VENDOR_ITEM_RESTORE_BUY_FAIL, &resp);
    })
}

pub fn vendor_battery_buy(
    pkt: Packet,
    client: &FFClient,
    state: &mut ShardServerState,
) -> FFResult<()> {
    const BATTERY_TYPE_BOOST: i16 = 3;
    const BATTERY_TYPE_POTION: i16 = 4;

    (|| {
        let (npc_id, vendor_id, battery_type, battery_count) =
            match config_get().general.protocol.get() {
                FFProtocol::v0104 => {
                    let pkt: &v0104::sP_CL2FE_REQ_PC_VENDOR_BATTERY_BUY = pkt.get()?;
                    (pkt.iNPC_ID, pkt.iVendorID, pkt.Item.iID, pkt.Item.iOpt)
                }
                FFProtocol::v1013 => {
                    let pkt: &v1013::sP_CL2FE_REQ_PC_VENDOR_BATTERY_BUY = pkt.get()?;
                    (pkt.iNPC_ID, pkt.iVendorID, pkt.Item.iID, pkt.Item.iOpt)
                }
            };
        validate_vendor(client, state, npc_id, vendor_id)?;

        let mut quantity = battery_count as u32 * 100;

        let player = state.get_player_mut(client.get_player_id()?)?;
        match battery_type {
            BATTERY_TYPE_BOOST => {
                quantity = min(player.get_weapon_boosts() + quantity, PC_BATTERY_MAX)
                    - player.get_weapon_boosts();
            }
            BATTERY_TYPE_POTION => {
                quantity = min(player.get_nano_potions() + quantity, PC_BATTERY_MAX)
                    - player.get_nano_potions();
            }
            other => {
                return Err(FFError::build(
                    Severity::Warning,
                    format!("Bad battery type: {}", other),
                ));
            }
        }

        let cost = quantity;
        if player.get_taros() < cost {
            return Err(FFError::build(
                Severity::Warning,
                format!(
                    "Not enough taros to buyback item ({} < {})",
                    player.get_taros(),
                    cost
                ),
            ));
        }

        match battery_type {
            BATTERY_TYPE_BOOST => {
                player.set_weapon_boosts(player.get_weapon_boosts() + quantity);
            }
            BATTERY_TYPE_POTION => {
                player.set_nano_potions(player.get_nano_potions() + quantity);
            }
            other => {
                return Err(FFError::build(
                    Severity::Warning,
                    format!("Bad battery type: {}", other),
                ));
            }
        }
        let taros_new = player.set_taros(player.get_taros() - cost);

        let resp = sP_FE2CL_REP_PC_VENDOR_BATTERY_BUY_SUCC {
            iCandy: taros_new as i32,
            iBatteryW: player.get_weapon_boosts() as i32,
            iBatteryN: player.get_nano_potions() as i32,
        };

        client.send_packet(P_FE2CL_REP_PC_VENDOR_BATTERY_BUY_SUCC, &resp);
        Ok(())
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_REP_PC_VENDOR_BATTERY_BUY_FAIL {
            iErrorCode: unused!(),
        };
        client.send_packet(P_FE2CL_REP_PC_VENDOR_BATTERY_BUY_FAIL, &resp);
    })
}

pub fn streetstall_cancel(client: &FFClient) -> FFResult<()> {
    // streetstalls are scrapped, but the client still sends this packet
    // if the UI is brought up with /Store and then closed. it gets
    // softlocked if we don't respond
    let resp = sP_FE2CL_PC_STREETSTALL_REP_CANCEL_SUCC {
        iPCCharState: unused!(),
    };
    client.send_packet(P_FE2CL_PC_STREETSTALL_REP_CANCEL_SUCC, &resp);
    Ok(())
}

fn validate_vendor(
    client: &FFClient,
    state: &mut ShardServerState,
    npc_id: i32,
    vendor_id: i32,
) -> FFResult<()> {
    let pc_id = client.get_player_id()?;
    if npc_id == vendor_id {
        /*
         * due to a client bug where the iNPC_ID field in vendor packets is incorrectly
         * set to the same value as iVendorID, we need to lookup the NPC by its type
         * instead (which is equal to iVendorID for whatever reason).
         * On top of that, there may exist multiple NPCs with the same type... so if
         * one of them is close enough, we'll accept it.
         */
        let npc_ids = state.entity_map.find_npcs(|n| n.ty == vendor_id);
        for npc_id in npc_ids {
            if validate_vendor(client, state, npc_id, vendor_id).is_ok() {
                return Ok(());
            }
        }
        Err(FFError::build(
            Severity::Warning,
            "No matching NPCs close enough".to_string(),
        ))
    } else {
        let npc = state.get_npc(npc_id)?;
        if npc.ty != vendor_id {
            return Err(FFError::build(
                Severity::Warning,
                format!(
                    "Vendor {} has type {} instead of {}",
                    npc_id, npc.ty, vendor_id
                ),
            ));
        }
        state
            .entity_map
            .validate_proximity(
                &[EntityID::Player(pc_id), EntityID::NPC(npc_id)],
                RANGE_INTERACT,
            )
            .map_err(|e| {
                e.with_parent(FFError::build(
                    Severity::Warning,
                    format!("Vendor {} not close enough", npc_id),
                ))
            })
    }
}

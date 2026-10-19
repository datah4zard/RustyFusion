use rand::{
    rngs::{StdRng, ThreadRng},
    Rng, SeedableRng,
};
use uuid::Uuid;

use crate::{
    config::config_get,
    defines::{MSG_BOX_DURATION_DEFAULT, RANGE_GROUP_PARTICIPATE},
    entity::{Combatant, Entity, EntityID, Player},
    enums::*,
    error::*,
    item::{Item, Reward, RewardItem},
    nano::Nano,
    net::{
        packet::{PacketID::*, *},
        FFClient, FFProtocol,
    },
    state::ShardServerState,
    tabledata::tdata_get,
    util, Position,
};

pub fn send_equip_change(client: &FFClient, pc_id: i32, equip_slot_num: i32, item: Option<Item>) {
    match config_get().general.protocol.get() {
        FFProtocol::v0104 => client.send_packet(
            P_FE2CL_PC_EQUIP_CHANGE,
            &v0104::sP_FE2CL_PC_EQUIP_CHANGE {
                iPC_ID: pc_id,
                iEquipSlotNum: equip_slot_num,
                EquipSlotItem: item.into_proto(),
            },
        ),
        FFProtocol::v1013 => client.send_packet(
            P_FE2CL_PC_EQUIP_CHANGE,
            &v1013::sP_FE2CL_PC_EQUIP_CHANGE {
                iPC_ID: pc_id,
                iEquipSlotNum: equip_slot_num,
                EquipSlotItem: item.into_proto(),
            },
        ),
    }
}

pub fn send_nano_create_succ(
    client: &FFClient,
    fusion_matter: u32,
    quest_item_slot_num: Option<i32>,
    quest_item: Option<Item>,
    nano: Option<&Nano>,
    level: i16,
) {
    match config_get().general.protocol.get() {
        FFProtocol::v0104 => client.send_packet(
            P_FE2CL_REP_PC_NANO_CREATE_SUCC,
            &v0104::sP_FE2CL_REP_PC_NANO_CREATE_SUCC {
                iPC_FusionMatter: fusion_matter as i32,
                iQuestItemSlotNum: quest_item_slot_num.unwrap_or(-1),
                QuestItem: quest_item.into_proto(),
                Nano: nano.into_proto(),
                iPC_Level: level,
            },
        ),
        FFProtocol::v1013 => client.send_packet(
            P_FE2CL_REP_PC_NANO_CREATE_SUCC,
            &v1013::sP_FE2CL_REP_PC_NANO_CREATE_SUCC {
                iPC_FusionMatter: fusion_matter as i32,
                iQuestItemSlotNum: quest_item_slot_num.unwrap_or(-1),
                QuestItem: quest_item.into_proto(),
                Nano: nano.into_proto(),
                iPC_Level: level,
            },
        ),
    }
}

pub fn send_warp_use_npc_succ(
    client: &FFClient,
    pos: Position,
    location: i32,
    item_slot_num: i32,
    item: Option<Item>,
    taros: u32,
) {
    match config_get().general.protocol.get() {
        FFProtocol::v0104 => client.send_packet(
            P_FE2CL_REP_PC_WARP_USE_NPC_SUCC,
            &v0104::sP_FE2CL_REP_PC_WARP_USE_NPC_SUCC {
                iX: pos.x,
                iY: pos.y,
                iZ: pos.z,
                eIL: location,
                iItemSlotNum: item_slot_num,
                Item: item.into_proto(),
                iCandy: taros as i32,
            },
        ),
        FFProtocol::v1013 => client.send_packet(
            P_FE2CL_REP_PC_WARP_USE_NPC_SUCC,
            &v1013::sP_FE2CL_REP_PC_WARP_USE_NPC_SUCC {
                iX: pos.x,
                iY: pos.y,
                iZ: pos.z,
                eIL: location,
                iItemSlotNum: item_slot_num,
                Item: item.into_proto(),
                iCandy: taros as i32,
            },
        ),
    }
}

pub fn push_item_reward(pkt: &mut PacketBuilder, reward: RewardItem) {
    match config_get().general.protocol.get() {
        FFProtocol::v0104 => {
            let reward: v0104::sItemReward = reward.into_proto();
            pkt.push(&reward);
        }
        FFProtocol::v1013 => {
            let reward: v1013::sItemReward = reward.into_proto();
            pkt.push(&reward);
        }
    }
}

pub fn open_nano_capsule(
    client: &FFClient,
    player: &mut Player,
    nano_id: i16,
    slot_num: i32,
) -> FFResult<()> {
    // the capsule is already gone from the slot, so an empty reward clears it clientside
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

    push_item_reward(
        &mut reward_pkt,
        RewardItem::Normal {
            item: None,
            slot_num: slot_num as usize,
        },
    );
    client.send_payload(reward_pkt.build()?);

    let resp = sP_FE2CL_REP_ITEM_CHEST_OPEN_SUCC { iSlotNum: slot_num };
    client.send_packet(P_FE2CL_REP_ITEM_CHEST_OPEN_SUCC, &resp);

    if player.get_nano(nano_id).is_some() {
        log_if_failed(send_announcement(
            client,
            "You have already acquired this nano!",
            None,
        ));
        return Ok(());
    }

    let nano = player.unlock_nano(nano_id)?.clone();
    send_nano_create_succ(
        client,
        player.get_fusion_matter(),
        None,
        None,
        Some(&nano),
        player.get_level(),
    );
    Ok(())
}

pub fn broadcast_state(pc_id: i32, player_sbf: i8, state: &mut ShardServerState) {
    let bcast = sP_FE2CL_PC_STATE_CHANGE {
        iPC_ID: pc_id,
        iState: player_sbf,
    };
    state
        .entity_map
        .for_each_around(EntityID::Player(pc_id), |client| {
            client.send_packet(P_FE2CL_PC_STATE_CHANGE, &bcast);
        });
}

pub fn broadcast_monkey(pc_id: i32, ride_type: RideType, state: &mut ShardServerState) {
    let player = state.get_player(pc_id).unwrap();

    // monkey activate packet
    let pkt_monkey = sP_FE2CL_PC_RIDING {
        iPC_ID: pc_id,
        eRT: ride_type as i32,
    };

    // nano stash packets
    let pkt_nano = sP_FE2CL_REP_NANO_ACTIVE_SUCC {
        iActiveNanoSlotNum: -1,
        eCSTB___Add: 0,
    };
    let pkt_nano_bcast = sP_FE2CL_NANO_ACTIVE {
        iPC_ID: pc_id,
        Nano: None.into_proto(),
        iConditionBitFlag: player.get_condition_bit_flag(),
        eCSTB___Add: 0,
    };

    let client = player.get_client().unwrap();
    client.send_packet(P_FE2CL_REP_PC_RIDING_SUCC, &pkt_monkey);
    client.send_packet(P_FE2CL_REP_NANO_ACTIVE_SUCC, &pkt_nano);
    state
        .entity_map
        .for_each_around(EntityID::Player(pc_id), |c| {
            c.send_packet(P_FE2CL_PC_RIDING, &pkt_monkey);
            c.send_packet(P_FE2CL_NANO_ACTIVE, &pkt_nano_bcast);
        });
}

pub fn remove_group_member(
    leaver_id: EntityID,
    group_id: Uuid,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let mut group = state.groups.get(&group_id).unwrap().clone();
    group.remove_member(leaver_id)?;

    if group.should_disband() {
        // we can just tell all players that they've left the group
        // (except the leaver; that is the caller's job)
        let leaver_pkt = sP_FE2CL_PC_GROUP_LEAVE_SUCC::default();
        for eid in group.get_member_ids() {
            let entity = state.entity_map.get_entity_raw(*eid).unwrap();
            if let Some(client) = entity.get_client() {
                client.send_packet(P_FE2CL_PC_GROUP_LEAVE_SUCC, &leaver_pkt);
            }
            match eid {
                EntityID::Player(pc_id) => {
                    state.get_player_mut(*pc_id).unwrap().group_id = None;
                }
                EntityID::NPC(npc_id) => {
                    state.get_npc_mut(*npc_id).unwrap().group_id = None;
                }
                _ => unreachable!(),
            }
        }

        log(Severity::Debug, &format!("Disbanded group {}", group_id));
        state.groups.remove(&group_id);
        return Ok(());
    }

    // notify clients of the group member removal
    let (pc_group_data, npc_group_data) = group.get_member_data(state);
    match leaver_id {
        EntityID::Player(leaver_pc_id) => {
            let mut update_pkt =
                PacketBuilder::new(P_FE2CL_PC_GROUP_LEAVE).with(&sP_FE2CL_PC_GROUP_LEAVE {
                    iID_LeaveMember: leaver_pc_id,
                    iMemberPCCnt: pc_group_data.len() as i32,
                    iMemberNPCCnt: npc_group_data.len() as i32,
                });

            for pc_data in &pc_group_data {
                update_pkt.push(pc_data);
            }
            for npc_data in &npc_group_data {
                update_pkt.push(npc_data);
            }

            if let Some(update_pkt) = log_if_failed(update_pkt.build()) {
                for eid in group.get_member_ids() {
                    let entity = state.entity_map.get_entity_raw(*eid).unwrap();
                    if let Some(client) = entity.get_client() {
                        client.send_payload(update_pkt.clone());
                    }
                }
            }
        }
        EntityID::NPC(leaver_npc_id) => {
            let mut update_pkt = PacketBuilder::new(P_FE2CL_REP_NPC_GROUP_KICK_SUCC).with(
                &sP_FE2CL_REP_NPC_GROUP_KICK_SUCC {
                    iPC_ID: unused!(),
                    iNPC_ID: leaver_npc_id,
                    iMemberPCCnt: pc_group_data.len() as i32,
                    iMemberNPCCnt: npc_group_data.len() as i32,
                },
            );

            for pc_data in &pc_group_data {
                update_pkt.push(pc_data);
            }
            for npc_data in &npc_group_data {
                update_pkt.push(npc_data);
            }

            if let Some(update_pkt) = log_if_failed(update_pkt.build()) {
                for eid in group.get_member_ids() {
                    let entity = state.entity_map.get_entity_raw(*eid).unwrap();
                    if let Some(client) = entity.get_client() {
                        client.send_payload(update_pkt.clone());
                    }
                }
            }
        }
        _ => unreachable!(),
    }

    // save group state
    state.groups.insert(group_id, group);
    Ok(())
}

pub fn send_system_message(client: &FFClient, msg: &str) -> FFResult<()> {
    let pkt = sP_FE2CL_PC_MOTD_LOGIN {
        iType: unused!(),
        szSystemMsg: util::encode_utf16(msg)?,
    };

    client.send_packet(P_FE2CL_PC_MOTD_LOGIN, &pkt);
    Ok(())
}

pub fn send_announcement(
    client: &FFClient,
    msg: &str,
    duration_secs: Option<usize>,
) -> FFResult<()> {
    let duration = duration_secs
        .map(|secs| secs as i32)
        .unwrap_or(MSG_BOX_DURATION_DEFAULT);

    let pkt = sP_FE2CL_ANNOUNCE_MSG {
        iAnnounceType: unused!(),
        iDuringTime: duration,
        szAnnounceMsg: util::encode_utf16(msg)?,
    };

    client.send_packet(P_FE2CL_ANNOUNCE_MSG, &pkt);
    Ok(())
}

pub fn give_defeat_rewards(
    player: &mut Player,
    defeated_type: i32,
    rng: &mut impl Rng,
    group_size: usize,
    rolled_reward: &Reward,
) {
    let active_task_id = player.mission_journal.get_active_task_id().unwrap_or(0);
    let client = player.get_client().unwrap();
    let mut item_rewards = Vec::new();
    let (enemy_in_tasks, count_updated) = player.mission_journal.mark_enemy_defeated(defeated_type);

    // if this kill reduced the remaining enemy count in any tasks, notify the client
    if count_updated {
        let kill_pkt = sP_FE2CL_REP_PC_KILL_QUEST_NPCs_SUCC {
            iNPCID: defeated_type,
        };
        client.send_packet(P_FE2CL_REP_PC_KILL_QUEST_NPCs_SUCC, &kill_pkt);
    }

    // go through each task that has this enemy as a target and drop quest items
    for task_id in &enemy_in_tasks {
        let task_def = tdata_get().get_task_definition(*task_id).unwrap();
        let mut chosen_qitem = None;
        for qitem in &task_def.dropped_qitems {
            let qitem_id = *qitem.0;
            let max_qitem_count = *task_def.obj_qitems.get(&qitem_id).unwrap_or(&0);
            let qitem_count = player.get_quest_item_count(qitem_id);
            if qitem_count < max_qitem_count {
                match qitem_count {
                    0 => {
                        if player.get_free_slots(ItemLocation::QInven) > 0 {
                            chosen_qitem = Some(qitem);
                            break;
                        }
                    }
                    _ => {
                        chosen_qitem = Some(qitem);
                        break;
                    }
                }
            }
        }

        if let Some((&qitem_id, &drop_chance)) = chosen_qitem {
            let roll: f32 = rng.gen();
            log(
                Severity::Debug,
                &format!(
                    "Rolled {} against {} ({:?}) for qitem {}",
                    roll,
                    drop_chance,
                    roll < drop_chance,
                    qitem_id
                ),
            );
            if roll < drop_chance {
                let new_qitem_count = player.get_quest_item_count(qitem_id) + 1;
                let qitem_slot = player
                    .set_quest_item_count(qitem_id, new_qitem_count)
                    .unwrap();
                let qitem_drop = RewardItem::Quest {
                    id: qitem_id,
                    count: new_qitem_count,
                    slot_num: qitem_slot,
                };
                if active_task_id == *task_id {
                    // active task rewards should show up first
                    item_rewards.insert(0, qitem_drop);
                } else {
                    item_rewards.push(qitem_drop);
                }

                // fresh drop so a repair won't be needed.
                // to prevent one from happening, we signal that it's already done
                player.mission_journal.repair_task(*task_id).unwrap();
            }
        }
    }

    let reward = rolled_reward.clone().with_combat_rewards(
        player,
        player.get_level()
            - tdata_get()
                .get_npc_stats(defeated_type)
                .map(|s| s.level)
                .unwrap_or(player.get_level()),
        group_size,
    );
    let gained_taros = reward.taros;
    let gained_fm = reward.fusion_matter;
    let gained_potions = reward.nano_potions;
    let gained_boosts = reward.weapon_boosts;
    for item in reward.items {
        if let Ok(slot) = player.find_free_slot(ItemLocation::Inven) {
            player
                .set_item(ItemLocation::Inven, slot, Some(item))
                .unwrap();
            item_rewards.push(RewardItem::Normal {
                item: Some(item),
                slot_num: slot,
            });
        }
    }

    let mut reward_pkt =
        PacketBuilder::new(P_FE2CL_REP_REWARD_ITEM).with(&sP_FE2CL_REP_REWARD_ITEM {
            m_iCandy: player.set_taros(player.get_taros().saturating_add(gained_taros)) as i32,
            m_iFusionMatter: player
                .set_fusion_matter(player.get_fusion_matter().saturating_add(gained_fm))
                as i32,
            m_iBatteryN: player
                .set_nano_potions(player.get_nano_potions().saturating_add(gained_potions))
                as i32,
            m_iBatteryW: player
                .set_weapon_boosts(player.get_weapon_boosts().saturating_add(gained_boosts))
                as i32,
            iItemCnt: item_rewards.len() as i8,
            iFatigue: 100,
            iFatigue_Level: 1,
            iNPC_TypeID: defeated_type,
            iTaskID: active_task_id,
        });

    for item in &item_rewards {
        push_item_reward(&mut reward_pkt, *item);
    }

    if let Some(reward_pkt) = log_if_failed(reward_pkt.build()) {
        let client = player.get_client().unwrap();
        client.send_payload(reward_pkt);
    }
}

pub fn on_mob_defeated(
    npc_id: i32,
    defeater_id: EntityID,
    state: &mut ShardServerState,
    rng: &mut ThreadRng,
) -> FFResult<()> {
    let defeated_type = state.get_npc(npc_id)?.ty;
    let defeater = state.get_combatant(defeater_id)?;
    let position = defeater.get_position();
    let instance = defeater.get_chunk_coords().i;
    let mut recipients = std::collections::HashSet::new();
    if let EntityID::Player(pc_id) = defeater_id {
        recipients.insert(pc_id);
    }
    if let Some(group_id) = defeater.get_group_id() {
        if let Some(group) = state.groups.get(&group_id) {
            for member in group.get_member_ids() {
                if let EntityID::Player(pc_id) = *member {
                    let player = state.get_player(pc_id)?;
                    if player.instance_id == instance
                        && player.get_position().distance_to(&position) <= RANGE_GROUP_PARTICIPATE
                    {
                        recipients.insert(pc_id);
                    }
                }
            }
        }
    }
    if recipients.is_empty() {
        return Ok(());
    }
    let group_size = recipients.len();
    let rolled_reward = match tdata_get().get_mob_reward(defeated_type) {
        Ok(reward) => reward,
        Err(error) => {
            log_error(error);
            Reward::new(RewardCategory::Combat)
        }
    };
    let seed: u64 = rng.gen();
    for pc_id in recipients {
        let player = state.get_player_mut(pc_id)?;
        let mut shared_rng = StdRng::seed_from_u64(seed);
        give_defeat_rewards(
            player,
            defeated_type,
            &mut shared_rng,
            group_size,
            &rolled_reward,
        );
    }
    Ok(())
}

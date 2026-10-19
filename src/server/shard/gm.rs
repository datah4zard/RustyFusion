use std::{
    cmp::max,
    time::{Duration, SystemTime},
};

use crate::{
    chunk::{EntityMap, InstanceID, TickMode},
    config::config_get,
    defines::*,
    entity::{Combatant, Egg, Entity, EntityID, PlayerSearchQuery, NPC},
    enums::*,
    error::*,
    item::Item,
    monitor::monitor_event_to_packet,
    net::{
        packet::{PacketID::*, *},
        ClientMap, FFClient, FFProtocol,
    },
    state::ShardServerState,
    tabledata::tdata_get,
    util, Position,
};

pub fn gm_pc_set_value(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__DEVELOPER as i16)?;

    let pkt: &sP_CL2FE_GM_REQ_PC_SET_VALUE = pkt.get()?;
    let pc_id = pkt.iPC_ID;
    let value = pkt.iSetValue;
    let value_type = pkt.iSetValueType;
    let player = state.get_player_mut(pc_id)?;

    let value = match value_type as u32 {
        CN_GM_SET_VALUE_TYPE__HP => player.set_hp(value),
        CN_GM_SET_VALUE_TYPE__WEAPON_BATTERY => player.set_weapon_boosts(value as u32) as i32,
        CN_GM_SET_VALUE_TYPE__NANO_BATTERY => player.set_nano_potions(value as u32) as i32,
        CN_GM_SET_VALUE_TYPE__FUSION_MATTER => player.set_fusion_matter(value as u32) as i32,
        CN_GM_SET_VALUE_TYPE__CANDY => player.set_taros(value as u32) as i32,
        CN_GM_SET_VALUE_TYPE__SPEED => placeholder!(value),
        CN_GM_SET_VALUE_TYPE__JUMP => placeholder!(value),
        _ => {
            return Err(FFError::build(
                Severity::Warning,
                format!("Bad value type: {}", value_type),
            ));
        }
    };

    let resp = sP_FE2CL_GM_REP_PC_SET_VALUE {
        iPC_ID: pkt.iPC_ID,
        iSetValue: value,
        iSetValueType: value_type,
    };
    clients
        .get_sender()
        .send_packet(P_FE2CL_GM_REP_PC_SET_VALUE, &resp);
    Ok(())
}

pub fn gm_pc_give_item(
    pkt: Packet,
    client: &FFClient,
    state: &mut ShardServerState,
) -> FFResult<()> {
    (|| {
        let pc_id = helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__DEVELOPER as i16)?;
        let (location, slot_num, time_left, given_item, qitem_id, qitem_count) =
            match config_get().general.protocol.get() {
                FFProtocol::v0104 => {
                    let pkt: &v0104::sP_CL2FE_REQ_PC_GIVE_ITEM = pkt.get()?;
                    let item: Option<Item> = pkt.Item.try_into_proto()?;
                    (
                        pkt.eIL,
                        pkt.iSlotNum,
                        pkt.iTimeLeft,
                        item,
                        pkt.Item.iID,
                        pkt.Item.iOpt as usize,
                    )
                }
                FFProtocol::v1013 => {
                    let pkt: &v1013::sP_CL2FE_REQ_PC_GIVE_ITEM = pkt.get()?;
                    let item: Option<Item> = pkt.Item.try_into_proto()?;
                    (
                        pkt.eIL,
                        pkt.iSlotNum,
                        pkt.iTimeLeft,
                        item,
                        pkt.Item.iID,
                        pkt.Item.iOpt as usize,
                    )
                }
            };
        let player = state.get_player_mut(pc_id)?;

        let mut item = given_item;
        let time = time_left as u32;
        if let Some(item) = item.as_mut() {
            if time > 0 {
                let duration = Duration::from_secs(time as u64);
                let expiry_time = SystemTime::now() + duration;
                item.set_expiry_time(expiry_time);
            }
        }

        let item_location = location.try_into()?;
        let slot_number = match item_location {
            ItemLocation::QInven => player.set_quest_item_count(qitem_id, qitem_count)?,
            other => {
                let req_slot_number = slot_num as usize;
                player.set_item(other, req_slot_number, item)?;
                req_slot_number
            }
        };

        match config_get().general.protocol.get() {
            FFProtocol::v0104 => client.send_packet(
                P_FE2CL_REP_PC_GIVE_ITEM_SUCC,
                &v0104::sP_FE2CL_REP_PC_GIVE_ITEM_SUCC {
                    eIL: location,
                    iSlotNum: slot_number as i32,
                    Item: item.into_proto(),
                },
            ),
            FFProtocol::v1013 => client.send_packet(
                P_FE2CL_REP_PC_GIVE_ITEM_SUCC,
                &v1013::sP_FE2CL_REP_PC_GIVE_ITEM_SUCC {
                    eIL: location,
                    iSlotNum: slot_number as i32,
                    Item: item.into_proto(),
                },
            ),
        }
        Ok(())
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_REP_PC_GIVE_ITEM_FAIL {
            iErrorCode: unused!(),
        };
        client.send_packet(P_FE2CL_REP_PC_GIVE_ITEM_FAIL, &resp);
    })
}

pub fn gm_pc_give_nano(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let pkt: &sP_CL2FE_REQ_PC_GIVE_NANO = pkt.get()?;

    (|| {
        let client = clients.get_sender();
        let pc_id = helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__DEVELOPER as i16)?;
        let nano_id = pkt.iNanoID;
        let player = state.get_player_mut(pc_id)?;
        let new_level = max(player.get_level(), nano_id);
        player.set_level(new_level)?;
        let fusion_matter = player.get_fusion_matter();
        let nano = player.unlock_nano(nano_id)?;

        crate::helpers::send_nano_create_succ(
            client,
            fusion_matter,
            None,
            None,
            Some(&*nano),
            new_level,
        );

        let bcast = sP_FE2CL_REP_PC_CHANGE_LEVEL {
            iPC_ID: pc_id,
            iPC_Level: new_level,
        };

        state
            .entity_map
            .for_each_around(EntityID::Player(pc_id), |c| {
                c.send_packet(P_FE2CL_REP_PC_CHANGE_LEVEL, &bcast);
            });
        Ok(())
    })()
    .catch_fail(|| {
        let client = clients.get_sender();
        let resp = sP_FE2CL_REP_PC_NANO_CREATE_FAIL {
            iPC_ID: client.get_player_id().unwrap(),
            iErrorCode: unused!(),
        };

        client.send_packet(P_FE2CL_REP_PC_NANO_CREATE_FAIL, &resp);
    })
}

pub fn gm_pc_goto(pkt: Packet, clients: &ClientMap, state: &mut ShardServerState) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__DEVELOPER as i16)?;
    let pkt: &sP_CL2FE_REQ_PC_GOTO = pkt.get()?;
    let new_pos = Position {
        x: pkt.iToX,
        y: pkt.iToY,
        z: pkt.iToZ,
    };

    let player = state.get_player_mut(pc_id)?;
    player.set_position(new_pos);
    player.instance_id = InstanceID {
        map_num: ID_OVERWORLD,
        instance_num: None,
        ..player.instance_id
    };

    let taros = player.get_taros();

    state.entity_map.update(EntityID::Player(pc_id), None, true);

    // sP_FE2CL_REP_PC_GOTO_SUCC doesn't reset the clientside instance state,
    // but we need that to happen so we use the NPC warp packet instead
    crate::helpers::send_warp_use_npc_succ(
        client,
        new_pos,
        ItemLocation::end(),
        unused!(),
        None,
        taros,
    );
    Ok(())
}

pub fn gm_pc_special_state_switch(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__GM as i16)?;
    let pkt: &sP_CL2FE_GM_REQ_PC_SPECIAL_STATE_SWITCH = pkt.get()?;

    let player = state.get_player_mut(pc_id)?;

    match pkt.iSpecialStateFlag as u32 {
        CN_SPECIAL_STATE_FLAG__PRINT_GM => {
            player.show_gm_marker = !player.show_gm_marker;
        }
        CN_SPECIAL_STATE_FLAG__INVISIBLE => {
            player.invisible = !player.invisible;
        }
        CN_SPECIAL_STATE_FLAG__INVULNERABLE => {
            player.invulnerable = !player.invulnerable;
        }
        _ => {
            return Err(FFError::build(
                Severity::Warning,
                format!(
                    "P_CL2FE_GM_REQ_PC_SPECIAL_STATE_SWITCH: invalid special state flag: {}",
                    pkt.iSpecialStateFlag
                ),
            ));
        }
    }

    let special_state_flags = player.get_special_state_bit_flag();

    let resp = sP_FE2CL_REP_PC_SPECIAL_STATE_SWITCH_SUCC {
        iPC_ID: pkt.iPC_ID,
        iReqSpecialStateFlag: pkt.iSpecialStateFlag,
        iSpecialState: special_state_flags,
    };
    state
        .entity_map
        .for_each_around(EntityID::Player(pkt.iPC_ID), |c| {
            c.send_packet(P_FE2CL_PC_SPECIAL_STATE_CHANGE, &resp);
        });

    client.send_packet(P_FE2CL_REP_PC_SPECIAL_STATE_SWITCH_SUCC, &resp);
    Ok(())
}

pub fn gm_pc_motd_register(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__CS as i16)?;
    let pkt: &sP_CL2FE_GM_REQ_PC_MOTD_REGISTER = pkt.get()?;

    let pkt = sP_FE2LS_MOTD_REGISTER {
        szMessage: pkt.szSystemMsg,
    };

    if let Some(login_server) = clients.get_login_server() {
        login_server.send_packet(P_FE2LS_MOTD_REGISTER, &pkt);
    }

    Ok(())
}

pub fn gm_pc_announce(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__CS as i16)?;
    let player = state.get_player(pc_id).unwrap();
    let pkt: &sP_CL2FE_GM_REQ_PC_ANNOUNCE = pkt.get()?;
    let area_type: AreaType = pkt.iAreaType.try_into()?;

    let msg = util::parse_utf16(&pkt.szAnnounceMsg)?;
    log(
        Severity::Info,
        &format!("(GM announcement/{:?}) {}: \"{}\"", area_type, player, msg),
    );

    let bcast_event = ffmonitor::BroadcastEvent {
        scope: match area_type {
            AreaType::Local => ffmonitor::BroadcastScope::Local,
            AreaType::Channel => ffmonitor::BroadcastScope::Channel,
            AreaType::Shard => ffmonitor::BroadcastScope::Shard,
            AreaType::Global => ffmonitor::BroadcastScope::Global,
        },
        announcement_type: pkt.iAnnounceType as usize,
        duration_secs: pkt.iDuringTime as usize,
        from: player.to_string(),
        message: msg,
    };

    let pkt = sP_FE2CL_ANNOUNCE_MSG {
        iAnnounceType: pkt.iAnnounceType,
        iDuringTime: pkt.iDuringTime,
        szAnnounceMsg: pkt.szAnnounceMsg,
    };

    match area_type {
        AreaType::Local => {
            state
                .entity_map
                .for_each_around(EntityID::Player(pc_id), |c| {
                    c.send_packet(P_FE2CL_ANNOUNCE_MSG, &pkt);
                });
        }
        AreaType::Channel => state
            .entity_map
            .find_players(|p| p.instance_id.channel_num == player.instance_id.channel_num)
            .iter()
            .for_each(|pc_id| {
                let player = state.get_player(*pc_id).unwrap();
                let client = player.get_client().unwrap();
                client.send_packet(P_FE2CL_ANNOUNCE_MSG, &pkt);
            }),
        AreaType::Shard => state
            .entity_map
            .find_players(|_| true)
            .iter()
            .for_each(|pc_id| {
                let player = state.get_player(*pc_id).unwrap();
                let client = player.get_client().unwrap();
                client.send_packet(P_FE2CL_ANNOUNCE_MSG, &pkt);
            }),
        AreaType::Global => {
            if let Some(login_server) = clients.get_login_server() {
                login_server.send_packet(P_FE2LS_ANNOUNCE_MSG, &pkt);
            } else {
                log(
                    Severity::Warning,
                    "No login server connected; cannot send global announce",
                );
            }
        }
    }

    if let Some(login_server) = clients.get_login_server() {
        let monitor_pkt =
            monitor_event_to_packet(ffmonitor::Event::Broadcast(bcast_event)).unwrap();
        login_server.send_packet(P_FE2LS_UPDATE_MONITOR, &monitor_pkt);
    } else {
        log(
            Severity::Warning,
            "No login server connected; cannot send bcast monitor event",
        );
    }

    Ok(())
}

pub fn gm_pc_location(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    let gm_pc_id = helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__CS as i16)?;
    let pkt: &sP_CL2FE_GM_REQ_PC_LOCATION = pkt.get()?;
    let search_mode: TargetSearchBy = pkt.eTargetSearchBy.try_into()?;
    let search_query = match search_mode {
        TargetSearchBy::PlayerID => PlayerSearchQuery::ByID(pkt.iTargetPC_ID),
        TargetSearchBy::PlayerName => PlayerSearchQuery::ByName(
            util::parse_utf16(&pkt.szTargetPC_FirstName)?,
            util::parse_utf16(&pkt.szTargetPC_LastName)?,
        ),
        TargetSearchBy::PlayerUID => PlayerSearchQuery::ByUID(pkt.iTargetPC_UID),
    };

    if let Some(pc_id) = search_query.execute(state) {
        let player = state.get_player(pc_id)?;
        let pos = player.get_position();
        let resp = sP_FE2CL_GM_REP_PC_LOCATION {
            iTargetPC_UID: player.get_uid(),
            iTargetPC_ID: pc_id,
            iShardID: state.shard_id.unwrap(),
            iMapType: if player.instance_id.instance_num.is_some() {
                1 // instance
            } else {
                0 // non-instance
            },
            iMapID: player.instance_id.instance_num.unwrap_or(0) as i32,
            iMapNum: player.instance_id.map_num as i32,
            iX: pos.x,
            iY: pos.y,
            iZ: pos.z,
            szTargetPC_FirstName: util::encode_utf16(&player.first_name).unwrap(),
            szTargetPC_LastName: util::encode_utf16(&player.last_name).unwrap(),
        };

        client.send_packet(P_FE2CL_GM_REP_PC_LOCATION, &resp);

        Ok(())
    } else if search_mode != TargetSearchBy::PlayerID && clients.get_login_server().is_some() {
        // for name or UID search, we can ask the login server,
        // which will ask all the other shards
        let pkt = sP_FE2LS_REQ_PC_LOCATION {
            iPC_ID: gm_pc_id,
            sReq: *pkt,
        };

        let login_server = clients.get_login_server().unwrap();
        login_server.send_packet(P_FE2LS_REQ_PC_LOCATION, &pkt);
        Ok(())
    } else {
        Err(helpers::send_search_fail(
            clients.get_sender(),
            search_query,
        ))
    }
}

pub fn gm_target_pc_special_state_onoff(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__CS as i16)?;
    let pkt: &sP_CL2FE_GM_REQ_TARGET_PC_SPECIAL_STATE_ONOFF = pkt.get()?;

    let search_mode: TargetSearchBy = pkt.eTargetSearchBy.try_into()?;
    let search_query = match search_mode {
        TargetSearchBy::PlayerID => PlayerSearchQuery::ByID(pkt.iTargetPC_ID),
        TargetSearchBy::PlayerName => PlayerSearchQuery::ByName(
            util::parse_utf16(&pkt.szTargetPC_FirstName)?,
            util::parse_utf16(&pkt.szTargetPC_LastName)?,
        ),
        TargetSearchBy::PlayerUID => PlayerSearchQuery::ByUID(pkt.iTargetPC_UID),
    };

    let pc_id = search_query
        .execute(state)
        .ok_or_else(|| helpers::send_search_fail(client, search_query))?;

    let player = state.get_player_mut(pc_id)?;
    let new_flag = pkt.iONOFF != 0;
    match pkt.iSpecialStateFlag as u32 {
        // this packet is only used for /mute
        CN_SPECIAL_STATE_FLAG__MUTE_FREECHAT => {
            player.freechat_muted = new_flag;
        }
        _ => {
            return Err(FFError::build(
                Severity::Warning,
                format!(
                    "P_CL2FE_GM_REQ_TARGET_PC_SPECIAL_STATE_ONOFF: invalid special state flag: {}",
                    pkt.iSpecialStateFlag
                ),
            ));
        }
    }

    let special_state_flags = player.get_special_state_bit_flag();
    let resp = sP_FE2CL_REP_PC_SPECIAL_STATE_SWITCH_SUCC {
        iPC_ID: pc_id,
        iReqSpecialStateFlag: pkt.iSpecialStateFlag,
        iSpecialState: special_state_flags,
    };

    state
        .entity_map
        .for_each_around(EntityID::Player(pc_id), |c| {
            c.send_packet(P_FE2CL_PC_SPECIAL_STATE_CHANGE, &resp);
        });

    client.send_packet(P_FE2CL_REP_PC_SPECIAL_STATE_SWITCH_SUCC, &resp);

    Ok(())
}

pub fn gm_target_pc_teleport(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    let gm_pc_id = helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__CS as i16)?;
    let pkt: &sP_CL2FE_GM_REQ_TARGET_PC_TELEPORT = pkt.get()?;

    // the "target PC" is the player being teleported
    let search_mode: TargetSearchBy = pkt.eTargetPCSearchBy.try_into()?;
    let search_query = match search_mode {
        TargetSearchBy::PlayerID => PlayerSearchQuery::ByID(pkt.iTargetPC_ID),
        TargetSearchBy::PlayerName => PlayerSearchQuery::ByName(
            util::parse_utf16(&pkt.szTargetPC_FirstName)?,
            util::parse_utf16(&pkt.szTargetPC_LastName)?,
        ),
        TargetSearchBy::PlayerUID => PlayerSearchQuery::ByUID(pkt.iTargetPC_UID),
    };
    let target_pc_id = search_query
        .execute(state)
        .ok_or_else(|| helpers::send_search_fail(client, search_query))?;
    let target_player = state.get_player(target_pc_id).unwrap();
    let teleport_type: TeleportType = pkt.eTeleportType.try_into()?;
    let (dest_pos, dest_inst_id) = match teleport_type {
        TeleportType::XYZ => (
            Position {
                x: pkt.iToX,
                y: pkt.iToY,
                z: pkt.iToZ,
            },
            target_player.instance_id,
        ),
        TeleportType::MapXYZ => (
            Position {
                x: pkt.iToX,
                y: pkt.iToY,
                z: pkt.iToZ,
            },
            InstanceID {
                // player needs to be in the same map as the instance they want to teleport to
                instance_num: if pkt.iToMap == 0 {
                    None
                } else {
                    Some(pkt.iToMap as u32)
                },
                ..target_player.instance_id
            },
        ),
        TeleportType::MyLocation => {
            let my_player = state.get_player(gm_pc_id).unwrap();
            (my_player.get_position(), my_player.instance_id)
        }
        TeleportType::SomeoneLocation => {
            // the "goal PC" is the player being teleported to
            let search_mode: TargetSearchBy = pkt.eGoalPCSearchBy.try_into()?;
            let search_query = match search_mode {
                TargetSearchBy::PlayerID => PlayerSearchQuery::ByID(pkt.iGoalPC_ID),
                TargetSearchBy::PlayerName => PlayerSearchQuery::ByName(
                    util::parse_utf16(&pkt.szGoalPC_FirstName)?,
                    util::parse_utf16(&pkt.szGoalPC_LastName)?,
                ),
                TargetSearchBy::PlayerUID => PlayerSearchQuery::ByUID(pkt.iGoalPC_UID),
            };
            let goal_pc_id = search_query
                .execute(state)
                .ok_or_else(|| helpers::send_search_fail(client, search_query))?;
            let goal_player = state.get_player(goal_pc_id).unwrap();
            (goal_player.get_position(), goal_player.instance_id)
        }
        TeleportType::Unstick => (
            target_player.get_position().get_unstuck(),
            target_player.instance_id,
        ),
    };

    let player = state.get_player_mut(target_pc_id).unwrap();
    player.set_pre_warp();
    player.set_position(dest_pos);
    player.instance_id = dest_inst_id;

    let taros = player.get_taros();
    let client = player.get_client().unwrap();
    crate::helpers::send_warp_use_npc_succ(
        &client,
        dest_pos,
        ItemLocation::end(),
        unused!(),
        None,
        taros,
    );

    // see transport::helpers::do_warp to see why we use None for the chunk here
    state
        .entity_map
        .update(EntityID::Player(target_pc_id), None, true);

    Ok(())
}

pub fn gm_kick_player(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__CS as i16)?;
    let pkt: &sP_CL2FE_GM_REQ_KICK_PLAYER = pkt.get()?;
    let search_mode: TargetSearchBy = pkt.eTargetSearchBy.try_into()?;
    let search_query = match search_mode {
        TargetSearchBy::PlayerID => PlayerSearchQuery::ByID(pkt.iTargetPC_ID),
        TargetSearchBy::PlayerName => PlayerSearchQuery::ByName(
            util::parse_utf16(&pkt.szTargetPC_FirstName)?,
            util::parse_utf16(&pkt.szTargetPC_LastName)?,
        ),
        TargetSearchBy::PlayerUID => PlayerSearchQuery::ByUID(pkt.iTargetPC_UID),
    };

    let pc_id = search_query
        .execute(state)
        .ok_or_else(|| helpers::send_search_fail(client, search_query))?;

    let client = state.get_player(pc_id).unwrap().get_client().unwrap();

    let pkt = sP_FE2CL_REP_PC_EXIT_SUCC {
        iID: pc_id,
        iExitCode: EXIT_CODE_REQ_BY_GM as i32,
    };

    client.send_packet(P_FE2CL_REP_PC_EXIT_SUCC, &pkt);
    client.disconnect();
    Ok(())
}

pub fn gm_reward_rate(
    pkt: Packet,
    client: &FFClient,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let pc_id = helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__DEVELOPER as i16)?;
    let pkt: &sP_CL2FE_GM_REQ_REWARD_RATE = pkt.get()?;
    let player = state.get_player_mut(pc_id)?;

    if pkt.iGetSet != 0 {
        let reward_type: RewardType = pkt.iRewardType.try_into()?;
        let rate_percent = pkt.iSetRateValue as f64;
        let category: RewardCategory = (pkt.iRewardRateIndex as usize).try_into()?;
        player
            .reward_data
            .set_reward_rate(reward_type, category, rate_percent);
    }

    let resp = sP_FE2CL_GM_REP_REWARD_RATE_SUCC {
        afRewardRate_Taros: player.reward_data.get_rates_as_array(RewardType::Taros),
        afRewardRate_FusionMatter: player
            .reward_data
            .get_rates_as_array(RewardType::FusionMatter),
    };
    client.send_packet(P_FE2CL_GM_REP_REWARD_RATE_SUCC, &resp);
    Ok(())
}

pub fn gm_pc_task_complete(
    pkt: Packet,
    client: &FFClient,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let pc_id = helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__DEVELOPER as i16)?;
    let pkt: &sP_CL2FE_REQ_PC_TASK_COMPLETE = pkt.get()?;
    let player = state.get_player_mut(pc_id)?;
    let task_id = pkt.iTaskNum;
    player.mission_journal.complete_task(task_id)?;
    let resp = sP_FE2CL_REP_PC_TASK_END_SUCC { iTaskNum: task_id };
    client.send_packet(P_FE2CL_REP_PC_TASK_END_SUCC, &resp);
    Ok(())
}

pub fn gm_pc_mission_complete(
    pkt: Packet,
    client: &FFClient,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let pc_id = helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__DEVELOPER as i16)?;
    let pkt: &sP_CL2FE_REQ_PC_MISSION_COMPLETE = pkt.get()?;
    let player = state.get_player_mut(pc_id)?;
    let mission_id = pkt.iMissionNum;
    player.mission_journal.set_mission_completed(mission_id)?;
    let resp = sP_FE2CL_REP_PC_MISSION_COMPLETE_SUCC {
        iMissionNum: mission_id,
    };
    client.send_packet(P_FE2CL_REP_PC_MISSION_COMPLETE_SUCC, &resp);
    Ok(())
}

pub fn gm_shiny_summon(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__GM as i16)?;
    let pkt: &sP_CL2FE_REQ_SHINY_SUMMON = pkt.get()?;
    let player = state.get_player(pc_id)?;

    let egg_type = pkt.iShinyType;
    tdata_get().get_egg_stats(egg_type)?;
    let egg_pos = Position {
        x: pkt.iX,
        y: pkt.iY,
        z: pkt.iZ,
    };
    let egg_instance_id = player.instance_id;

    let entity_map = &mut state.entity_map;
    let egg_id = entity_map.gen_next_egg_id();
    let egg = Egg::new(egg_id, egg_type, egg_pos, egg_instance_id, true);
    let chunk_coords = egg.get_chunk_coords();
    let eid = entity_map.track(Box::new(egg), TickMode::Always);
    entity_map.update(eid, Some(chunk_coords), true);
    Ok(())
}

pub fn gm_npc_summon(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__GM as i16)?;
    let pkt: &sP_CL2FE_REQ_NPC_SUMMON = pkt.get()?;
    let player = state.get_player(pc_id)?;

    let npc_type = pkt.iNPCType;
    tdata_get().get_npc_stats(npc_type)?;

    let spawn_pos = player.get_position();
    let spawn_angle = player.get_rotation();
    let spawn_instance_id = player.instance_id;

    let entity_map = &mut state.entity_map;
    let count = pkt.iNPCCnt as usize;
    for _ in 0..count {
        let npc_id = entity_map.gen_next_npc_id();
        let npc = NPC::new(npc_id, npc_type, spawn_pos, spawn_angle, spawn_instance_id).unwrap();
        helpers::spawn_temp_npc(entity_map, npc);
    }

    Ok(())
}

pub fn gm_npc_group_summon(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__GM as i16)?;
    let pkt: &sP_CL2FE_REQ_NPC_GROUP_SUMMON = pkt.get()?;
    let player = state.get_player(pc_id)?;

    let spawn_pos = player.get_position();
    let spawn_angle = player.get_rotation();
    let spawn_instance_id = player.instance_id;

    let group_id = pkt.iNPCGroupType;
    let entity_map = &mut state.entity_map;
    let npcs = tdata_get().make_group_npcs(entity_map, unused!(), group_id);
    for mut npc in npcs {
        npc.spawn_position = spawn_pos;
        npc.set_position(spawn_pos);
        npc.set_rotation(spawn_angle);
        npc.instance_id = spawn_instance_id;
        helpers::spawn_temp_npc(entity_map, npc);
    }

    Ok(())
}

pub fn gm_npc_unsummon(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__GM as i16)?;
    let pkt: &sP_CL2FE_REQ_NPC_UNSUMMON = pkt.get()?;
    let npc_id = pkt.iNPC_ID;
    let npc = state.get_npc(npc_id)?;
    if !npc.summoned {
        return Ok(());
    }

    helpers::remove_temp_npc(state, npc_id);
    Ok(())
}

mod helpers {
    use crate::{ai, helpers::send_announcement};

    use super::*;

    pub fn validate_perms(
        client: &FFClient,
        state: &ShardServerState,
        req_perms: i16,
    ) -> FFResult<i32> {
        let user_pc_id = client.get_player_id()?;
        let player = state.get_player(user_pc_id)?;
        if player.perms > req_perms {
            return Err(FFError::build(
                Severity::Warning,
                format!(
                    "{} tried to use cheats without sufficient perms: {}",
                    player, player.perms
                ),
            ));
        }
        Ok(user_pc_id)
    }

    pub fn send_search_fail(client: &FFClient, query: PlayerSearchQuery) -> FFError {
        let err_msg = format!("Player not found: {:?}", query);
        log_if_failed(send_announcement(client, &err_msg, None));
        FFError::build(Severity::Warning, err_msg)
    }

    pub fn spawn_temp_npc(entity_map: &mut EntityMap, mut npc: NPC) {
        npc.summoned = true;
        let (ai, tick_mode) = ai::make_for_npc(&npc, true);
        npc.ai = ai;
        let chunk_coords = npc.get_chunk_coords();
        let eid = entity_map.track(Box::new(npc), tick_mode);
        entity_map.update(eid, Some(chunk_coords), true);
    }

    pub fn remove_temp_npc(state: &mut ShardServerState, npc_id: i32) {
        let entity_map = &mut state.entity_map;
        let eid = EntityID::NPC(npc_id);
        entity_map.update(eid, None, true);
        let npc = entity_map.untrack(eid);
        npc.cleanup(state)
    }
}

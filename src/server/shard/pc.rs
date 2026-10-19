use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use tokio::sync::Mutex;

use crate::{
    chunk::{TickMode, MAP_SQUARE_SIZE},
    config::config_get,
    database::{db_get, DbImpl as _},
    defines::*,
    entity::{Combatant, Entity, EntityID, Player},
    enums::*,
    error::*,
    helpers::broadcast_state,
    net::{
        crypto::{self, EncryptionMode},
        packet::{PacketID::*, *},
        ClientMap, ClientType, FFClient, FFProtocol,
    },
    state::ShardServerState,
    tabledata::tdata_get,
    util, Position,
};

pub async fn pc_enter(
    pkt: Packet,
    clients: &ClientMap<'_>,
    state_lock: Arc<Mutex<ShardServerState>>,
    time: SystemTime,
) -> FFResult<()> {
    let pkt: &sP_CL2FE_REQ_PC_ENTER = pkt.get()?;
    let serial_key: i64 = pkt.iEnterSerialKey;
    let enter_serial_key = pkt.iEnterSerialKey;

    // Phase 1: validate, kick duplicate, reserve ID (lock held)
    let (login_data, pc_id, existing_player) = {
        let mut state = state_lock.lock().await;
        let Some(login_data) = state.login_data.remove(&serial_key) else {
            return Err(FFError::build(Severity::Warning, format!("Login data for serial key {} missing; double check your shard's external IP config", serial_key)));
        };

        log(
            Severity::Info,
            &format!(
                "Loading player {} with pending channel {}",
                login_data.iPC_UID, login_data.iChannelRequestNum
            ),
        );

        // guard against a second pc_enter for the same UID while we're doing the DB load
        if !state.pending_entering_uids.insert(login_data.iPC_UID) {
            return Err(FFError::build(
                Severity::Warning,
                format!("Player UID {} is already entering", login_data.iPC_UID),
            ));
        }

        // check if this player is already in the shard and kick if so.
        // take ownership of the existing Player to avoid a redundant DB load.
        let existing_player = if let Some(existing_client) = state
            .get_player_by_uid(login_data.iPC_UID)
            .map(|p| p.get_client().unwrap())
        {
            let existing_pc_id = existing_client.get_player_id().unwrap();
            log(
                Severity::Warning,
                &format!(
                    "Player with UID {} already in the shard as player {}; kicking...",
                    login_data.iPC_UID, existing_pc_id
                ),
            );

            let pkt = sP_FE2CL_REP_PC_EXIT_DUPLICATE {
                iErrorCode: unused!(),
            };

            existing_client.send_packet(P_FE2CL_REP_PC_EXIT_DUPLICATE, &pkt);
            existing_client.clear_player_id().unwrap();
            existing_client.disconnect();

            let existing_player = Player::remove_from_state(existing_pc_id, &mut state);
            Some(existing_player)
        } else {
            None
        };

        let pc_id = state.entity_map.gen_next_pc_id();
        (login_data, pc_id, existing_player)
        // lock released here
    };

    // Phase 2: get the player, either from the existing session or from DB (lock NOT held)
    let player_uid = login_data.iPC_UID;
    let mut player = match existing_player {
        Some(player) => player,
        None => {
            let db = db_get();
            match db
                .load_player(login_data.iAccountID, login_data.iPC_UID)
                .await
            {
                Ok(Some(player)) => player,
                Ok(None) => {
                    state_lock
                        .lock()
                        .await
                        .pending_entering_uids
                        .remove(&player_uid);

                    return Err(FFError::build(
                        Severity::Warning,
                        format!(
                            "Player with UID {} not found for account with ID {}",
                            login_data.iPC_UID, login_data.iAccountID
                        ),
                    ));
                }
                Err(e) => {
                    // clean up pending_entering_uids on failure
                    state_lock
                        .lock()
                        .await
                        .pending_entering_uids
                        .remove(&player_uid);

                    return Err(e);
                }
            }
        }
    };

    // Phase 3: insert player into state (re-acquire lock)
    let mut state = state_lock.lock().await;
    state.pending_entering_uids.remove(&player_uid);

    let client = clients.get_sender();
    player.set_player_id(pc_id);
    player.set_client(client.clone());
    client.set_client_type(ClientType::GameClient {
        account_id: login_data.iAccountID,
        serial_key: enter_serial_key,
        pc_id: Some(pc_id),
    });

    // set buddy warp timestamp from login data if present
    if login_data.iBuddyWarpTime > 0 {
        player.buddy_warp_available_at = Some(login_data.iBuddyWarpTime);
    }

    let channel_num = if login_data.iChannelRequestNum > 0 {
        login_data.iChannelRequestNum
    } else {
        state.entity_map.get_min_pop_channel_num()
    };

    player.instance_id.channel_num = channel_num;

    let svr_time = util::get_timestamp_ms(time);
    let protocol = config_get().general.protocol.get();
    let resp_pkt = match protocol {
        FFProtocol::v0104 => Packet::new(
            P_FE2CL_REP_PC_ENTER_SUCC,
            &v0104::sP_FE2CL_REP_PC_ENTER_SUCC {
                iID: pc_id,
                PCLoadData2CL: player.get_load_data_104(),
                uiSvrTime: svr_time,
            },
        )?,
        FFProtocol::v1013 => Packet::new(
            P_FE2CL_REP_PC_ENTER_SUCC,
            &v1013::sP_FE2CL_REP_PC_ENTER_SUCC {
                iID: pc_id,
                PCLoadData2CL: player.get_load_data_1013(),
                uiSvrTime: svr_time,
            },
        )?,
    };

    let nano_book = match protocol {
        FFProtocol::v0104 => None,
        FFProtocol::v1013 => Some(helpers::build_nano_book(&player)?),
    };

    let iv1: i32 = pc_id + 1;
    let iv2: i32 = player.get_fusion_matter() as i32 + 1;
    let e_key = crypto::gen_key(svr_time, iv1, iv2);
    let fe_key = login_data.uiFEKey;
    let enc_mode = EncryptionMode::FEKey;
    client.update_encryption(Some(e_key), Some(fe_key), Some(enc_mode));

    let pkt_motd = sP_FE2LS_REQ_MOTD { iPC_ID: pc_id };
    match clients.get_login_server() {
        Some(login_server) => {
            login_server.send_packet(P_FE2LS_REQ_MOTD, &pkt_motd);
        }
        None => {
            log(
                Severity::Warning,
                "P_CL2FE_REQ_PC_ENTER: No login server connected! Things may break.",
            );
        }
    }

    log(
        Severity::Info,
        &format!(
            "{} joined (channel {})",
            player, player.instance_id.channel_num
        ),
    );

    let player_uid = player.get_uid();
    state.entity_map.track(Box::new(player), TickMode::Always);
    state.player_uid_to_id.insert(player_uid, pc_id);

    let sender = clients.get_sender();
    match nano_book {
        // the resize must arrive before PC_ENTER_SUCC so the client
        // sizes its nano array, and the contents after it
        Some((resize, pages)) => {
            sender.send_payload(resize);
            sender.send_payload(resp_pkt);
            for page in pages {
                sender.send_payload(page);
            }
        }
        None => sender.send_payload(resp_pkt),
    }

    Ok(())
}

pub async fn pc_exit(clients: &ClientMap<'_>, state: Arc<Mutex<ShardServerState>>) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = client.get_player_id()?;

    let exit_code = if clients.get_login_server().is_some() {
        EXIT_CODE_REQ_BY_PC
    } else {
        EXIT_CODE_SERVER_ERROR
    };

    let resp = sP_FE2CL_REP_PC_EXIT_SUCC {
        iID: pc_id,
        iExitCode: exit_code as i32,
    };

    let player = {
        let mut state = state.lock().await;
        Player::remove_from_state(pc_id, &mut state)
    };

    // save to db
    let db = db_get();
    log_if_failed(db.save_player(&player).await);

    client.send_packet(P_FE2CL_REP_PC_EXIT_SUCC, &resp);
    client.clear_player_id().unwrap();
    client.disconnect();
    Ok(())
}

pub fn pc_loading_complete(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let _pkt: &sP_CL2FE_REQ_PC_LOADING_COMPLETE = pkt.get()?;

    let resp = sP_FE2CL_REP_PC_LOADING_COMPLETE_SUCC { iPC_ID: unused!() };
    let pc_id = clients.get_sender().get_player_id()?;
    let player = state.get_player_mut(pc_id)?;
    if player
        .race
        .as_ref()
        .is_some_and(|r| r.instance != player.instance_id)
    {
        player.race = None;
    }
    let preserve_race_pods = player.race.is_some();
    let map_num = player.instance_id.map_num;
    let chunk = player.get_chunk_coords();
    state
        .entity_map
        .update(EntityID::Player(pc_id), Some(chunk), true);
    let client = clients.get_sender();
    client.send_packet(P_FE2CL_REP_PC_LOADING_COMPLETE_SUCC, &resp);

    // map info sync
    if map_num != ID_OVERWORLD && !preserve_race_pods {
        let map_data = tdata_get().get_map_data(map_num)?;
        let x_min = map_data.map_square.0 * MAP_SQUARE_SIZE;
        let y_min = map_data.map_square.1 * MAP_SQUARE_SIZE;
        let mut map_info_pkt = sP_FE2CL_INSTANCE_MAP_INFO {
            iInstanceMapNum: map_num as i32,
            iCreateTick: unused!(),
            iMapCoordX_Min: x_min,
            iMapCoordX_Max: x_min + MAP_SQUARE_SIZE,
            iMapCoordY_Min: y_min,
            iMapCoordY_Max: y_min + MAP_SQUARE_SIZE,
            iMapCoordZ_Min: i32::MIN,
            iMapCoordZ_Max: i32::MAX,
            iEP_ID: unused!(),
            iEPTopRecord_Score: unused!(),
            iEPTopRecord_Rank: unused!(),
            iEPTopRecord_Time: unused!(),
            iEPTopRecord_RingCount: unused!(),
            iEPSwitch_StatusON_Cnt: unused!(),
        };

        if let Some(ep_id) = map_data.ep_id {
            map_info_pkt.iEP_ID = ep_id as i32;
            // TODO remaining EP data
        }
        client.send_packet(P_FE2CL_INSTANCE_MAP_INFO, &map_info_pkt);
    }

    // buddy list sync.
    // we only want to do this once and we can't do it on initial load
    let player = state.get_player_mut(pc_id).unwrap();
    if !player.buddy_list_synced {
        let buddy_info = player.get_all_buddy_info();
        let mut buddy_list_pkt = PacketBuilder::new(P_FE2CL_REP_PC_BUDDYLIST_INFO_SUCC).with(
            &sP_FE2CL_REP_PC_BUDDYLIST_INFO_SUCC {
                iID: unused!(),
                iPCUID: unused!(),
                iListNum: 0, // we don't need to chunk the buddy list
                iBuddyCnt: buddy_info.len() as i8,
            },
        );

        for entry in buddy_info {
            let buddy_pkt: sBuddyBaseInfo = entry.into();
            buddy_list_pkt.push(&buddy_pkt);
        }

        let buddy_list_pkt = buddy_list_pkt.build()?;
        client.send_payload(buddy_list_pkt);
        player.buddy_list_synced = true;
    }

    Ok(())
}

pub fn pc_move(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
    time: SystemTime,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = client.get_player_id()?;
    let pkt: &sP_CL2FE_REQ_PC_MOVE = pkt.get()?;
    let pos = Position {
        x: pkt.iX,
        y: pkt.iY,
        z: pkt.iZ,
    };

    let angle = pkt.iAngle;

    // TODO anticheat

    let resp = sP_FE2CL_PC_MOVE {
        iCliTime: pkt.iCliTime,
        iX: pkt.iX,
        iY: pkt.iY,
        iZ: pkt.iZ,
        fVX: pkt.fVX,
        fVY: pkt.fVY,
        fVZ: pkt.fVZ,
        iAngle: pkt.iAngle,
        cKeyValue: pkt.cKeyValue,
        iSpeed: pkt.iSpeed,
        iID: pc_id,
        iSvrTime: util::get_timestamp_ms(time),
    };

    state
        .entity_map
        .for_each_around(EntityID::Player(pc_id), |client| {
            client.send_packet(P_FE2CL_PC_MOVE, &resp);
        });

    let player = state.get_player_mut(pc_id)?;
    let entity_id = player.get_id();
    player.set_position(pos);
    player.set_rotation(angle);
    let chunk = player.get_chunk_coords();
    state.entity_map.update(entity_id, Some(chunk), true);
    Ok(())
}

pub fn pc_jump(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
    time: SystemTime,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = client.get_player_id()?;
    let pkt: &sP_CL2FE_REQ_PC_JUMP = pkt.get()?;
    let pos = Position {
        x: pkt.iX,
        y: pkt.iY,
        z: pkt.iZ,
    };

    let angle = pkt.iAngle;

    // TODO anticheat

    let resp = sP_FE2CL_PC_JUMP {
        iCliTime: pkt.iCliTime,
        iX: pkt.iX,
        iY: pkt.iY,
        iZ: pkt.iZ,
        iVX: pkt.iVX,
        iVY: pkt.iVY,
        iVZ: pkt.iVZ,
        iAngle: pkt.iAngle,
        cKeyValue: pkt.cKeyValue,
        iSpeed: pkt.iSpeed,
        iID: pc_id,
        iSvrTime: util::get_timestamp_ms(time),
    };

    state
        .entity_map
        .for_each_around(EntityID::Player(pc_id), |client| {
            client.send_packet(P_FE2CL_PC_JUMP, &resp);
        });

    let player = state.get_player_mut(pc_id)?;
    let entity_id = player.get_id();
    player.set_position(pos);
    player.set_rotation(angle);
    let chunk = player.get_chunk_coords();
    state.entity_map.update(entity_id, Some(chunk), true);

    Ok(())
}

pub fn pc_stop(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
    time: SystemTime,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = client.get_player_id()?;
    let pkt: &sP_CL2FE_REQ_PC_STOP = pkt.get()?;
    let pos = Position {
        x: pkt.iX,
        y: pkt.iY,
        z: pkt.iZ,
    };

    // TODO anticheat

    let resp = sP_FE2CL_PC_STOP {
        iCliTime: pkt.iCliTime,
        iX: pkt.iX,
        iY: pkt.iY,
        iZ: pkt.iZ,
        iID: pc_id,
        iSvrTime: util::get_timestamp_ms(time),
    };

    state
        .entity_map
        .for_each_around(EntityID::Player(pc_id), |client| {
            client.send_packet(P_FE2CL_PC_STOP, &resp);
        });

    let player = state.get_player_mut(pc_id)?;
    let entity_id = player.get_id();
    player.set_position(pos);
    let chunk = player.get_chunk_coords();
    state.entity_map.update(entity_id, Some(chunk), true);

    Ok(())
}

pub fn pc_movetransportation(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
    time: SystemTime,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = client.get_player_id()?;
    let pkt: &sP_CL2FE_REQ_PC_MOVETRANSPORTATION = pkt.get()?;
    let pos = Position {
        x: pkt.iX,
        y: pkt.iY,
        z: pkt.iZ,
    };

    let angle = pkt.iAngle;

    let _slider = state.get_slider(pkt.iT_ID)?;
    // TODO anticheat

    let resp = sP_FE2CL_PC_MOVETRANSPORTATION {
        iCliTime: pkt.iCliTime,
        iLcX: pkt.iLcX,
        iLcY: pkt.iLcY,
        iLcZ: pkt.iLcZ,
        iX: pkt.iX,
        iY: pkt.iY,
        iZ: pkt.iZ,
        fVX: pkt.fVX,
        fVY: pkt.fVY,
        fVZ: pkt.fVZ,
        iT_ID: pkt.iT_ID,
        iAngle: pkt.iAngle,
        cKeyValue: pkt.cKeyValue,
        iSpeed: pkt.iSpeed,
        iPC_ID: pc_id,
        iSvrTime: util::get_timestamp_ms(time),
    };

    state
        .entity_map
        .for_each_around(EntityID::Player(pc_id), |client| {
            client.send_packet(P_FE2CL_PC_MOVETRANSPORTATION, &resp);
        });

    let player = state.get_player_mut(pc_id)?;

    // TODO anticheat

    let entity_id = player.get_id();
    player.set_position(pos);
    player.set_rotation(angle);
    let chunk = player.get_chunk_coords();
    state.entity_map.update(entity_id, Some(chunk), true);
    Ok(())
}

pub fn pc_transport_warp(
    pkt: Packet,
    client: &FFClient,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let pkt: &sP_CL2FE_REQ_PC_TRANSPORT_WARP = pkt.get()?;

    let slider = state.get_slider(pkt.iTransport_ID)?;
    let resp = sP_FE2CL_REP_PC_TRANSPORT_WARP_SUCC {
        TransportationAppearanceData: slider.get_appearance_data(),
        iLcX: pkt.iLcX,
        iLcY: pkt.iLcY,
        iLcZ: pkt.iLcZ,
    };

    client.send_packet(P_FE2CL_REP_PC_TRANSPORT_WARP_SUCC, &resp);
    Ok(())
}

pub fn pc_vehicle_on(clients: &ClientMap, state: &mut ShardServerState) -> FFResult<()> {
    (|| {
        let client = clients.get_sender();
        let pc_id = client.get_player_id()?;
        let player = state.get_player_mut(pc_id)?;

        if player.instance_id.map_num != ID_OVERWORLD {
            return Err(FFError::build(
                Severity::Warning,
                format!(
                    "Player {} tried to mount a vehicle outside the overworld: {}",
                    pc_id, player.instance_id
                ),
            ));
        }

        let vehicle = player
            .get_item(ItemLocation::Equip, EQUIP_SLOT_VEHICLE as usize)
            .unwrap();
        if vehicle.is_none() {
            return Err(FFError::build(
                Severity::Warning,
                format!(
                    "Player {} tried to mount a vehicle without one equipped",
                    pc_id
                ),
            ));
        }

        let vehicle = vehicle.as_ref().unwrap();

        if let Some(vehicle_speed) = vehicle.get_stats()?.speed {
            player.vehicle_speed = Some(vehicle_speed);
        } else {
            panic_log(&format!("Vehicle has no speed: {:?}", vehicle));
        }

        broadcast_state(pc_id, player.get_state_bit_flag(), state);

        let resp = sP_FE2CL_PC_VEHICLE_ON_SUCC::default();
        client.send_packet(P_FE2CL_PC_VEHICLE_ON_SUCC, &resp);
        Ok(())
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_PC_VEHICLE_ON_FAIL {
            iErrorCode: unused!(),
        };

        clients
            .get_sender()
            .send_packet(P_FE2CL_PC_VEHICLE_ON_FAIL, &resp);
    })
}

pub fn pc_vehicle_off(clients: &ClientMap, state: &mut ShardServerState) -> FFResult<()> {
    (|| {
        let client = clients.get_sender();
        let pc_id = client.get_player_id()?;
        let player = state.get_player_mut(pc_id)?;

        player.vehicle_speed = None;
        broadcast_state(pc_id, player.get_state_bit_flag(), state);

        let resp = sP_FE2CL_PC_VEHICLE_OFF_SUCC::default();
        client.send_packet(P_FE2CL_PC_VEHICLE_OFF_SUCC, &resp);
        Ok(())
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_PC_VEHICLE_OFF_FAIL {
            iErrorCode: unused!(),
        };

        clients
            .get_sender()
            .send_packet(P_FE2CL_PC_VEHICLE_OFF_FAIL, &resp);
    })
}

pub fn pc_special_state_switch(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = client.get_player_id()?;
    let pkt: &sP_CL2FE_REQ_PC_SPECIAL_STATE_SWITCH = pkt.get()?;

    let player = state.get_player_mut(pc_id)?;

    match pkt.iSpecialStateFlag as u32 {
        CN_SPECIAL_STATE_FLAG__FULL_UI => {
            player.in_menu = !player.in_menu;
        }
        _ => {
            return Err(FFError::build(
                Severity::Warning,
                format!(
                    "P_CL2FE_REQ_PC_SPECIAL_STATE_SWITCH: invalid special state flag: {}",
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

pub fn pc_combat_begin_end(
    clients: &ClientMap,
    state: &mut ShardServerState,
    in_combat: bool,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = client.get_player_id()?;

    let player = state.get_player_mut(pc_id)?;
    player.in_combat = in_combat; // TODO anticheat
    if !in_combat {
        player.reset();
    }

    let special_state_flags = player.get_special_state_bit_flag();

    let resp = sP_FE2CL_REP_PC_SPECIAL_STATE_SWITCH_SUCC {
        iPC_ID: pc_id,
        iReqSpecialStateFlag: CN_SPECIAL_STATE_FLAG__COMBAT as i8,
        iSpecialState: special_state_flags,
    };
    state
        .entity_map
        .for_each_around(EntityID::Player(pc_id), |c| {
            c.send_packet(P_FE2CL_PC_SPECIAL_STATE_CHANGE, &resp);
        });
    Ok(())
}

fn get_respawn_point(pos: Position, map_num: u32) -> Position {
    let respawn_pos = tdata_get().get_nearest_respawn_point(pos, map_num);
    respawn_pos.unwrap_or_else(|| {
        log(
            Severity::Warning,
            &format!("Couldn't find a respawn point in map {}", map_num),
        );
        pos
    })
}

pub fn pc_regen(pkt: Packet, clients: &ClientMap, state: &mut ShardServerState) -> FFResult<()> {
    const WARP_AWAY_COOLDOWN: Duration = Duration::from_secs(60);

    let client = clients.get_sender();
    let pc_id = client.get_player_id()?;
    let pkt: &sP_CL2FE_REQ_PC_REGEN = pkt.get()?;
    let revive_type: PCRegenType = pkt.iRegenType.try_into()?;

    let player = state.get_player_mut(pc_id)?;
    let new_chunk_coords = match revive_type {
        PCRegenType::Xcom => {
            if !player.is_dead() {
                return Err(FFError::build(
                    Severity::Warning,
                    format!("{} tried to revive while not dead", player),
                ));
            }
            player.set_position(get_respawn_point(
                player.get_position(),
                player.instance_id.map_num,
            ));
            Some(player.get_chunk_coords())
        }
        PCRegenType::HereByPhoenix => todo!(),
        PCRegenType::HereByPhoenixGroup => todo!(),
        PCRegenType::Unstick => {
            // check warp away timer
            if let Some(last_warp_away) = player.last_warp_away_time {
                let time_now = SystemTime::now();
                if time_now.duration_since(last_warp_away).unwrap_or_default() < WARP_AWAY_COOLDOWN
                {
                    return Err(FFError::build(
                        Severity::Warning,
                        format!("{} tried to warp away too soon", player),
                    ));
                } else {
                    player.last_warp_away_time = Some(time_now);
                }
            }

            player.set_position(get_respawn_point(
                player.get_position(),
                player.instance_id.map_num,
            ));
            Some(player.get_chunk_coords())
        }
        other => {
            return Err(FFError::build(
                Severity::Warning,
                format!("Unsupported regen type: {:?}", other),
            ));
        }
    };
    player.do_revive();

    let (regen_data, regen_data_bcast) = player.get_regen_data();

    let resp = sP_FE2CL_REP_PC_REGEN_SUCC {
        PCRegenData: regen_data,
        iFusionMatter: player.get_fusion_matter() as i32,
        // we do NOT want the client to do GC. this is because we re-chunk the player serverside.
        // we can't de-chunk and wait for the client to ask for re-chunking because the player
        // might be in an instance that would get cleaned up prematurely.
        bMoveLocation: 0,
    };
    client.send_packet(P_FE2CL_REP_PC_REGEN_SUCC, &resp);
    if let Some(new_chunk) = new_chunk_coords {
        state
            .entity_map
            .update(EntityID::Player(pc_id), Some(new_chunk), true);
    }

    let bcast = sP_FE2CL_PC_REGEN {
        PCRegenDataForOtherPC: regen_data_bcast,
    };
    state
        .entity_map
        .for_each_around(EntityID::Player(pc_id), |c| {
            c.send_packet(P_FE2CL_PC_REGEN, &bcast);
        });
    Ok(())
}

pub fn pc_first_use_flag_set(
    pkt: Packet,
    client: &FFClient,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let pc_id = client.get_player_id()?;
    let pkt: &sP_CL2FE_REQ_PC_FIRST_USE_FLAG_SET = pkt.get()?;

    let player = state.get_player_mut(pc_id)?;
    player.update_first_use_flag(pkt.iFlagCode)?;
    Ok(())
}

pub fn pc_change_mentor(
    pkt: Packet,
    client: &FFClient,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let pkt: &sP_CL2FE_REQ_PC_CHANGE_MENTOR = pkt.get()?;
    (|| {
        let player = state.get_player_mut(client.get_player_id()?)?;
        if player.get_level() < 4 {
            return Err(FFError::build(
                Severity::Warning,
                "Player tried to change mentor before level 4".to_string(),
            ));
        }

        let guide_count = player.update_guide(pkt.iMentor.try_into()?);

        let resp = sP_FE2CL_REP_PC_CHANGE_MENTOR_SUCC {
            iMentor: pkt.iMentor,
            iMentorCnt: guide_count as i16,
            iFusionMatter: player.get_fusion_matter() as i32,
        };

        client.send_packet(P_FE2CL_REP_PC_CHANGE_MENTOR_SUCC, &resp);
        Ok(())
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_REP_PC_CHANGE_MENTOR_FAIL {
            iMentor: pkt.iMentor,
            iErrorCode: unused!(),
        };

        client.send_packet(P_FE2CL_REP_PC_CHANGE_MENTOR_FAIL, &resp);
    })
}

pub fn pc_channel_num(client: &FFClient, state: &mut ShardServerState) -> FFResult<()> {
    let player = state.get_player(client.get_player_id()?)?;
    let resp = sP_FE2CL_REP_PC_CHANNEL_NUM {
        iChannelNum: player.instance_id.channel_num as i32,
    };
    client.send_packet(P_FE2CL_REP_PC_CHANNEL_NUM, &resp);
    Ok(())
}

pub fn pc_channel_info(client: &FFClient, state: &mut ShardServerState) -> FFResult<()> {
    let player = state.get_player(client.get_player_id()?)?;
    let channel_num = player.instance_id.channel_num;
    let num_channels = config_get().shard.num_channels.get();
    let mut resp = PacketBuilder::new(P_FE2CL_REP_CHANNEL_INFO).with(&sP_FE2CL_REP_CHANNEL_INFO {
        iCurrChannelNum: channel_num as i32,
        iChannelCnt: num_channels as i32,
    });

    for channel_num in 1..=num_channels {
        resp.push(&sChannelInfo {
            iChannelNum: channel_num as i32,
            iCurrentUserCnt: state.entity_map.get_channel_population(channel_num) as i32,
        });
    }

    let pkt = resp.build()?;
    client.send_payload(pkt);
    Ok(())
}

pub fn pc_warp_channel(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let pkt: &sP_CL2FE_REQ_PC_WARP_CHANNEL = pkt.get()?;

    let mut error_code = 0;
    (|| {
        let client = clients.get_sender();
        let pc_id = client.get_player_id()?;
        let channel_num = pkt.iChannelNum as u8;
        let num_channels = config_get().shard.num_channels.get();

        if channel_num == 0 || channel_num > num_channels {
            error_code = 3; // "the channel number is invalid."
            return Err(FFError::build(
                Severity::Warning,
                format!("Invalid channel number: {}", channel_num),
            ));
        }

        let max_channel_pop = config_get().shard.max_channel_pop.get();
        if state.entity_map.get_channel_population(channel_num) >= max_channel_pop {
            error_code = 4; // "the channel is full."
            return Err(FFError::build(
                Severity::Warning,
                format!("Channel {} is full", channel_num),
            ));
        }

        let player = state.get_player_mut(pc_id)?;
        if player.instance_id.channel_num == channel_num {
            error_code = 2; // "you're already in the channel."
            return Err(FFError::build(
                Severity::Warning,
                format!("Player {} is already in channel {}", pc_id, channel_num),
            ));
        }

        player.instance_id.channel_num = channel_num;
        let chunk_coords = player.get_chunk_coords();

        let resp = sP_FE2CL_REP_PC_WARP_CHANNEL_SUCC::default();
        client.send_packet(P_FE2CL_REP_PC_WARP_CHANNEL_SUCC, &resp);

        state
            .entity_map
            .update(EntityID::Player(pc_id), Some(chunk_coords), true);

        Ok(())
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_REP_PC_WARP_CHANNEL_FAIL {
            iErrorCode: error_code,
        };

        clients
            .get_sender()
            .send_packet(P_FE2CL_REP_PC_WARP_CHANNEL_FAIL, &resp);
    })
}

mod helpers {
    use super::*;

    pub fn build_nano_book(player: &Player) -> FFResult<(Packet, Vec<Packet>)> {
        const PAGE_LEN: usize = 10;
        let book_size = nano_count(&FFProtocol::v1013);

        let mut pkt = v1013::sP_FE2CL_REP_NANO_BOOK_SUBSET {
            PCUID: player.get_uid(),
            bookSize: book_size as i32,
            elementOffset: 0,
            element: [None.into_proto(); PAGE_LEN],
        };
        let resize = Packet::new(P_FE2CL_REP_NANO_BOOK_SUBSET, &pkt)?;

        let mut pages = Vec::with_capacity(book_size.div_ceil(PAGE_LEN));
        for offset in (0..book_size).step_by(PAGE_LEN) {
            pkt.elementOffset = offset as i32;
            for (i, slot) in pkt.element.iter_mut().enumerate() {
                let nano_id = offset + i;
                *slot = if nano_id < book_size {
                    player.get_nano(nano_id as i16).into_proto()
                } else {
                    None.into_proto()
                };
            }
            pages.push(Packet::new(P_FE2CL_REP_NANO_BOOK_SUBSET, &pkt)?);
        }

        Ok((resize, pages))
    }
}

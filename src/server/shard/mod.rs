use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use tokio::{sync::Mutex, task::JoinHandle};

use crate::{
    config::config_get,
    database::{db_get, DbImpl as _},
    defines::EXIT_CODE_REQ_BY_SVR,
    entity::{Entity as _, Player},
    error::*,
    helpers::send_announcement,
    net::{
        packet::{PacketID::*, *},
        ClientMap, ClientType, FFClient, FFServer,
    },
    state::ShardServerState,
    util,
};

mod buddy;
mod chat;
mod combat;
mod gm;
mod group;
mod item;
mod login;
mod mission;
mod nano;
mod npc;
mod pc;
mod racing;
mod trade;
mod transport;

pub fn handle_packet<'a>(
    pkt: Packet,
    key: usize,
    clients: &'a HashMap<usize, FFClient>,
    state: Arc<Mutex<ShardServerState>>,
) -> Pin<Box<dyn Future<Output = FFResult<()>> + Send + 'a>> {
    Box::pin(async move {
        let time = SystemTime::now();
        let clients = ClientMap::new(key, clients);
        let pkt_id = pkt.id();

        // These packet handlers use the state lock directly for efficiency with the DB
        if pkt_id == P_CL2FE_REQ_PC_ENTER {
            return pc::pc_enter(pkt, &clients, state, time).await;
        }

        if pkt_id == P_CL2FE_REQ_PC_EXIT {
            return pc::pc_exit(&clients, state).await;
        }

        if pkt_id == P_CL2FE_REQ_EP_RACE_END {
            return racing::end(pkt, clients.get_sender(), state).await;
        }

        let mut state = state.lock().await;
        let state = &mut *state;
        match pkt_id {
            P_LS2FE_REP_AUTH_CHALLENGE => login::login_connect_challenge(pkt, clients.get_sender()),
            P_LS2FE_REP_CONNECT_SUCC => login::login_connect_succ(pkt, clients.get_sender(), state),
            P_LS2FE_REP_CONNECT_FAIL => login::login_connect_fail(pkt),
            P_LS2FE_REQ_UPDATE_LOGIN_INFO => {
                login::login_update_info(pkt, clients.get_sender(), state)
            }
            P_LS2FE_REQ_LIVE_CHECK => login::login_live_check(clients.get_sender()),
            P_LS2FE_REP_MOTD => login::login_motd(pkt, state),
            P_LS2FE_ANNOUNCE_MSG => login::login_announce_msg(pkt, &clients),
            P_LS2FE_REQ_PC_LOCATION => login::login_pc_location(pkt, &clients, state),
            P_LS2FE_REP_PC_LOCATION_SUCC => login::login_pc_location_succ(pkt, state),
            P_LS2FE_REP_PC_LOCATION_FAIL => login::login_pc_location_fail(pkt, state),
            P_LS2FE_REQ_PC_EXIT_DUPLICATE => login::login_pc_exit_duplicate(pkt, state),
            P_LS2FE_REP_GET_BUDDY_STATE => login::login_get_buddy_state(pkt, state),
            P_LS2FE_REQ_SEND_BUDDY_FREECHAT => login::login_buddy_freechat(pkt, &clients, state),
            P_LS2FE_REP_SEND_BUDDY_FREECHAT_SUCC => login::buddy_freechat_succ(pkt, state),
            P_LS2FE_REQ_SEND_BUDDY_MENUCHAT => login::login_buddy_menuchat(pkt, &clients, state),
            P_LS2FE_REP_SEND_BUDDY_MENUCHAT_SUCC => login::buddy_menuchat_succ(pkt, state),
            P_LS2FE_REQ_BUDDY_WARP => login::login_buddy_warp(pkt, &clients, state),
            P_LS2FE_REP_BUDDY_WARP_SUCC => login::login_buddy_warp_succ(pkt, state).await,
            P_LS2FE_REP_BUDDY_WARP_FAIL => login::login_buddy_warp_fail(pkt, state),
            P_LS2FE_REP_LIVE_CHECK => {
                clients.get_sender().clear_live_check();
                Ok(())
            }
            //
            P_CL2LS_REQ_LOGIN => wrong_server(pkt, clients.get_sender()),
            //
            P_CL2FE_REQ_PC_ENTER => unreachable!(),
            P_CL2FE_REQ_PC_LOADING_COMPLETE => pc::pc_loading_complete(pkt, &clients, state),
            P_CL2FE_REQ_PC_CHANNEL_NUM => pc::pc_channel_num(clients.get_sender(), state),
            P_CL2FE_REQ_CHANNEL_INFO => pc::pc_channel_info(clients.get_sender(), state),
            P_CL2FE_REQ_PC_WARP_CHANNEL => pc::pc_warp_channel(pkt, &clients, state),
            P_CL2FE_REQ_PC_MOVE => pc::pc_move(pkt, &clients, state, time),
            P_CL2FE_REQ_PC_JUMP => pc::pc_jump(pkt, &clients, state, time),
            P_CL2FE_REQ_PC_STOP => pc::pc_stop(pkt, &clients, state, time),
            P_CL2FE_REQ_PC_MOVETRANSPORTATION => {
                pc::pc_movetransportation(pkt, &clients, state, time)
            }
            P_CL2FE_REQ_PC_TRANSPORT_WARP => {
                pc::pc_transport_warp(pkt, clients.get_sender(), state)
            }
            P_CL2FE_REQ_PC_VEHICLE_ON => pc::pc_vehicle_on(&clients, state),
            P_CL2FE_REQ_PC_VEHICLE_OFF => pc::pc_vehicle_off(&clients, state),
            P_CL2FE_REQ_PC_SPECIAL_STATE_SWITCH => {
                pc::pc_special_state_switch(pkt, &clients, state)
            }
            P_CL2FE_REQ_PC_COMBAT_BEGIN => pc::pc_combat_begin_end(&clients, state, true),
            P_CL2FE_REQ_PC_COMBAT_END => pc::pc_combat_begin_end(&clients, state, false),
            P_CL2FE_REQ_PC_REGEN => pc::pc_regen(pkt, &clients, state),
            P_CL2FE_REQ_PC_FIRST_USE_FLAG_SET => {
                pc::pc_first_use_flag_set(pkt, clients.get_sender(), state)
            }
            P_CL2FE_REQ_PC_CHANGE_MENTOR => pc::pc_change_mentor(pkt, clients.get_sender(), state),
            P_CL2FE_REQ_PC_EXIT => unreachable!(),
            //
            P_CL2FE_REQ_PC_GIVE_ITEM => gm::gm_pc_give_item(pkt, clients.get_sender(), state),
            P_CL2FE_GM_REQ_PC_SET_VALUE => gm::gm_pc_set_value(pkt, &clients, state),
            P_CL2FE_REQ_PC_GIVE_NANO => gm::gm_pc_give_nano(pkt, &clients, state),
            P_CL2FE_REQ_PC_GOTO => gm::gm_pc_goto(pkt, &clients, state),
            P_CL2FE_GM_REQ_PC_SPECIAL_STATE_SWITCH => {
                gm::gm_pc_special_state_switch(pkt, &clients, state)
            }
            P_CL2FE_GM_REQ_PC_MOTD_REGISTER => gm::gm_pc_motd_register(pkt, &clients, state),
            P_CL2FE_GM_REQ_PC_ANNOUNCE => gm::gm_pc_announce(pkt, &clients, state),
            P_CL2FE_GM_REQ_PC_LOCATION => gm::gm_pc_location(pkt, &clients, state),
            P_CL2FE_GM_REQ_TARGET_PC_SPECIAL_STATE_ONOFF => {
                gm::gm_target_pc_special_state_onoff(pkt, &clients, state)
            }
            P_CL2FE_GM_REQ_TARGET_PC_TELEPORT => gm::gm_target_pc_teleport(pkt, &clients, state),
            P_CL2FE_GM_REQ_KICK_PLAYER => gm::gm_kick_player(pkt, &clients, state),
            P_CL2FE_GM_REQ_REWARD_RATE => gm::gm_reward_rate(pkt, clients.get_sender(), state),
            P_CL2FE_REQ_PC_TASK_COMPLETE => {
                gm::gm_pc_task_complete(pkt, clients.get_sender(), state)
            }
            P_CL2FE_REQ_PC_MISSION_COMPLETE => {
                gm::gm_pc_mission_complete(pkt, clients.get_sender(), state)
            }
            P_CL2FE_REQ_NPC_SUMMON => gm::gm_npc_summon(pkt, &clients, state),
            P_CL2FE_REQ_NPC_GROUP_SUMMON => gm::gm_npc_group_summon(pkt, &clients, state),
            P_CL2FE_REQ_NPC_UNSUMMON => gm::gm_npc_unsummon(pkt, &clients, state),
            P_CL2FE_REQ_SHINY_SUMMON => gm::gm_shiny_summon(pkt, &clients, state),
            //
            P_CL2FE_REQ_NPC_INTERACTION => npc::npc_interaction(pkt, clients.get_sender(), state),
            P_CL2FE_REQ_EP_RACE_START => racing::start(pkt, clients.get_sender(), state),
            P_CL2FE_REQ_EP_GET_RING => racing::get_ring(pkt, clients.get_sender(), state),
            P_CL2FE_REQ_EP_RACE_CANCEL => racing::cancel(clients.get_sender(), state),
            P_CL2FE_REQ_EP_RACE_END => unreachable!(),
            P_CL2FE_REQ_BARKER => npc::npc_bark(pkt, clients.get_sender(), state),
            //
            P_CL2FE_REQ_SEND_FREECHAT_MESSAGE => {
                chat::send_freechat_message(pkt, &clients, state).await
            }
            P_CL2FE_REQ_SEND_MENUCHAT_MESSAGE => chat::send_menuchat_message(pkt, &clients, state),
            P_CL2FE_REQ_SEND_ALL_GROUP_FREECHAT_MESSAGE => {
                chat::send_group_freechat_message(pkt, &clients, state)
            }
            P_CL2FE_REQ_SEND_ALL_GROUP_MENUCHAT_MESSAGE => {
                chat::send_group_menuchat_message(pkt, &clients, state)
            }
            P_CL2FE_REQ_PC_AVATAR_EMOTES_CHAT => chat::pc_avatar_emotes_chat(pkt, &clients, state),
            P_CL2FE_REQ_SEND_BUDDY_FREECHAT_MESSAGE => {
                chat::send_buddy_freechat_message(pkt, &clients, state)
            }
            P_CL2FE_REQ_SEND_BUDDY_MENUCHAT_MESSAGE => {
                chat::send_buddy_menuchat_message(pkt, &clients, state)
            }
            //
            P_CL2FE_REQ_PC_ATTACK_NPCs => combat::pc_attack_npcs(pkt, &clients, state),
            P_CL2FE_REQ_PC_ATTACK_CHARs => combat::pc_attack_pcs(pkt, &clients, state),
            P_CL2FE_REQ_NANO_SKILL_USE => combat::nano_skill_use(pkt, &clients, state),
            //
            P_CL2FE_REQ_ITEM_USE => crate::boosters::use_item(pkt, &clients, state),
            P_CL2FE_REQ_ITEM_MOVE => item::item_move(pkt, &clients, state),
            P_CL2FE_REQ_PC_ITEM_DELETE => item::item_delete(pkt, clients.get_sender(), state),
            P_CL2FE_REQ_PC_ITEM_COMBINATION => {
                item::item_combination(pkt, clients.get_sender(), state)
            }
            P_CL2FE_REQ_ITEM_CHEST_OPEN => item::item_chest_open(pkt, clients.get_sender(), state),
            P_CL2FE_REQ_PC_VENDOR_START => item::vendor_start(pkt, clients.get_sender(), state),
            P_CL2FE_REQ_PC_VENDOR_TABLE_UPDATE => {
                item::vendor_table_update(pkt, clients.get_sender())
            }
            P_CL2FE_REQ_PC_VENDOR_ITEM_BUY => {
                item::vendor_item_buy(pkt, clients.get_sender(), state, time)
            }
            P_CL2FE_REQ_PC_VENDOR_ITEM_SELL => {
                item::vendor_item_sell(pkt, clients.get_sender(), state)
            }
            P_CL2FE_REQ_PC_VENDOR_ITEM_RESTORE_BUY => {
                item::vendor_item_restore_buy(pkt, clients.get_sender(), state)
            }
            P_CL2FE_REQ_PC_VENDOR_BATTERY_BUY => {
                item::vendor_battery_buy(pkt, clients.get_sender(), state)
            }
            P_CL2FE_PC_STREETSTALL_REQ_CANCEL => item::streetstall_cancel(clients.get_sender()),
            //
            P_CL2FE_REQ_NANO_EQUIP => nano::nano_equip(pkt, &clients, state),
            P_CL2FE_REQ_NANO_UNEQUIP => nano::nano_unequip(pkt, &clients, state),
            P_CL2FE_REQ_NANO_ACTIVE => nano::nano_active(pkt, &clients, state),
            P_CL2FE_REQ_NANO_TUNE => nano::nano_tune(pkt, clients.get_sender(), state),
            P_CL2FE_REQ_CHARGE_NANO_STAMINA => {
                nano::charge_nano_stamina(clients.get_sender(), state)
            }
            //
            P_CL2FE_REQ_REQUEST_MAKE_BUDDY => buddy::request_make_buddy(pkt, &clients, state),
            P_CL2FE_REQ_ACCEPT_MAKE_BUDDY => buddy::accept_make_buddy(pkt, &clients, state),
            P_CL2FE_REQ_PC_FIND_NAME_MAKE_BUDDY => {
                buddy::find_name_make_buddy(pkt, &clients, state)
            }
            P_CL2FE_REQ_PC_FIND_NAME_ACCEPT_BUDDY => {
                buddy::find_name_accept_buddy(pkt, &clients, state)
            }
            P_CL2FE_REQ_GET_BUDDY_STATE => buddy::get_buddy_state(&clients, state),
            P_CL2FE_REQ_PC_BUDDY_WARP => buddy::pc_buddy_warp(pkt, &clients, state),
            //
            P_CL2FE_REQ_PC_TRADE_OFFER => trade::trade_offer(pkt, &clients, state),
            P_CL2FE_REQ_PC_TRADE_OFFER_REFUSAL => trade::trade_offer_refusal(pkt, &clients, state),
            P_CL2FE_REQ_PC_TRADE_OFFER_ACCEPT => trade::trade_offer_accept(pkt, &clients, state),
            P_CL2FE_REQ_PC_TRADE_OFFER_CANCEL => trade::trade_offer_cancel(pkt, &clients, state),
            P_CL2FE_REQ_PC_TRADE_CASH_REGISTER => trade::trade_cash_register(pkt, &clients, state),
            P_CL2FE_REQ_PC_TRADE_ITEM_REGISTER => trade::trade_item_register(pkt, &clients, state),
            P_CL2FE_REQ_PC_TRADE_ITEM_UNREGISTER => {
                trade::trade_item_unregister(pkt, &clients, state)
            }
            P_CL2FE_REQ_PC_TRADE_CONFIRM_CANCEL => {
                trade::trade_confirm_cancel(pkt, &clients, state)
            }
            P_CL2FE_REQ_PC_TRADE_CONFIRM => trade::trade_confirm(&clients, state).await,
            P_CL2FE_REQ_PC_TRADE_EMOTES_CHAT => trade::trade_emotes_chat(pkt, &clients, state),
            //
            P_CL2FE_REQ_REGIST_TRANSPORTATION_LOCATION => {
                transport::regist_transportation_location(pkt, clients.get_sender(), state)
            }
            P_CL2FE_REQ_PC_WARP_USE_TRANSPORTATION => {
                transport::warp_use_transportation(pkt, &clients, state)
            }
            P_CL2FE_REQ_PC_WARP_USE_NPC => transport::warp_use_npc(pkt, &clients, state),
            P_CL2FE_REQ_PC_TIME_TO_GO_WARP => transport::time_to_go_warp(pkt, &clients, state),
            //
            P_CL2FE_REQ_PC_TASK_START => mission::task_start(pkt, clients.get_sender(), state),
            P_CL2FE_REQ_PC_TASK_STOP => mission::task_stop(pkt, clients.get_sender(), state),
            P_CL2FE_REQ_PC_TASK_END => mission::task_end(pkt, &clients, state),
            P_CL2FE_REQ_PC_SET_CURRENT_MISSION_ID => {
                mission::set_current_mission_id(pkt, clients.get_sender(), state)
            }
            //
            P_CL2FE_REQ_PC_GROUP_INVITE => group::pc_group_invite(pkt, &clients, state),
            P_CL2FE_REQ_PC_GROUP_INVITE_REFUSE => {
                group::pc_group_invite_refuse(pkt, &clients, state)
            }
            P_CL2FE_REQ_PC_GROUP_JOIN => group::pc_group_join(pkt, &clients, state),
            P_CL2FE_REQ_PC_GROUP_LEAVE => group::pc_group_leave(&clients, state),
            P_CL2FE_REQ_NPC_GROUP_INVITE => group::npc_group_invite(pkt, &clients, state),
            P_CL2FE_REQ_NPC_GROUP_KICK => group::npc_group_kick(pkt, &clients, state),
            //
            P_CL2FE_REP_LIVE_CHECK => {
                clients.get_sender().clear_live_check();
                Ok(())
            }
            //
            _ => Err(FFError::build(
                Severity::Warning,
                "Unhandled packet".to_string(),
            )),
        }
    })
}

pub fn handle_disconnect(
    key: usize,
    clients: &HashMap<usize, FFClient>,
    state: &mut ShardServerState,
) {
    let clients = ClientMap::new(key, clients);
    let client = clients.get_sender();
    match client.get_client_type() {
        ClientType::LoginServer => {
            log(
                Severity::Warning,
                &format!("Login server ({}) disconnected", client.get_addr()),
            );
            state.login_server_conn_id = None;
        }
        ClientType::GameClient {
            pc_id: Some(pc_id), ..
        } => {
            // dirty exit; clean exit happens in P_CL2FE_REQ_PC_EXIT handler
            let player = Player::remove_from_state(pc_id, state);
            tokio::spawn(async move {
                let db = db_get();
                log_if_failed(db.save_player(&player).await);
            });
        }
        ClientType::Unknown => {
            log(
                Severity::Debug,
                &format!("Client disconnected: {}", client.get_addr()),
            );
        }
        _ => {
            log(
                Severity::Warning,
                &format!(
                    "Unhandled disconnect for client {} (type {})",
                    client.get_addr(),
                    client.get_client_type()
                ),
            );
        }
    }
}

pub fn send_live_check(client: &FFClient) {
    match client.get_client_type() {
        ClientType::GameClient { .. } => {
            let pkt = sP_FE2CL_REQ_LIVE_CHECK {
                iTempValue: unused!(),
            };
            client.send_packet(P_FE2CL_REQ_LIVE_CHECK, &pkt);
        }
        ClientType::LoginServer => {
            let pkt = sP_FE2LS_REQ_LIVE_CHECK {
                iTempValue: unused!(),
            };
            client.send_packet(P_FE2LS_REQ_LIVE_CHECK, &pkt);
        }
        _ => {}
    }
}

pub fn do_save(state: &ShardServerState) -> Option<JoinHandle<FFResult<(usize, Duration)>>> {
    let pc_ids: Vec<i32> = state.entity_map.get_player_ids().collect();
    if pc_ids.is_empty() {
        return None;
    }

    let time_start = Instant::now();
    log(
        Severity::Info,
        &format!("Saving {} player(s)...", pc_ids.len()),
    );

    let players: Vec<Player> = pc_ids
        .iter()
        .map(|pc_id| state.get_player(*pc_id).unwrap().clone())
        .collect();

    let handle = tokio::spawn(async move {
        let db = db_get();
        let player_refs: Vec<&Player> = players.iter().collect();
        db.save_players(&player_refs).await.map_err(|e| {
            FFError::build(Severity::Warning, "Failed to autosave players".to_string())
                .with_parent(e)
        })?;

        let duration = time_start.elapsed();
        Ok((players.len(), duration))
    });

    Some(handle)
}

pub fn shutdown_notify_clients(clients: &ClientMap, state: &ShardServerState) {
    let reconnect = if let Some(login_server) = clients.get_login_server() {
        let pkt = sP_FE2LS_DISCONNECTING {
            iTempValue: unused!(),
        };

        login_server.send_packet(P_FE2LS_DISCONNECTING, &pkt);

        true
    } else {
        false
    };

    for client in clients.get_all_gameclient() {
        let Ok(pc_id) = client.get_player_id() else {
            continue;
        };

        if !reconnect {
            let shutdown_pkt = sP_FE2CL_REP_PC_EXIT_SUCC {
                iID: pc_id,
                iExitCode: EXIT_CODE_REQ_BY_SVR as i32, // "You have lost your connection with the server."
            };

            client.send_packet(P_FE2CL_REP_PC_EXIT_SUCC, &shutdown_pkt);
            continue;
        }

        // We trick the client into attempting to reconnect to this same shard.
        // The login server will attempt to reconnect them for a short amount of time.
        send_announcement(
            client,
            "Lost connection to shard server.\nAttemping to reconnect...",
            Some(5),
        )
        .unwrap();

        let Ok(player) = state.get_player(pc_id) else {
            continue;
        };

        let channel_num = if player.instance_id.channel_num > 1 {
            Some(player.instance_id.channel_num as i32)
        } else {
            // Channel 1 is the default; we don't need to tell the client to switch
            None
        };

        let dc_pkt = sP_FE2CL_REP_PC_BUDDY_WARP_OTHER_SHARD_SUCC {
            iBuddyPCUID: unused!(),
            iShardNum: 0, // if this shard is going down, we should not tell clients to stay on it
            iChannelNum: channel_num.unwrap_or(0),
        };

        client.send_packet(P_FE2CL_REP_PC_BUDDY_WARP_OTHER_SHARD_SUCC, &dc_pkt);
    }
}

pub fn send_status_to_login_server(clients: &ClientMap, state: &ShardServerState) -> FFResult<()> {
    if !is_login_server_connected(state) {
        return Ok(());
    }

    let Some(client) = clients.get_login_server() else {
        return Ok(());
    };

    let pc_ids: Vec<i32> = state.entity_map.get_player_ids().collect();
    let mut pkt =
        PacketBuilder::new(P_FE2LS_UPDATE_PC_STATUSES).with(&sP_FE2LS_UPDATE_PC_STATUSES {
            iCnt: pc_ids.len() as u32,
        });

    for pc_id in pc_ids {
        let player = state.get_player(pc_id).unwrap();
        let pos = player.get_position();
        pkt.push(&sPlayerMetadata {
            iPC_UID: player.get_uid(),
            szFirstName: util::encode_utf16(&player.first_name).unwrap(),
            szLastName: util::encode_utf16(&player.last_name).unwrap(),
            iX: pos.x,
            iY: pos.y,
            iZ: pos.z,
            iChannelNum: player.instance_id.channel_num as i8,
        });
    }

    let pkt = pkt.build()?;
    client.send_payload(pkt);
    Ok(())
}

pub async fn connect_to_login_server(
    shard_server: &mut FFServer<ShardServerState>,
    state: &mut ShardServerState,
) -> FFResult<()> {
    if is_login_server_connected(state) {
        return Ok(());
    }

    let login_server_addr = config_get().shard.login_server_addr.get();
    log(
        Severity::Info,
        &format!("Connecting to login server at {}...", login_server_addr),
    );

    let conn = shard_server
        .connect(login_server_addr, ClientType::LoginServer)
        .await;

    if let Some(login_server) = &conn {
        login_connect_req(login_server);
    }

    Ok(())
}

fn login_connect_req(server: &FFClient) {
    let pkt = sP_FE2LS_REQ_AUTH_CHALLENGE {
        iTempValue: unused!(),
    };
    server.send_packet(P_FE2LS_REQ_AUTH_CHALLENGE, &pkt);
}

fn is_login_server_connected(state: &ShardServerState) -> bool {
    state.login_server_conn_id.is_some()
}

fn wrong_server(pkt: Packet, client: &FFClient) -> FFResult<()> {
    let pkt: &sP_CL2LS_REQ_LOGIN = pkt.get()?;
    let resp = sP_LS2CL_REP_LOGIN_FAIL {
        iErrorCode: 4, // "Login error"
        szID: pkt.szID,
    };

    client.send_packet(P_LS2CL_REP_LOGIN_FAIL, &resp);
    Ok(())
}

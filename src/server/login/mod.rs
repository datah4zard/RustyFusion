use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, time::SystemTime};

use ffmonitor::PlayerEvent;
use tokio::sync::Mutex;

use crate::{
    error::*,
    monitor::{monitor_flush, monitor_queue, MonitorEvent},
    net::{
        packet::{PacketID::*, *},
        ClientType, FFClient,
    },
    state::LoginServerState,
};

mod login;
mod shard;

pub fn handle_packet<'a>(
    pkt: Packet,
    key: usize,
    clients: &'a HashMap<usize, FFClient>,
    state: Arc<Mutex<LoginServerState>>,
) -> Pin<Box<dyn Future<Output = FFResult<()>> + Send + 'a>> {
    Box::pin(async move {
        let time = SystemTime::now();
        let mut state = state.lock().await;
        let state = &mut *state;
        let client = clients.get(&key).unwrap();
        match pkt.id() {
            P_FE2LS_REQ_AUTH_CHALLENGE => shard::auth_challenge(client),
            P_FE2LS_REQ_CONNECT => shard::connect(pkt, client, state, time),
            P_FE2LS_REP_UPDATE_LOGIN_INFO_SUCC => {
                shard::update_login_info_succ(pkt, key, clients, state)
            }
            P_FE2LS_REP_UPDATE_LOGIN_INFO_FAIL => shard::update_login_info_fail(pkt, clients),
            P_FE2LS_UPDATE_PC_STATUSES => shard::update_pc_statuses(pkt, client, state),
            P_FE2LS_UPDATE_MONITOR => shard::update_monitor(pkt),
            P_FE2LS_REQ_MOTD => shard::motd(pkt, client),
            P_FE2LS_MOTD_REGISTER => shard::motd_register(pkt),
            P_FE2LS_ANNOUNCE_MSG => shard::announce_msg(pkt, clients),
            P_FE2LS_REQ_PC_LOCATION => shard::pc_location(pkt, key, clients, state),
            P_FE2LS_REP_PC_LOCATION_SUCC => shard::pc_location_succ(pkt, clients, state),
            P_FE2LS_REP_PC_LOCATION_FAIL => shard::pc_location_fail(pkt, key, clients, state),
            P_FE2LS_REQ_GET_BUDDY_STATE => shard::get_buddy_state(pkt, key, clients, state),
            P_FE2LS_DISCONNECTING => shard::handle_disconnecting(key, clients, state),
            P_FE2LS_REQ_LIVE_CHECK => shard::shard_live_check(client),
            P_FE2LS_REQ_SEND_BUDDY_FREECHAT => shard::buddy_freechat(pkt, clients, state),
            P_FE2LS_REP_SEND_BUDDY_FREECHAT_SUCC => shard::buddy_freechat_succ(pkt, clients, state),
            P_FE2LS_REQ_SEND_BUDDY_MENUCHAT => shard::buddy_menuchat(pkt, clients, state),
            P_FE2LS_REP_SEND_BUDDY_MENUCHAT_SUCC => shard::buddy_menuchat_succ(pkt, clients, state),
            P_FE2LS_REQ_BUDDY_WARP => shard::buddy_warp(pkt, key, clients, state),
            P_FE2LS_REP_BUDDY_WARP_SUCC => shard::buddy_warp_succ(pkt, clients, state),
            P_FE2LS_REP_BUDDY_WARP_FAIL => shard::buddy_warp_fail(pkt, clients, state),
            P_FE2LS_REP_LIVE_CHECK => {
                client.clear_live_check();
                Ok(())
            }
            //
            P_CL2LS_REQ_LOGIN => login::login(pkt, client, state, time).await,
            P_CL2LS_REQ_PC_EXIT_DUPLICATE => login::pc_exit_duplicate(key, clients, state),
            P_CL2LS_REQ_SHARD_LIST_INFO => login::shard_list_info(client, state),
            P_CL2LS_REQ_CHECK_CHAR_NAME => login::check_char_name(pkt, client),
            P_CL2LS_REQ_SAVE_CHAR_NAME => login::save_char_name(pkt, client, state).await,
            P_CL2LS_REQ_CHAR_CREATE => login::char_create(pkt, client, state).await,
            P_CL2LS_REQ_CHAR_DELETE => login::char_delete(pkt, client, state).await,
            P_CL2LS_REQ_SAVE_CHAR_TUTOR => login::save_char_tutor(pkt, client, state).await,
            P_CL2LS_REQ_CHAR_SELECT => login::char_select(pkt, key, clients, state).await,
            P_CL2LS_REQ_SHARD_SELECT => login::shard_select(pkt, key, clients, state, time),
            P_CL2LS_REP_LIVE_CHECK => {
                client.clear_live_check();
                Ok(())
            }
            //
            other => Err(FFError::build(
                Severity::Warning,
                format!("Unhandled packet: {:?}", other),
            )),
        }
    })
}

pub fn handle_disconnect(
    key: usize,
    clients: &HashMap<usize, FFClient>,
    state: &mut LoginServerState,
) {
    let client = clients.get(&key).unwrap();
    match client.get_client_type() {
        ClientType::ShardServer(shard_id) => {
            state.unregister_shard(shard_id);
            log(
                Severity::Info,
                &format!(
                    "Shard server #{} ({}) disconnected",
                    shard_id,
                    client.get_addr()
                ),
            );
        }
        ClientType::GameClient { account_id, .. } => {
            log_if_failed(state.end_session(account_id));
            log(
                Severity::Info,
                &format!("Login session ended for account #{}", account_id),
            );
        }
        ClientType::Unknown => {}
        other => {
            log(
                Severity::Warning,
                &format!(
                    "Unhandled disconnect for client {} (type {})",
                    client.get_addr(),
                    other
                ),
            );
        }
    }
}

pub fn send_live_check(client: &FFClient) {
    match client.get_client_type() {
        ClientType::GameClient { .. } => {
            let pkt = sP_LS2CL_REQ_LIVE_CHECK {
                iTempValue: unused!(),
            };
            client.send_packet(P_LS2CL_REQ_LIVE_CHECK, &pkt);
        }
        ClientType::ShardServer(_) | ClientType::UnauthedShardServer(_) => {
            let pkt = sP_LS2FE_REQ_LIVE_CHECK {
                iTempValue: unused!(),
            };
            client.send_packet(P_LS2FE_REQ_LIVE_CHECK, &pkt);
        }
        _ => {}
    }
}

pub fn send_monitor_update(state: &LoginServerState) -> FFResult<()> {
    for data in state.get_all_shard_player_data() {
        monitor_queue(MonitorEvent::Player(PlayerEvent {
            x_coord: data.x_coord,
            y_coord: data.y_coord,
            name: format!("{} {}", data.first_name, data.last_name),
        }));
    }
    monitor_flush()
}

use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use crossterm::event::{self as ce, KeyCode};

use futures::StreamExt as _;
use rusty_fusion::{
    config::{config_get, config_init},
    database::db_init,
    defines::*,
    error::{log, log_error, log_if_failed, log_init, FFResult, Logger, Severity},
    net::{ClientMap, FFServer},
    scripting::scripting_init,
    server::shard,
    state::ShardServerState,
    tabledata::tdata_init,
    tui::{ShardTui, Tui as _},
    util,
};
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> FFResult<()> {
    color_eyre::install().unwrap();

    let log_rx = log_init();
    let config = config_init()?;
    let mut logger = Logger::new(log_rx, &config.shard.log_path.get());

    let mut tui = if config.general.enable_tui.get() {
        let terminal = ratatui::init();
        let tui = ShardTui::default();
        let ke = ce::EventStream::new();
        Some((terminal, tui, ke))
    } else {
        None
    };

    tdata_init()?;
    scripting_init()?;

    let mut tui_timer = util::make_timer(Duration::from_millis(250), true);
    let mut logger_timer = util::make_timer(
        Duration::from_secs(config.general.log_write_interval.get()),
        false,
    );
    let mut login_conn_timer = util::make_timer(
        Duration::from_secs(config.shard.login_server_conn_interval.get()),
        true,
    );
    let mut db_conn_timer = util::make_timer(
        Duration::from_secs(config.general.db_conn_retry_interval.get()),
        true,
    );
    let mut save_timer = util::make_timer(
        Duration::from_secs(config.shard.autosave_interval.get() * 60),
        false,
    );
    let mut status_timer = util::make_timer(
        Duration::from_secs(config.shard.login_server_update_interval.get()),
        false,
    );
    let mut item_expiry_timer = util::make_timer(Duration::from_secs(60), false);
    let mut entity_timer = util::make_timer(
        Duration::from_millis(1000 / SHARD_TICKS_PER_SECOND as u64),
        false,
    );
    let mut slow_timer = util::make_timer(Duration::from_secs(1), false);

    let state = Arc::new(Mutex::new(ShardServerState::default()));
    let live_check_time = Duration::from_secs(config.general.live_check_time.get());
    let listen_addr = config_get().shard.listen_addr.get();
    let mut server = FFServer::new(
        listen_addr,
        shard::handle_packet,
        Some(shard::handle_disconnect),
        Some((live_check_time, shard::send_live_check)),
        state.clone(),
    )
    .await?;

    log(
        Severity::Info,
        &format!("Shard server listening on {}", server.get_endpoint()),
    );

    let mut fatal_error = None;
    let mut save_handle = None;
    loop {
        tokio::select! {
            res = server.poll() => {
                if let Err(e) = res {
                    let fatal = e.get_severity() == Severity::Fatal;
                    if fatal {
                        log_error(e.clone());
                        fatal_error = Some(e);
                        break;
                    }

                    log_error(e);
                }
            }
            ke = async { tui.as_mut().unwrap().2.next().await }, if tui.is_some() => {
                match ke {
                    Some(Ok(event)) => {
                        if let ce::Event::Key(key_event) = event {
                            if util::is_ctrl_c(&key_event) {
                                break;
                            }

                            let t = &mut tui.as_mut().unwrap().1;
                            match key_event.code {
                                KeyCode::Up => t.scroll(1),
                                KeyCode::Down => t.scroll(-1),
                                KeyCode::PageUp => t.scroll(10),
                                KeyCode::PageDown => t.scroll(-10),
                                KeyCode::Esc => t.reset_scroll(),
                                _ => {}
                            }
                        }
                    }
                    Some(Err(e)) => {
                        log(Severity::Warning, &format!("Error reading key event: {}", e));
                    }
                    None => {
                        tui = None;
                        ratatui::restore();
                        logger.disable_buffer();
                        log(
                            Severity::Warning,
                            "Key event stream ended; TUI disabled",
                        );
                    }
                }
            }
            _ = tokio::signal::ctrl_c(), if tui.is_none() => {
                break;
            }
            _ = tui_timer.tick() => {
                logger.drain();
                if let Some((terminal, tui, _)) = &mut tui {
                    let clients = server.get_clients().await;
                    let state = state.lock().await;
                    if let Err(e) = terminal.draw(|frame| tui.render(frame, &state, &clients, logger.buffer().unwrap())) {
                        log(
                            Severity::Warning,
                            &format!("Failed to draw TUI; skipping this frame: {}", e),
                        );
                    }
                }
            }
            _ = entity_timer.tick() => {
                state.lock().await
                    .tick_entities(SystemTime::now());
            }
            _ = slow_timer.tick() => {
                let mut state = state.lock().await;
                state.tick_garbage_collection();
                state.tick_groups();
            }
            _ = item_expiry_timer.tick() => {
                state.lock().await
                    .check_for_expired_items(SystemTime::now());
            }
            _ = login_conn_timer.tick() => {
                log_if_failed(shard::connect_to_login_server(&mut server, &mut *state.lock().await).await);
            }
            _ = db_conn_timer.tick() => {
                log_if_failed(db_init(Severity::Fatal).await);
            }
            _ = status_timer.tick() => {
                let clients = server.get_clients().await;
                let client_map = ClientMap::new(0, &clients);
                log_if_failed(shard::send_status_to_login_server(&client_map, &*state.lock().await));
            }
            _ = save_timer.tick() => {
                if save_handle.is_none() {
                    let state = state.lock().await;
                    save_handle = shard::do_save(&state);
                }
            }
            res = async { save_handle.as_mut().unwrap().await }, if save_handle.is_some() => {
                save_handle = None;
                match res.unwrap() {
                    Ok((num_players, time_taken)) => {
                        log(
                            Severity::Info,
                            &format!("Saved {} player(s) in {}ms", num_players, time_taken.as_millis()),
                        );
                    }
                    Err(e) => {
                        fatal_error = Some(e);
                        break;
                    }
                }
            }
            _ = logger_timer.tick() => {
                logger.flush();
            }
        }
    }

    // final TUI render before cleanup
    log(Severity::Info, "Shard server shutting down...");
    logger.drain();

    let clients = server.get_clients().await;
    let state = state.lock().await;

    if let Some((terminal, tui, _)) = &mut tui {
        let _ =
            terminal.draw(|frame| tui.render(frame, &state, &clients, logger.buffer().unwrap()));
    }

    // save players
    if let Some(handle) = shard::do_save(&state) {
        let _ = handle.await;
    }

    let client_map = ClientMap::new(0, &clients);
    shard::shutdown_notify_clients(&client_map, &state);

    // disable TUI
    if tui.is_some() {
        ratatui::restore();
    }

    if let Some(e) = fatal_error {
        Err(e)
    } else {
        Ok(())
    }
}

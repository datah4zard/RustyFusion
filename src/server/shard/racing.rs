use crate::{
    config::config_get,
    database::{db_get, DbImpl},
    defines::*,
    entity::{Entity, EntityID},
    enums::{ItemLocation, ItemType, RewardCategory},
    error::*,
    item::{Item, Reward},
    net::{
        packet::{PacketID::*, *},
        FFClient, FFProtocol,
    },
    racing::{get_time_seconds, RaceRanking, RaceState},
    state::ShardServerState,
    tabledata::tdata_get,
    util,
};
use std::{collections::HashSet, sync::Arc, time::SystemTime};
use tokio::sync::Mutex;

pub fn start(pkt: Packet, client: &FFClient, state: &mut ShardServerState) -> FFResult<()> {
    let req: &sP_CL2FE_REQ_EP_RACE_START = pkt.get()?;
    let pc_id = client.get_player_id()?;
    let npc = state.get_npc(req.iStartEcomID)?;
    let player = state.get_player(pc_id)?;
    state
        .entity_map
        .validate_proximity(&[EntityID::Player(pc_id), npc.get_id()], RANGE_INTERACT)?;
    if npc.instance_id != player.instance_id || !(0..=1).contains(&req.iEPRaceMode) {
        return Err(FFError::build(
            Severity::Warning,
            "Invalid race start".into(),
        ));
    }
    if player.race.as_ref().is_some_and(|race| race.finishing) {
        return Err(FFError::build(
            Severity::Warning,
            "Race completion is pending".into(),
        ));
    }
    let info = tdata_get().get_race_info(player.instance_id.map_num)?;
    let instance = player.instance_id;
    state.get_player_mut(pc_id)?.race = Some(RaceState {
        instance,
        mode: req.iEPRaceMode,
        started: get_time_seconds(),
        id: uuid::Uuid::new_v4(),
        finishing: false,
        pods: HashSet::new(),
    });
    client.send_packet(
        P_FE2CL_REP_EP_RACE_START_SUCC,
        &sP_FE2CL_REP_EP_RACE_START_SUCC {
            iStartTick: 0,
            iLimitTime: info.time_limit as i32,
        },
    );
    Ok(())
}

pub fn get_ring(pkt: Packet, client: &FFClient, state: &mut ShardServerState) -> FFResult<()> {
    let req: &sP_CL2FE_REQ_EP_GET_RING = pkt.get()?;
    let player = state.get_player_mut(client.get_player_id()?)?;
    let Some(race) = player.race.as_mut() else {
        return Ok(());
    };
    let info = tdata_get().get_race_info(player.instance_id.map_num)?;
    if race.finishing
        || race.instance != player.instance_id
        || info.has_timed_out(get_time_seconds().saturating_sub(race.started))
        || race.pods.len() >= info.total_pods
        || !race.pods.insert(req.iRingLID)
    {
        return Ok(());
    }
    client.send_packet(
        P_FE2CL_REP_EP_GET_RING_SUCC,
        &sP_FE2CL_REP_EP_GET_RING_SUCC {
            iRingLID: req.iRingLID,
            iRingCount_Get: race.pods.len() as i32,
        },
    );
    Ok(())
}

pub fn cancel(client: &FFClient, state: &mut ShardServerState) -> FFResult<()> {
    let pc_id = client.get_player_id()?;
    let player = state.get_player_mut(pc_id)?;
    // An accepted finish owns the race until its completion task returns.
    if player.race.as_ref().is_none_or(|race| race.finishing) {
        return Ok(());
    }
    player.race = None;
    client.send_packet(
        P_FE2CL_REP_EP_RACE_CANCEL_SUCC,
        &sP_FE2CL_REP_EP_RACE_CANCEL_SUCC::default(),
    );
    let pos = tdata_get()
        .get_nearest_respawn_point(player.get_position(), player.instance_id.map_num)
        .unwrap_or(player.get_position());
    player.set_position(pos);
    client.send_packet(
        P_FE2CL_REP_PC_GOTO_SUCC,
        &sP_FE2CL_REP_PC_GOTO_SUCC {
            iX: pos.x,
            iY: pos.y,
            iZ: pos.z,
        },
    );
    state.entity_map.reload_player_chunks(pc_id);
    Ok(())
}

pub async fn end(
    pkt: Packet,
    client: &FFClient,
    state_lock: Arc<Mutex<ShardServerState>>,
) -> FFResult<()> {
    let req: &sP_CL2FE_REQ_EP_RACE_END = pkt.get()?;
    let pc_id = client.get_player_id()?;
    // Claim the finish and snapshot its reward while holding the state lock.
    // Duplicate ends, pod requests and new starts cannot change this finish.
    let (race, ranking, fm_reward, info) = {
        let mut state = state_lock.lock().await;
        let npc = state.get_npc(req.iEndEcomID)?;
        let player = state.get_player(pc_id)?;
        state
            .entity_map
            .validate_proximity(&[EntityID::Player(pc_id), npc.get_id()], RANGE_INTERACT)?;
        let Some(race) = player.race.clone() else {
            return Ok(());
        };
        if race.finishing
            || race.instance != player.instance_id
            || npc.instance_id != player.instance_id
        {
            return Ok(());
        }
        let info = tdata_get().get_race_info(player.instance_id.map_num)?;
        let seconds = get_time_seconds().saturating_sub(race.started);
        if info.has_timed_out(seconds) {
            return cancel(client, &mut state);
        }
        let ranking = RaceRanking {
            player_uid: player.get_uid(),
            ep_id: info.ep_id,
            score: info.score(
                race.pods.len(),
                seconds,
                config_get().shard.iz_race_score_capped.get(),
            ),
            pods: race.pods.len() as i32,
            seconds: seconds as i32,
            timestamp: util::get_timestamp_sec(SystemTime::now()) as i64,
        };
        let mut reward = Reward::new(RewardCategory::Racing);
        reward.fusion_matter = info.fusion_matter(race.pods.len());
        let fm_reward = reward.with_player_rates(player).fusion_matter;
        state
            .get_player_mut(pc_id)?
            .race
            .as_mut()
            .unwrap()
            .finishing = true;
        (race, ranking, fm_reward, info)
    };

    // Return to the connection loop so it also releases its clients-map read
    // lock. This owned task carries only the claimed race and client handle.
    let client = client.clone();
    tokio::spawn(async move {
        if let Err(error) = finish(&client, pc_id, state_lock, race, ranking, fm_reward, info).await
        {
            log_error(error);
        }
    });
    Ok(())
}

async fn finish(
    client: &FFClient,
    pc_id: i32,
    state_lock: Arc<Mutex<ShardServerState>>,
    race: RaceState,
    ranking: RaceRanking,
    fm_reward: u32,
    info: &'static crate::racing::RaceInfo,
) -> FFResult<()> {
    // Database waits and retries do not hold the shard-state mutex.
    let result = async {
        db_get().post_race_ranking(&ranking).await?;
        Ok::<_, FFError>(
            db_get()
                .get_top_race_ranking(ranking.player_uid, info.ep_id)
                .await?
                .unwrap_or(ranking.clone()),
        )
    }
    .await;

    let mut state = state_lock.lock().await;
    let Ok(player) = state.get_player_mut(pc_id) else {
        return result.map(|_| ());
    };
    // A disconnect, instance change or replacement race invalidates this finish.
    if player.get_uid() != ranking.player_uid
        || player.instance_id != race.instance
        || !player
            .race
            .as_ref()
            .is_some_and(|active| active.id == race.id && active.finishing)
    {
        return result.map(|_| ());
    }
    let best = match result {
        Ok(best) => best,
        Err(error) => {
            // Ranking persistence failures must not discard the gameplay reward.
            log(
                Severity::Warning,
                &format!(
                    "Racing ranking database operation failed: {}",
                    error.get_msg()
                ),
            );
            ranking.clone()
        }
    };
    let rank = info.rank(ranking.score);
    let old_fm = player.get_fusion_matter();
    let new_fm = player.set_fusion_matter(old_fm.saturating_add(fm_reward));
    let mut item_reward = None;
    if info.rewards[rank] != 0 {
        if let Ok(slot) = player.find_free_slot(ItemLocation::Inven) {
            let item = Item::new(ItemType::Chest, info.rewards[rank]);
            player.set_item(ItemLocation::Inven, slot, Some(item))?;
            item_reward = Some((slot, item));
        }
    }
    player.race = None;
    match config_get().general.protocol.get() {
        FFProtocol::v0104 => client.send_packet(
            P_FE2CL_REP_EP_RACE_END_SUCC,
            &v0104::sP_FE2CL_REP_EP_RACE_END_SUCC {
                iEPRank: rank as i32 + 1,
                iEPRingCnt: ranking.pods,
                iEPScore: ranking.score,
                iEPRaceTime: ranking.seconds,
                iEPRaceMode: race.mode,
                iEPRewardFM: new_fm.saturating_sub(old_fm) as i32,
                iEPTopRank: info.rank(best.score) as i32 + 1,
                iEPTopRingCount: best.pods,
                iEPTopScore: best.score,
                iEPTopTime: best.seconds,
                iFusionMatter: new_fm as i32,
                RewardItem: item_reward
                    .map(|(slot, item)| v0104::sItemReward {
                        sItem: Some(item).into_proto(),
                        eIL: ItemLocation::Inven as i32,
                        iSlotNum: slot as i32,
                    })
                    .unwrap_or_default(),
                iFatigue: 50,
                iFatigue_Level: 1,
            },
        ),
        FFProtocol::v1013 => client.send_packet(
            P_FE2CL_REP_EP_RACE_END_SUCC,
            &v1013::sP_FE2CL_REP_EP_RACE_END_SUCC {
                iEPRank: rank as i32 + 1,
                iEPRingCnt: ranking.pods,
                iEPScore: ranking.score,
                iEPRaceTime: ranking.seconds,
                iEPRaceMode: race.mode,
                iEPRewardFM: new_fm.saturating_sub(old_fm) as i32,
                iEPTopRank: info.rank(best.score) as i32 + 1,
                iEPTopRingCount: best.pods,
                iEPTopScore: best.score,
                iEPTopTime: best.seconds,
                iFusionMatter: new_fm as i32,
                RewardItem: item_reward
                    .map(|(slot, item)| v1013::sItemReward {
                        sItem: Some(item).into_proto(),
                        eIL: ItemLocation::Inven as i32,
                        iSlotNum: slot as i32,
                    })
                    .unwrap_or_default(),
                iFatigue: 50,
                iFatigue_Level: 1,
            },
        ),
    }
    Ok(())
}

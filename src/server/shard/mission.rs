use crate::{
    chunk::TickMode,
    config::config_get,
    defines::{RANGE_INTERACT, RANGE_TRIGGER},
    entity::{Combatant, EntityID},
    enums::{ItemLocation, MissionType, TaskType},
    error::*,
    helpers,
    item::RewardItem,
    mission::Task,
    net::{
        packet::{PacketID::*, *},
        ClientMap, FFClient, FFProtocol,
    },
    state::ShardServerState,
    tabledata::tdata_get,
};

pub fn task_start(pkt: Packet, client: &FFClient, state: &mut ShardServerState) -> FFResult<()> {
    let pkt: &sP_CL2FE_REQ_PC_TASK_START = pkt.get()?;
    (|| {
        let pc_id = client.get_player_id()?;
        let player = state.get_player(pc_id)?;
        let task_def = tdata_get().get_task_definition(pkt.iTaskNum)?;

        // check if already started
        if player
            .mission_journal
            .get_current_tasks()
            .iter()
            .any(|t| t.get_task_id() == pkt.iTaskNum)
        {
            // benign client bug; ignore
            return Ok(());
        }

        // check giver NPC type + proximity
        if let Some(giver_npc_type) = task_def.prereq_npc_type {
            let req_npc_id = pkt.iNPC_ID;
            let req_npc = state.get_npc(req_npc_id)?;
            if req_npc.ty != giver_npc_type {
                return Err(FFError::build(
                    Severity::Warning,
                    format!(
                        "Tried to start task {} from NPC type {}, should be {}",
                        pkt.iTaskNum, req_npc.ty, giver_npc_type
                    ),
                ));
            }
            state.entity_map.validate_proximity(
                &[EntityID::Player(pc_id), EntityID::NPC(req_npc_id)],
                RANGE_INTERACT,
            )?;
        }

        // check min level
        if let Some(min_level) = task_def.prereq_level {
            let player_level = player.get_level();
            if player_level < min_level {
                return Err(FFError::build(
                    Severity::Warning,
                    format!(
                        "Tried to start task {} with level {} < {}",
                        pkt.iTaskNum, player_level, min_level
                    ),
                ));
            }
        }

        // check guide
        if let Some(guide) = task_def.prereq_guide {
            if player.get_guide() != guide {
                return Err(FFError::build(
                    Severity::Warning,
                    format!(
                        "Tried to start task {} with guide {:?} != {:?}",
                        pkt.iTaskNum,
                        player.get_guide(),
                        guide
                    ),
                ));
            }
        }

        // check nanos
        for nano_id in &task_def.prereq_nano_ids {
            if player.get_nano(*nano_id).is_none() {
                return Err(FFError::build(
                    Severity::Warning,
                    format!(
                        "Tried to start task {} without nano {}",
                        pkt.iTaskNum, nano_id
                    ),
                ));
            }
        }

        // check if already completed
        if player
            .mission_journal
            .is_mission_completed(task_def.mission_id)
            .unwrap()
        {
            return Err(FFError::build(
                Severity::Warning,
                format!(
                    "Tried to start task for already completed mission {}",
                    task_def.mission_id
                ),
            ));
        }

        // check prereq missions
        for prereq_mission_id in &task_def.prereq_completed_mission_ids {
            if !player
                .mission_journal
                .is_mission_completed(*prereq_mission_id)?
            {
                return Err(FFError::build(
                    Severity::Warning,
                    format!(
                        "Tried to start task {} without completing prereq mission {}",
                        pkt.iTaskNum, prereq_mission_id
                    ),
                ));
            }
        }

        // check map number
        if let Some(map_num) = task_def.prereq_map_num {
            if player.get_mapnum() != map_num {
                return Err(FFError::build(
                    Severity::Warning,
                    format!(
                        "Tried to start task {} in mapnum {} != {}",
                        pkt.iTaskNum,
                        player.get_mapnum(),
                        map_num
                    ),
                ));
            }
        }

        // check previous task for completion or failure
        if !player
            .mission_journal
            .check_completed_previous_task(task_def)
            && !player.mission_journal.check_failed_previous_task(task_def)
        {
            return Err(FFError::build(
                Severity::Warning,
                format!(
                    "Tried to start task {} without completing previous task",
                    pkt.iTaskNum
                ),
            ));
        }

        // check escort npc
        if let Some(escort_npc_type) = task_def.obj_escort_npc_type {
            let escort_npc_id = pkt.iEscortNPC_ID;
            let escort_npc = state.get_npc(escort_npc_id)?;
            if escort_npc.instance_id != player.instance_id {
                return Err(FFError::build(
                    Severity::Warning,
                    format!(
                        "Tried to start task {} with escort NPC instance {} != player instance {}",
                        pkt.iTaskNum, escort_npc.instance_id, player.instance_id
                    ),
                ));
            }
            if escort_npc.ty != escort_npc_type {
                return Err(FFError::build(
                    Severity::Warning,
                    format!(
                        "Tried to start task {} with escort NPC type {}, should be {}",
                        pkt.iTaskNum, escort_npc.ty, escort_npc_type
                    ),
                ));
            }
        }

        // check for free qitem slots. this is stricter than it needs to be but
        // we can keep it as long as we don't run into problems
        if task_def.given_qitems.len() > player.get_free_slots(ItemLocation::QInven) {
            return Err(FFError::build(
                Severity::Warning,
                format!(
                    "Tried to start task {} with {} qitems but only {} qitem slots",
                    pkt.iTaskNum,
                    task_def.given_qitems.len(),
                    player.get_free_slots(ItemLocation::QInven)
                ),
            ));
        }

        // all clear, start the task
        let mut task: Task = task_def.into();
        let mission_def = task.get_mission_def();

        // start escort path (non-destructive if start_task fails)
        if task_def.obj_escort_npc_type.is_some() {
            let escort_npc_id = pkt.iEscortNPC_ID;
            let escort_npc = state.get_npc_mut(pkt.iEscortNPC_ID).unwrap();
            if let Some(ref mut path) = escort_npc.path {
                path.start();
            } else {
                // Don't override loose follow (for groups)
                if escort_npc.loose_follow.is_none() {
                    escort_npc.loose_follow = Some(EntityID::Player(pc_id));
                }
                state
                    .entity_map
                    .set_tick(EntityID::NPC(escort_npc_id), TickMode::Always)
                    .unwrap();
            }
            task.escort_npc_id = Some(escort_npc_id);
        }

        let player = state.get_player_mut(pc_id).unwrap();
        if player
            .mission_journal
            .start_task(task, player.get_level())?
        {
            log(
                Severity::Info,
                &format!(
                    "{} started mission: {} [{}]",
                    player, mission_def.mission_name, mission_def.mission_id
                ),
            );
        }

        // grant qitems
        if !task_def.given_qitems.is_empty() {
            let mut qitem_pkt =
                PacketBuilder::new(P_FE2CL_REP_REWARD_ITEM).with(&sP_FE2CL_REP_REWARD_ITEM {
                    m_iCandy: player.get_taros() as i32,
                    m_iFusionMatter: player.get_fusion_matter() as i32,
                    m_iBatteryN: player.get_nano_potions() as i32,
                    m_iBatteryW: player.get_weapon_boosts() as i32,
                    iItemCnt: task_def.given_qitems.len() as i8,
                    iFatigue: 100,
                    iFatigue_Level: 1,
                    iNPC_TypeID: 0,
                    iTaskID: task_def.task_id,
                });

            for (qitem_id, qitem_count_mod) in &task_def.given_qitems {
                let curr_count = player.get_quest_item_count(*qitem_id) as isize;
                let new_count = (curr_count + *qitem_count_mod) as usize;
                let qitem_slot = player.set_quest_item_count(*qitem_id, new_count).unwrap();
                helpers::push_item_reward(
                    &mut qitem_pkt,
                    RewardItem::Quest {
                        id: *qitem_id,
                        count: new_count,
                        slot_num: qitem_slot,
                    },
                );
            }

            if let Some(qitem_pkt) = log_if_failed(qitem_pkt.build()) {
                client.send_payload(qitem_pkt);
            }
        }

        let resp = sP_FE2CL_REP_PC_TASK_START_SUCC {
            iTaskNum: pkt.iTaskNum,
            iRemainTime: task_def
                .obj_time_limit
                .map(|d| d.as_secs() as i32)
                .unwrap_or(unused!()),
        };

        client.send_packet(P_FE2CL_REP_PC_TASK_START_SUCC, &resp);
        Ok(())
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_REP_PC_TASK_START_FAIL {
            iTaskNum: pkt.iTaskNum,
            iErrorCode: unused!(),
        };

        client.send_packet(P_FE2CL_REP_PC_TASK_START_FAIL, &resp);
    })
}

pub fn task_stop(pkt: Packet, client: &FFClient, state: &mut ShardServerState) -> FFResult<()> {
    let pkt: &sP_CL2FE_REQ_PC_TASK_STOP = pkt.get()?;
    let pc_id = client.get_player_id()?;
    let player = state.get_player_mut(pc_id)?;

    let task_def = tdata_get().get_task_definition(pkt.iTaskNum)?;
    let mission_def = tdata_get().get_mission_definition(task_def.mission_id)?;
    if mission_def.mission_type == MissionType::Nano {
        return Err(FFError::build(
            Severity::Warning,
            "Tried to delete a nano mission".to_string(),
        ));
    }

    player.mission_journal.remove_task(pkt.iTaskNum)?;

    for item_id in &task_def.delete_qitems {
        let qitem_slot = player.set_quest_item_count(*item_id, 0).unwrap();
        // client doesn't automatically delete qitems clientside
        let pkt = sP_FE2CL_REP_PC_ITEM_DELETE_SUCC {
            eIL: ItemLocation::QInven as i32,
            iSlotNum: qitem_slot as i32,
        };

        client.send_packet(P_FE2CL_REP_PC_ITEM_DELETE_SUCC, &pkt);
    }

    let resp = sP_FE2CL_REP_PC_TASK_STOP_SUCC {
        iTaskNum: pkt.iTaskNum,
    };
    client.send_packet(P_FE2CL_REP_PC_TASK_STOP_SUCC, &resp);
    Ok(())
}

pub fn task_end(pkt: Packet, clients: &ClientMap, state: &mut ShardServerState) -> FFResult<()> {
    let pkt: &sP_CL2FE_REQ_PC_TASK_END = pkt.get()?;
    let mut error_code = codes::TaskEndErr::NotComplete; // N.B. true failures are handled in player tick
    (|| {
        let pc_id = clients.get_sender().get_player_id()?;
        let player = state.get_player(pc_id)?;
        let running_tasks = player.mission_journal.get_current_tasks();
        let task = running_tasks
            .iter()
            .find(|t| t.get_task_id() == pkt.iTaskNum)
            .ok_or(FFError::build(
                Severity::Warning,
                format!("Tried to end task {} that is not active", pkt.iTaskNum),
            ))?;

        if task.failed {
            // ignore, task will get cleaned up by next start request
            return Ok(());
        }

        let task_def = task.get_task_def();

        // check target npc type + proximity
        if let Some(target_npc_type) = task_def.obj_npc_type {
            let target_npc_id = pkt.iNPC_ID;
            let target_npc = state.get_npc(target_npc_id)?;
            if target_npc.ty != target_npc_type {
                return Err(FFError::build(
                    Severity::Warning,
                    format!(
                        "Tried to end task {} with objective NPC type {}, should be {}",
                        pkt.iTaskNum, target_npc.ty, target_npc_type
                    ),
                ));
            }
            state.entity_map.validate_proximity(
                &[EntityID::Player(pc_id), EntityID::NPC(target_npc_id)],
                match task_def.task_type {
                    TaskType::Talk => RANGE_INTERACT,
                    TaskType::GotoLocation => RANGE_TRIGGER,
                    TaskType::EscortDefence => RANGE_TRIGGER,
                    _ => RANGE_INTERACT,
                },
            )?;
        }

        // check qitems
        for (qitem_id, qitem_count) in &task_def.obj_qitems {
            let has_count = player.get_quest_item_count(*qitem_id);
            if has_count < *qitem_count {
                return Err(FFError::build(
                    Severity::Warning,
                    format!(
                        "Tried to end task {} with qitem {} count {} (need {})",
                        pkt.iTaskNum, qitem_id, has_count, qitem_count,
                    ),
                ));
            }
        }

        // check enemies
        let remaining_counts = &task.remaining_enemy_defeats;
        for enemy_id in task_def.obj_enemies.keys() {
            let remaining_count = *remaining_counts.get(enemy_id).unwrap();
            if remaining_count > 0 {
                return Err(FFError::build(
                    Severity::Warning,
                    format!(
                        "Tried to end task {} with enemy {} x {} remaining",
                        pkt.iTaskNum, enemy_id, remaining_count
                    ),
                ));
            }
        }

        // check escort NPC is alive (and at destination, if pathed)
        if let Some(escort_npc_id) = task.escort_npc_id {
            let escort_npc = state.get_npc(escort_npc_id)?;
            if task_def.obj_npc_type.is_some()
                && escort_npc.path.as_ref().is_some_and(|path| !path.is_done())
            {
                return Err(FFError::build(
                    Severity::Warning,
                    format!(
                        "Tried to end task {} with pathed escort NPC {} not at destination",
                        pkt.iTaskNum, escort_npc_id
                    ),
                ));
            }
            if escort_npc.is_dead() {
                return Err(FFError::build(
                    Severity::Warning,
                    format!(
                        "Tried to end task {} with escort NPC {} dead",
                        pkt.iTaskNum, escort_npc_id
                    ),
                ));
            }
        }

        // check time limit
        if let Some(time_limit) = task.fail_time {
            if time_limit < std::time::SystemTime::now() {
                return Err(FFError::build(
                    Severity::Warning,
                    format!("Tried to end task {} after time limit", pkt.iTaskNum),
                ));
            }
        }

        // check for inventory space for rewards
        if let Some(reward_id) = task_def.succ_reward {
            let reward = tdata_get().get_mission_reward(reward_id)?;
            let inv_space = player.get_free_slots(ItemLocation::Inven);
            if reward.items.len() > inv_space {
                error_code = codes::TaskEndErr::InventoryFull;
                return Err(FFError::build(
                    Severity::Warning,
                    format!(
                        "Tried to end task {} with {} items but only {} inventory space",
                        pkt.iTaskNum,
                        reward.items.len(),
                        inv_space
                    ),
                ));
            }
        }

        // check for free qitem slots for qitem rewards
        if player.get_free_slots(ItemLocation::QInven) < task_def.succ_qitems.len() {
            error_code = codes::TaskEndErr::InventoryFull;
            return Err(FFError::build(
                Severity::Warning,
                format!(
                    "Tried to end task {} with {} qitems but only {} qitem slots",
                    pkt.iTaskNum,
                    task_def.succ_qitems.len(),
                    player.get_free_slots(ItemLocation::QInven)
                ),
            ));
        }

        // all clear, mark the task completed. it'll be overwritten by the next task
        let player = state.get_player_mut(pc_id).unwrap();
        player.mission_journal.complete_task(pkt.iTaskNum)?;

        // if escort following, stop it
        if let Some(escort_npc_id) = task.escort_npc_id {
            let escort_npc = state.get_npc_mut(escort_npc_id).unwrap();
            escort_npc.loose_follow = None;
        }
        let player = state.get_player_mut(pc_id).unwrap();

        // success qitem changes
        if !task_def.succ_qitems.is_empty() {
            let mut qitem_pkt =
                PacketBuilder::new(P_FE2CL_REP_REWARD_ITEM).with(&sP_FE2CL_REP_REWARD_ITEM {
                    m_iCandy: player.get_taros() as i32,
                    m_iFusionMatter: player.get_fusion_matter() as i32,
                    m_iBatteryN: player.get_nano_potions() as i32,
                    m_iBatteryW: player.get_weapon_boosts() as i32,
                    iItemCnt: task_def.succ_qitems.len() as i8,
                    iFatigue: 100,
                    iFatigue_Level: 1,
                    iNPC_TypeID: 0,
                    iTaskID: task_def.task_id,
                });

            for (qitem_id, qitem_count_mod) in &task_def.succ_qitems {
                let curr_count = player.get_quest_item_count(*qitem_id) as isize;
                let new_count = (curr_count + *qitem_count_mod) as usize;
                let qitem_slot = player.set_quest_item_count(*qitem_id, new_count).unwrap();
                helpers::push_item_reward(
                    &mut qitem_pkt,
                    RewardItem::Quest {
                        id: *qitem_id,
                        count: new_count,
                        slot_num: qitem_slot,
                    },
                );
            }

            if let Some(qitem_pkt) = log_if_failed(qitem_pkt.build()) {
                clients.get_sender().send_payload(qitem_pkt);
            }
        }

        if let Some(reward_id) = task_def.succ_reward {
            match tdata_get()
                .get_mission_reward(reward_id)
                .map(|r| r.with_player_rates(player))
            {
                Err(e) => log_error(e),
                Ok(reward) => {
                    let taros_new = player.get_taros().saturating_add(reward.taros);
                    let fm_new = player
                        .get_fusion_matter()
                        .saturating_add(reward.fusion_matter);
                    let mut reward_pkt = PacketBuilder::new(P_FE2CL_REP_REWARD_ITEM).with(
                        &sP_FE2CL_REP_REWARD_ITEM {
                            m_iCandy: player.set_taros(taros_new) as i32,
                            m_iFusionMatter: player.set_fusion_matter(fm_new) as i32,
                            m_iBatteryN: player.get_nano_potions() as i32,
                            m_iBatteryW: player.get_weapon_boosts() as i32,
                            iItemCnt: reward.items.len() as i8,
                            iFatigue: 100,
                            iFatigue_Level: 1,
                            iNPC_TypeID: unused!(),
                            iTaskID: task_def.task_id,
                        },
                    );

                    for item in &reward.items {
                        let slot_num = player.find_free_slot(ItemLocation::Inven).unwrap();
                        let item_reward = *item;
                        player
                            .set_item(ItemLocation::Inven, slot_num, Some(item_reward))
                            .unwrap();
                        helpers::push_item_reward(
                            &mut reward_pkt,
                            RewardItem::Normal {
                                item: Some(item_reward),
                                slot_num,
                            },
                        );
                    }

                    if let Some(reward_pkt) = log_if_failed(reward_pkt.build()) {
                        clients.get_sender().send_payload(reward_pkt);
                    }
                }
            }
        }

        if task_def.succ_task_id.is_none() {
            // Final task, mission complete
            let mission_def = tdata_get()
                .get_mission_definition(task_def.mission_id)
                .unwrap();
            player
                .mission_journal
                .remove_task(task_def.task_id)
                .unwrap();
            player
                .mission_journal
                .set_mission_completed(task_def.mission_id)
                .unwrap();

            log(
                Severity::Info,
                &format!(
                    "{} completed mission: {} [{}]",
                    player, mission_def.mission_name, task_def.mission_id
                ),
            );

            if let Some(nano_id) = task_def.succ_nano_id {
                let old_level = player.get_level();
                match player.unlock_nano(nano_id).cloned() {
                    Ok(nano) => {
                        let level = match config_get().general.protocol.get() {
                            FFProtocol::v0104 => {
                                let player_stats = tdata_get().get_player_stats(old_level).unwrap();

                                // Player is guaranteed to have enough fusion matter to create the nano,
                                // but if they used commands they may have less. Clamp to 0.
                                let new_fm = player
                                    .get_fusion_matter()
                                    .saturating_sub(player_stats.req_fm_nano_create);
                                player.set_fusion_matter(new_fm);

                                let new_level = std::cmp::max(old_level, nano_id);
                                match player.set_level(new_level) {
                                    Ok(l) => l,
                                    Err(e) => {
                                        log_error(e);
                                        player.get_level()
                                    }
                                }
                            }
                            FFProtocol::v1013 => old_level,
                        };

                        helpers::send_nano_create_succ(
                            clients.get_sender(),
                            player.get_fusion_matter(),
                            None,
                            None,
                            Some(&nano),
                            level,
                        );

                        if level != old_level {
                            let bcast = sP_FE2CL_REP_PC_CHANGE_LEVEL {
                                iPC_ID: pc_id,
                                iPC_Level: level,
                            };

                            state
                                .entity_map
                                .for_each_around(EntityID::Player(pc_id), |c| {
                                    c.send_packet(P_FE2CL_REP_PC_CHANGE_LEVEL, &bcast)
                                });
                        }
                    }
                    Err(e) => log_error(e),
                }
            }
        }

        let resp = sP_FE2CL_REP_PC_TASK_END_SUCC {
            iTaskNum: pkt.iTaskNum,
        };

        clients
            .get_sender()
            .send_packet(P_FE2CL_REP_PC_TASK_END_SUCC, &resp);

        Ok(())
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_REP_PC_TASK_END_FAIL {
            iTaskNum: pkt.iTaskNum,
            iErrorCode: error_code as i32,
        };

        clients
            .get_sender()
            .send_packet(P_FE2CL_REP_PC_TASK_END_FAIL, &resp);
    })
}

pub fn set_current_mission_id(
    pkt: Packet,
    client: &FFClient,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let pkt: &sP_CL2FE_REQ_PC_SET_CURRENT_MISSION_ID = pkt.get()?;

    (|| {
        let pc_id = client.get_player_id()?;
        let player = state.get_player_mut(pc_id)?;
        let active_mission_slot = player
            .mission_journal
            .set_active_mission_id(pkt.iCurrentMissionID)?;

        log(
            Severity::Debug,
            &format!(
                "Player {} set active mission slot to {}, mission ID {}",
                player.get_uid(),
                active_mission_slot,
                pkt.iCurrentMissionID
            ),
        );

        let resp = sP_FE2CL_REP_PC_SET_CURRENT_MISSION_ID {
            iCurrentMissionID: pkt.iCurrentMissionID,
        };

        client.send_packet(P_FE2CL_REP_PC_SET_CURRENT_MISSION_ID, &resp);
        Ok(())
    })()
    .catch_fail(|| {
        let resp = sP_FE2CL_REP_PC_SET_CURRENT_MISSION_ID {
            iCurrentMissionID: 0,
        };

        client.send_packet(P_FE2CL_REP_PC_SET_CURRENT_MISSION_ID, &resp);
    })
}

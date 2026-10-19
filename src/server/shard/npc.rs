use rand::seq::IteratorRandom;

use crate::{
    entity::EntityID,
    error::*,
    net::{
        packet::{PacketID::*, *},
        FFClient,
    },
    state::ShardServerState,
    tabledata::tdata_get,
};

pub fn npc_interaction(
    pkt: Packet,
    client: &FFClient,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let pkt: &sP_CL2FE_REQ_NPC_INTERACTION = pkt.get()?;
    let pc_id = client.get_player_id()?;
    let npc_id = pkt.iNPC_ID;

    let npc = state.get_npc_mut(npc_id)?;
    if pkt.bFlag == 0 {
        if !npc.interacting_pcs.remove(&pc_id) {
            log(
                Severity::Warning,
                &format!(
                    "Player {} tried to stop interacting with NPC {} when not interacting",
                    pc_id, npc_id
                ),
            );
        }
    } else if !npc.interacting_pcs.insert(pc_id) {
        log(
            Severity::Warning,
            &format!(
                "Player {} tried to start interacting with NPC {} when already interacting",
                pc_id, npc_id
            ),
        );
    }

    Ok(())
}

pub fn npc_bark(pkt: Packet, client: &FFClient, state: &mut ShardServerState) -> FFResult<()> {
    let pkt: &sP_CL2FE_REQ_BARKER = pkt.get()?;
    let task_id = pkt.iMissionTaskID;
    let pc_id = client.get_player_id()?;

    let task_def = tdata_get().get_task_definition(task_id)?;
    let barks = &task_def.barks;
    if barks.is_empty() {
        return Err(FFError::build(
            Severity::Warning,
            format!(
                "Player requested barker for task with no barks ({})",
                task_id
            ),
        ));
    }

    // we ignore pkt.iNPC_ID because the client always picks the first NPC in its container.
    // instead, we pick at random from the nearby NPCS that have a compatible barker type
    // for this mission. thanks @FinnHornhoover for finding this workaround (OpenFusion #266)
    let nearby_npc_ids = state.entity_map.get_around_entity(EntityID::Player(pc_id));
    let mut compatible_barks = Vec::with_capacity(nearby_npc_ids.len());
    for npc_id in nearby_npc_ids {
        if let EntityID::NPC(npc_id) = npc_id {
            let npc = state.get_npc(npc_id).unwrap();
            let npc_stats = tdata_get().get_npc_stats(npc.ty).unwrap();
            if npc_stats.team == crate::enums::CombatantTeam::Mob {
                continue;
            }
            if let Some(line) = compatible_bark(barks, npc_stats.bark_type) {
                compatible_barks.push((npc_id, line));
            }
        }
    }

    let chosen_bark = compatible_barks.iter().choose(&mut rand::thread_rng());
    if let Some(&(npc_id, bark_id)) = chosen_bark {
        let pkt = sP_FE2CL_REP_BARKER {
            iNPC_ID: npc_id,
            iMissionStringID: bark_id,
        };
        client.send_packet(P_FE2CL_REP_BARKER, &pkt);
    }

    Ok(())
}

fn compatible_bark(barks: &[i32], bark_type: Option<usize>) -> Option<i32> {
    let index = bark_type?.checked_sub(1)?;
    if index >= 4 {
        return None;
    }
    barks.get(index).copied().filter(|id| *id != 0)
}

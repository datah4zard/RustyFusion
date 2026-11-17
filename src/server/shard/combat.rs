use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use crate::{
    defines::EQUIP_SLOT_HAND,
    entity::{Combatant, Entity, EntityID, Projectile, ProjectileKind, NPC},
    enums::{CharType, ItemLocation, SkillTargetType, TargetType},
    error::*,
    net::{
        packet::{PacketID::*, *},
        ClientMap,
    },
    skills::{self, SkillResult},
    state::ShardServerState,
    tabledata::{tdata_get, NPCStats},
    Position,
};

#[allow(non_camel_case_types)]
#[allow(non_snake_case)]
#[repr(packed(4))]
#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct sTargetNpcId {
    pub iNPC_ID: i32,
}
impl FFPacket for sTargetNpcId {}

#[allow(non_camel_case_types)]
#[allow(non_snake_case)]
#[repr(packed(4))]
#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct sTargetPcId {
    pub iPC_ID: i32,
}
impl FFPacket for sTargetPcId {}

#[allow(non_camel_case_types)]
#[allow(non_snake_case)]
#[repr(packed(4))]
#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct sProjectileTarget {
    pub target_id: i32,
    pub character_type: i32,
}
impl FFPacket for sProjectileTarget {}

const MAX_TARGETS: usize = 3;
const BATTERY_BASE_COST: u32 = 6;

// TODO factor in the player's ping
const FIRE_LAG_ALLOWANCE: Duration = Duration::from_millis(300);
const HIT_EXPIRY_ALLOWANCE: Duration = Duration::from_millis(1000);

pub fn pc_attack_npcs(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = client.get_player_id()?;

    let mut reader = PacketReader::new(&pkt);
    let pkt: &sP_CL2FE_REQ_PC_ATTACK_NPCs = reader.get_struct()?;
    let target_count = pkt.iNPCCnt as usize;
    if target_count == 0 {
        return Ok(());
    }

    let mut target_ids = Vec::with_capacity(MAX_TARGETS);
    let mut weapon_boosts_needed = 0;
    for i in 0..target_count {
        // TODO stricter anti-cheat.
        // validate target count, range, attack cooldown, etc against weapon stats
        if i >= MAX_TARGETS {
            return Err(FFError::build(
                Severity::Warning,
                format!(
                    "Player {} tried to attack {} NPCs (max {})",
                    pc_id, pkt.iNPCCnt, MAX_TARGETS
                ),
            ));
        }
        let npc_id = reader.get_struct::<sTargetNpcId>()?.iNPC_ID;
        let npc = match state.get_npc(npc_id) {
            Ok(npc) => npc,
            Err(e) => {
                log_error(e);
                continue;
            }
        };
        weapon_boosts_needed += BATTERY_BASE_COST + npc.get_level() as u32;
        target_ids.push(npc.get_id());
    }

    // consume weapon boosts
    let player = state.get_player_mut(pc_id)?;
    let weapon_boosts = player.get_weapon_boosts();
    let charged = if weapon_boosts >= weapon_boosts_needed {
        player.set_weapon_boosts(weapon_boosts - weapon_boosts_needed);
        true
    } else {
        player.set_weapon_boosts(0);
        false
    };

    // attack handler
    skills::do_basic_attack(player.get_id(), &target_ids, charged, state)?;

    Ok(())
}

pub fn pc_attack_pcs(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = client.get_player_id()?;

    let mut reader = PacketReader::new(&pkt);
    let pkt: &sP_CL2FE_REQ_PC_ATTACK_CHARs = reader.get_struct()?;
    let target_count = pkt.iTargetCnt as usize;
    if target_count == 0 {
        return Ok(());
    }

    let player = state.get_player(pc_id)?;
    let mut target_ids = Vec::with_capacity(MAX_TARGETS);
    let mut weapon_boosts_needed = 0;
    for i in 0..target_count {
        // TODO see above
        if i >= MAX_TARGETS {
            log(
                Severity::Warning,
                &format!(
                    "{} tried to attack {} PCs (max {})",
                    player, pkt.iTargetCnt, MAX_TARGETS
                ),
            );
            break;
        }

        let target_pc_id = reader.get_struct::<sTargetPcId>()?.iPC_ID;
        if target_pc_id == pc_id {
            log(
                Severity::Warning,
                &format!("{} tried to attack themselves", player),
            );
            continue;
        }

        let target_player = match state.get_player(target_pc_id) {
            Ok(player) => player,
            Err(e) => {
                log_error(e);
                continue;
            }
        };

        weapon_boosts_needed += BATTERY_BASE_COST + target_player.get_level() as u32;
        target_ids.push(target_player.get_id());
    }

    // consume weapon boosts
    let player = state.get_player_mut(pc_id)?;
    let weapon_boosts = player.get_weapon_boosts();
    let charged = if weapon_boosts >= weapon_boosts_needed {
        player.set_weapon_boosts(weapon_boosts - weapon_boosts_needed);
        true
    } else {
        player.set_weapon_boosts(0);
        false
    };

    // attack handler
    skills::do_basic_attack(player.get_id(), &target_ids, charged, state)?;

    Ok(())
}

pub fn pc_rocket_style_fire(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = client.get_player_id()?;
    let pkt: &sP_CL2FE_REQ_PC_ROCKET_STYLE_FIRE = pkt.get()?;

    let start_pos = state.get_player(pc_id)?.get_position();
    let end_pos = Position {
        x: pkt.iToX,
        y: pkt.iToY,
        z: pkt.iToZ,
    };

    let Some(projectile_id) =
        try_spawn_projectile(ProjectileKind::Rocket, start_pos, end_pos, pc_id, state)?
    else {
        return Ok(());
    };

    let player = state.get_player(pc_id)?;
    let spc_bullet = sPCBullet::from(player.get_projectile(projectile_id)?);
    let nano = player.get_active_nano();

    let resp = sP_FE2CL_REP_PC_ROCKET_STYLE_FIRE_SUCC {
        iSkillID: pkt.iSkillID,
        iX: start_pos.x,
        iY: start_pos.y,
        iZ: start_pos.z,
        iToX: end_pos.x,
        iToY: end_pos.y,
        iToZ: end_pos.z,
        iBulletID: projectile_id,
        Bullet: spc_bullet,
        iBatteryW: player.get_weapon_boosts() as i32,
        bNanoDeactive: 0,
        iNanoID: nano.map(|n| n.get_id()).unwrap_or(0),
        iNanoStamina: nano.map(|n| n.get_stamina()).unwrap_or(0),
    };
    client.send_packet(P_FE2CL_REP_PC_ROCKET_STYLE_FIRE_SUCC, &resp);

    let bcast = sP_FE2CL_PC_ROCKET_STYLE_FIRE {
        iPC_ID: pc_id,
        iX: start_pos.x,
        iY: start_pos.y,
        iZ: start_pos.z,
        iToX: end_pos.x,
        iToY: end_pos.y,
        iToZ: end_pos.z,
        iBulletID: projectile_id,
        Bullet: spc_bullet,
        bNanoDeactive: 0,
    };
    state
        .entity_map
        .for_each_around(EntityID::Player(pc_id), |c| {
            c.send_packet(P_FE2CL_PC_ROCKET_STYLE_FIRE, &bcast);
        });

    Ok(())
}

pub fn pc_grenade_style_fire(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = client.get_player_id()?;
    let pkt: &sP_CL2FE_REQ_PC_GRENADE_STYLE_FIRE = pkt.get()?;

    let start_pos = state.get_player(pc_id)?.get_position();
    let end_pos = Position {
        x: pkt.iToX,
        y: pkt.iToY,
        z: pkt.iToZ,
    };

    let Some(projectile_id) =
        try_spawn_projectile(ProjectileKind::Grenade, start_pos, end_pos, pc_id, state)?
    else {
        return Ok(());
    };

    let player = state.get_player(pc_id)?;
    let spc_bullet = sPCBullet::from(player.get_projectile(projectile_id)?);
    let nano = player.get_active_nano();

    let resp = sP_FE2CL_REP_PC_GRENADE_STYLE_FIRE_SUCC {
        iSkillID: pkt.iSkillID,
        iToX: end_pos.x,
        iToY: end_pos.y,
        iToZ: end_pos.z,
        iBulletID: projectile_id,
        Bullet: spc_bullet,
        iBatteryW: player.get_weapon_boosts() as i32,
        bNanoDeactive: 0,
        iNanoID: nano.map(|n| n.get_id()).unwrap_or(0),
        iNanoStamina: nano.map(|n| n.get_stamina()).unwrap_or(0),
    };
    client.send_packet(P_FE2CL_REP_PC_GRENADE_STYLE_FIRE_SUCC, &resp);

    let bcast = sP_FE2CL_PC_GRENADE_STYLE_FIRE {
        iPC_ID: pc_id,
        iToX: end_pos.x,
        iToY: end_pos.y,
        iToZ: end_pos.z,
        iBulletID: projectile_id,
        Bullet: spc_bullet,
        bNanoDeactive: 0,
    };
    state
        .entity_map
        .for_each_around(EntityID::Player(pc_id), |c| {
            c.send_packet(P_FE2CL_PC_GRENADE_STYLE_FIRE, &bcast);
        });

    Ok(())
}

fn try_spawn_projectile(
    projectile_kind: ProjectileKind,
    start_pos: Position,
    end_pos: Position,
    pc_id: i32,
    state: &mut ShardServerState,
) -> FFResult<Option<i8>> {
    let Some(weapon) = *state
        .get_player(pc_id)?
        .get_item(ItemLocation::Equip, EQUIP_SLOT_HAND as usize)?
    else {
        return Err(FFError::build(
            Severity::Warning,
            format!(
                "Player {} tried to fire a {} with no item equipped",
                pc_id,
                projectile_kind.name()
            ),
        ));
    };

    let reject = |reason: &str| {
        FFError::build(
            Severity::Warning,
            format!(
                "Player {} failed to fire {} weapon {}: {}",
                pc_id,
                projectile_kind.name(),
                weapon.id,
                reason
            ),
        )
    };

    let weapon_stats = weapon.get_stats()?;
    if weapon_stats.equip_type != Some(projectile_kind.equip_type()) {
        return Err(reject("wrong weapon type"));
    }

    // table times are in tenths of a second
    let tenths = |value: Option<i32>| Duration::from_millis(value.unwrap() as u64 * 100);

    let max_targets = weapon_stats.target_number.unwrap() as usize;
    let fire_delay = tenths(weapon_stats.delay_time);
    let flight_time = tenths(weapon_stats.duration_time);
    let effect_area = weapon_stats.effect_area.unwrap() as u32;

    let player = state.get_player_mut(pc_id)?;

    // rate limiting for weapon cooldown
    let now = Instant::now();
    if let Some(next_allowed) = player.get_next_shot_allowed_for_weapon(weapon.id) {
        if now + FIRE_LAG_ALLOWANCE < next_allowed {
            return Err(reject(&format!(
                "too fast ({} ms before it was due, allowance {} ms, cooldown {} ms)",
                (next_allowed - now).as_millis(),
                FIRE_LAG_ALLOWANCE.as_millis(),
                fire_delay.as_millis()
            )));
        }
    }

    let weapon_boosts = player.get_weapon_boosts();
    let weapon_boosts_needed = BATTERY_BASE_COST + weapon_stats.required_level as u32;
    let weapon_charged = weapon_boosts >= weapon_boosts_needed;

    let projectile = Projectile {
        kind: projectile_kind,
        weapon_id: weapon.id,
        single_power: player.get_single_power(),
        multi_power: player.get_multi_power(),
        charged: weapon_charged,
        attack_style: Combatant::get_style(player),
        instance_id: player.instance_id,
        effect_area,
        max_targets,
        // TODO validate end_pos against start_pos (range validation)
        start_pos,
        end_pos,
        end_time: now + flight_time + HIT_EXPIRY_ALLOWANCE,
    };

    // silently drop if too many active bullets
    let Some(projectile_id) = player.add_projectile(projectile) else {
        log(
            Severity::Debug,
            &format!(
                "Player {} tried to fire a {} but has too many projectiles",
                pc_id,
                projectile_kind.name()
            ),
        );
        return Ok(None);
    };

    // only a stored shot costs boosts and books the cooldown
    player.set_weapon_boosts(if weapon_charged {
        weapon_boosts - weapon_boosts_needed
    } else {
        0
    });

    let due = player
        .get_next_shot_allowed_for_weapon(weapon.id)
        .map_or(now, |next_allowed| next_allowed.max(now));
    player.set_next_shot_allowed_for_weapon(weapon.id, due + fire_delay);

    Ok(Some(projectile_id))
}

pub fn pc_projectile_hit(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let pc_id = clients.get_sender().get_player_id()?;

    let mut reader = PacketReader::new(&pkt);
    let header: &sP_CL2FE_REQ_PC_ROCKET_STYLE_HIT = reader.get_struct()?;
    let projectile_id = header.iBulletID;
    let hit_pos = Position {
        x: header.iX,
        y: header.iY,
        z: header.iZ,
    };
    let targets = (0..header.iTargetCnt)
        .map(|_| reader.get_struct::<sProjectileTarget>().copied())
        .collect::<FFResult<Vec<_>>>()?;

    let Some(projectile) = state
        .get_player_mut(pc_id)?
        .remove_projectile(projectile_id)
    else {
        return Err(FFError::build(
            Severity::Info,
            format!(
                "Player {} reported a hit for unknown or expired projectile {}",
                pc_id, projectile_id
            ),
        ));
    };
    let spc_bullet = sPCBullet::from(&projectile);

    let skip = |severity: Severity, reason: String| {
        log(
            severity,
            &format!("Player {} {} hit {}", pc_id, projectile.kind.name(), reason),
        );
    };

    let mut hits = Vec::new();
    let mut seen_ids = HashSet::new();
    for target in targets {
        let target_id = target.target_id;
        match CharType::try_from(target.character_type) {
            Ok(CharType::Mob) => {}
            Ok(CharType::Player) => {
                skip(
                    Severity::Info,
                    format!("listed player {target_id}; PvP is TODO"),
                );
                continue;
            }
            _ => {
                skip(
                    Severity::Warning,
                    format!(
                        "listed target {} with invalid type {}",
                        target_id, target.character_type
                    ),
                );
                continue;
            }
        }
        if !seen_ids.insert(target_id) {
            skip(
                Severity::Warning,
                format!("listed NPC {target_id} more than once"),
            );
            continue;
        }
        let Some(npc) = log_if_failed(state.get_npc(target_id)) else {
            continue;
        };
        if npc.instance_id != projectile.instance_id {
            skip(
                Severity::Warning,
                format!(
                    "claimed {} outside the instance it was fired in ({} != {})",
                    npc, npc.instance_id, projectile.instance_id
                ),
            );
            continue;
        }
        if npc.is_dead() {
            skip(Severity::Debug, format!("claimed dead {npc}"));
            continue;
        }
        let Some(stats) = log_if_failed(tdata_get().get_npc_stats(npc.ty)) else {
            continue;
        };
        if let Some(distance) = hit_to_mob_distance(npc, stats, hit_pos, projectile.effect_area) {
            hits.push((npc.get_id(), distance));
        }
    }

    // keep the closest n targets if there are more than the limit
    let target_ids = closest_targets(hits, projectile.max_targets);

    let results =
        skills::do_projectile_attack(EntityID::Player(pc_id), &target_ids, &projectile, state)?;

    let target_cnt = results.len() as i32;
    let mut bcast = match projectile.kind {
        ProjectileKind::Rocket => {
            PacketBuilder::new(P_FE2CL_PC_ROCKET_STYLE_HIT).with(&sP_FE2CL_PC_ROCKET_STYLE_HIT {
                iPC_ID: pc_id,
                iBulletID: projectile_id,
                Bullet: spc_bullet,
                iTargetCnt: target_cnt,
            })
        }
        ProjectileKind::Grenade => {
            PacketBuilder::new(P_FE2CL_PC_GRENADE_STYLE_HIT).with(&sP_FE2CL_PC_GRENADE_STYLE_HIT {
                iPC_ID: pc_id,
                iBulletID: projectile_id,
                Bullet: spc_bullet,
                iTargetCnt: target_cnt,
            })
        }
    };
    for result in &results {
        bcast.push(result);
    }

    if let Some(bcast) = log_if_failed(bcast.build()) {
        state
            .entity_map
            .for_each_around(EntityID::Player(pc_id), |c| {
                c.send_payload(bcast.clone());
            });
    }

    Ok(())
}

pub fn nano_skill_use(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()> {
    let client = clients.get_sender();
    let pc_id = client.get_player_id()?;
    let player = state.get_player(pc_id)?;
    let Some(nano) = player.get_active_nano() else {
        return Err(FFError::build(
            Severity::Warning,
            format!(
                "{} tried to use a nano skill without an active nano",
                player
            ),
        ));
    };

    let skill_level = placeholder!(1); // TODO calculate from gumballs

    let Some(skill) = nano.get_skill() else {
        return Err(FFError::build(
            Severity::Warning,
            format!("{} tried to use a skill from a nano with no skill", player),
        ));
    };

    let skill_cost = skill.costs[skill_level as usize - 1] as i16;
    let nano_stamina = nano.get_stamina();
    if nano_stamina < skill_cost {
        return Err(FFError::build(
            Severity::Warning,
            format!(
                "{} tried to use a nano skill without enough stamina ({} needed, {} available)",
                player, skill_cost, nano_stamina
            ),
        ));
    }

    let mut reader = PacketReader::new(&pkt);
    let pkt: &sP_CL2FE_REQ_NANO_SKILL_USE = reader.get_struct()?;
    let target_count = pkt.iTargetCnt as usize;
    if target_count == 0 {
        return Ok(());
    }

    let mut target_ids = Vec::with_capacity(MAX_TARGETS);
    for i in 0..target_count {
        if i >= MAX_TARGETS {
            log(
                Severity::Warning,
                &format!(
                    "{} tried to use a nano skill on {} targets (max {})",
                    player, pkt.iTargetCnt, MAX_TARGETS
                ),
            );
            break;
        }

        let target_id = match skill.target_type {
            TargetType::HostileNPCs => {
                let target_npc_id = reader.get_struct::<sTargetNpcId>()?.iNPC_ID;
                EntityID::NPC(target_npc_id)
            }
            TargetType::FriendlyPCs => {
                let target_pc_id = reader.get_struct::<sTargetPcId>()?.iPC_ID;
                EntityID::Player(target_pc_id)
            }
            TargetType::CasterPC => player.get_id(),
        };

        // validate against targeting type
        let valid = match skill.targeting_type {
            SkillTargetType::None => {
                return Err(FFError::build(
                    Severity::Warning,
                    format!(
                        "{} tried to use a nano skill with no targeting type",
                        player
                    ),
                ));
            }
            _ => placeholder!(true), // TODO validate for each targeting type
        };

        if !valid {
            log(
                Severity::Warning,
                &format!(
                    "{} tried to use a nano skill on an invalid target {:?} for targeting type {:?}",
                    player, target_id, skill.targeting_type
                ),
            );
            continue;
        }

        target_ids.push(target_id);
    }

    let results = skills::do_skill(player.get_id(), &target_ids, skill, skill_level, state)?;

    let nano = state
        .get_player_mut(pc_id)
        .unwrap()
        .get_active_nano_mut()
        .unwrap();

    nano.set_stamina(nano_stamina - skill_cost);

    let target_cnt = results.len() as i32;
    let skill_id = nano.selected_skill.unwrap();
    let nano_stamina = nano.get_stamina();
    let nano_deactive = nano_stamina == 0;
    let nano_id = nano.get_id();
    let skill_type = skill.skill_type as i32;

    let mut succ_builder =
        PacketBuilder::new(P_FE2CL_NANO_SKILL_USE_SUCC).with(&sP_FE2CL_NANO_SKILL_USE_SUCC {
            iPC_ID: pc_id,
            iBulletID: pkt.iBulletID,
            iSkillID: skill_id,
            iArg1: pkt.iArg1,
            iArg2: pkt.iArg2,
            iArg3: pkt.iArg3,
            bNanoDeactive: nano_deactive as i32,
            iNanoID: nano_id,
            iNanoStamina: nano_stamina,
            eST: skill_type,
            iTargetCnt: target_cnt,
        });

    let mut bcast_builder =
        PacketBuilder::new(P_FE2CL_NANO_SKILL_USE).with(&sP_FE2CL_NANO_SKILL_USE {
            iPC_ID: pc_id,
            iBulletID: pkt.iBulletID,
            iSkillID: skill_id,
            iArg1: pkt.iArg1,
            iArg2: pkt.iArg2,
            iArg3: pkt.iArg3,
            bNanoDeactive: nano_deactive as i32,
            iNanoID: nano_id,
            iNanoStamina: nano_stamina,
            eST: skill_type,
            iTargetCnt: target_cnt,
        });

    for result in results {
        // These look identical, but since they have different concrete types, each arm
        // is a different push call with a different type parameter.
        match result {
            SkillResult::Damage(sr) => {
                succ_builder.push(&sr);
                bcast_builder.push(&sr);
            }
            SkillResult::DotDamage(sr) => {
                succ_builder.push(&sr);
                bcast_builder.push(&sr);
            }
            SkillResult::HealHP(sr) => {
                succ_builder.push(&sr);
                bcast_builder.push(&sr);
            }
            SkillResult::HealStamina(sr) => {
                succ_builder.push(&sr);
                bcast_builder.push(&sr);
            }
            SkillResult::StaminaSelf(sr) => {
                succ_builder.push(&sr);
                bcast_builder.push(&sr);
            }
            SkillResult::DamageAndDebuff(sr) => {
                succ_builder.push(&sr);
                bcast_builder.push(&sr);
            }
            SkillResult::Buff(sr) => {
                succ_builder.push(&sr);
                bcast_builder.push(&sr);
            }
            SkillResult::BatteryDrain(sr) => {
                succ_builder.push(&sr);
                bcast_builder.push(&sr);
            }
            SkillResult::DamageAndMove(sr) => {
                succ_builder.push(&sr);
                bcast_builder.push(&sr);
            }
            SkillResult::Move(sr) => {
                succ_builder.push(&sr);
                bcast_builder.push(&sr);
            }
            SkillResult::Resurrect(sr) => {
                succ_builder.push(&sr);
                bcast_builder.push(&sr);
            }
        }
    }

    if let Some(pkt) = log_if_failed(succ_builder.build()) {
        client.send_payload(pkt);
    }

    if let Some(pkt) = log_if_failed(bcast_builder.build()) {
        state
            .entity_map
            .for_each_around(EntityID::Player(pc_id), |c| {
                c.send_payload(pkt.clone());
            });
    }

    Ok(())
}

// anti-cheat for hit range
fn hit_to_mob_distance(
    npc: &NPC,
    stats: &NPCStats,
    hit_pos: Position,
    effect_area: u32,
) -> Option<u32> {
    let half_height = stats.height / 2;
    let npc_pos = npc.get_position();
    // mob server positions are from the feet
    let body_center = Position {
        z: npc_pos.z + half_height as i32,
        ..npc_pos
    };

    let limit = stats.radius.max(half_height) + effect_area;
    let distance = hit_pos.distance_to(&body_center);
    (distance < limit).then_some(distance)
}

fn closest_targets(mut hits: Vec<(EntityID, u32)>, max_targets: usize) -> Vec<EntityID> {
    hits.sort_by_key(|&(_, distance)| distance);
    hits.truncate(max_targets);
    hits.into_iter().map(|(id, _)| id).collect()
}

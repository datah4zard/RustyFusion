use std::{path::PathBuf, sync::LazyLock, time::SystemTime};

use async_trait::async_trait;
use deadpool_sqlite::{
    rusqlite::{
        self, params_from_iter,
        types::{FromSql, ToSql, Value, ValueRef},
        Connection,
    },
    Config as PoolConfig, Hook, HookError, Pool, Runtime,
};
use regex::Regex;

use crate::{
    database::*,
    defines::*,
    entity::{BuddyListEntry, Combatant, Entity, PlayerFlags, PlayerStyle},
    enums::PlayerGuide,
    item::Item,
    mission::Task,
    nano::Nano,
    net::packet::*,
    state::Cookie,
    tabledata::tdata_get,
    util::{self, Bitfield},
    Position,
};

impl From<rusqlite::Error> for FFError {
    fn from(e: rusqlite::Error) -> Self {
        FFError::build(db_error_severity(), "Database error".to_string())
            .with_parent(FFError::build(Severity::Debug, e.to_string()))
    }
}
impl From<deadpool_sqlite::PoolError> for FFError {
    fn from(e: deadpool_sqlite::PoolError) -> Self {
        FFError::build(db_error_severity(), "Database pool error".to_string())
            .with_parent(FFError::build(Severity::Debug, e.to_string()))
    }
}
impl From<deadpool_sqlite::InteractError> for FFError {
    fn from(e: deadpool_sqlite::InteractError) -> Self {
        FFError::build(db_error_severity(), "Database interact error".to_string())
            .with_parent(FFError::build(Severity::Debug, e.to_string()))
    }
}
impl From<deadpool_sqlite::CreatePoolError> for FFError {
    fn from(e: deadpool_sqlite::CreatePoolError) -> Self {
        FFError::build(
            db_error_severity(),
            "Database pool creation error".to_string(),
        )
        .with_parent(FFError::build(Severity::Debug, e.to_string()))
    }
}

pub struct SqliteDatabase {
    pool: Pool,
    path: PathBuf,
}
impl std::fmt::Debug for SqliteDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SQLite Database ({})", self.path.display())
    }
}

struct OwnedRow {
    columns: Vec<String>,
    values: Vec<Value>,
}
impl OwnedRow {
    fn index_of(&self, col: &str) -> rusqlite::Result<usize> {
        self.columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(col))
            .ok_or_else(|| rusqlite::Error::InvalidColumnName(col.to_string()))
    }

    fn get<T: FromSql>(&self, col: &str) -> T {
        self.try_get(col).unwrap_or_else(|e| {
            panic!("Column `{}` could not be read: {}", col, e);
        })
    }

    fn try_get<T: FromSql>(&self, col: &str) -> rusqlite::Result<T> {
        let idx = self.index_of(col)?;
        let val_ref = ValueRef::from(&self.values[idx]);
        T::column_result(val_ref).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(idx, val_ref.data_type(), Box::new(e))
        })
    }
}

impl SqliteDatabase {
    pub async fn connect(config: &GeneralConfig) -> FFResult<Self> {
        let path = PathBuf::from(config.db_path.get());

        let post_create = Hook::sync_fn(|wrapper, _| {
            let conn = wrapper
                .lock()
                .map_err(|_| HookError::message("sqlite connection mutex poisoned"))?;
            conn.execute_batch(
                "PRAGMA journal_mode=WAL;\
                 PRAGMA synchronous=NORMAL;\
                 PRAGMA foreign_keys=ON;\
                 PRAGMA busy_timeout=5000;",
            )
            .map_err(HookError::Backend)?;
            conn.set_prepared_statement_cache_capacity(64);
            Ok(())
        });

        let pool: Pool = PoolConfig::new(&path)
            .builder(Runtime::Tokio1)
            .unwrap()
            .post_create(post_create)
            .build()
            .map_err(|e| {
                FFError::build(
                    Severity::Warning,
                    format!("Failed to build SQLite pool: {}", e),
                )
            })?;

        let conn = pool.get().await?;
        let meta_exists: bool = conn
            .interact(|conn| -> rusqlite::Result<bool> {
                conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND lower(name) = 'meta')",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .map(|n| n != 0)
            })
            .await??;

        if !meta_exists {
            log(
                Severity::Info,
                "Meta table missing; initializing database...",
            );

            let protocol_version = config.protocol.get().version_number();

            conn.interact(move |conn| -> FFResult<()> {
                let tx = conn.transaction()?;
                Self::exec_in(&tx, "create_tables", &[&protocol_version, &DB_VERSION])?;
                tx.commit()?;
                Ok(())
            })
            .await??;
        }

        Ok(Self { pool, path })
    }

    fn read_sql(name: &str) -> FFResult<&'static str> {
        crate::database::get_sql_string(name)
    }

    fn count_params(s: &str) -> usize {
        static SQL_PARAMETER_REGEX: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\?[0-9]+").unwrap());
        SQL_PARAMETER_REGEX
            .find_iter(s)
            .map(|m| m.as_str()[1..].parse::<usize>().unwrap())
            .max()
            .unwrap_or(0)
    }

    fn query(conn: &Connection, name: &str, params: &[&dyn ToSql]) -> FFResult<Vec<OwnedRow>> {
        let sql = Self::read_sql(name)?;
        let mut stmt = conn.prepare_cached(sql)?;
        let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let col_count = stmt.column_count();
        let mut rows = stmt.query(params_from_iter(params.iter().copied()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(col_count);
            for i in 0..col_count {
                values.push(row.get::<_, Value>(i)?);
            }
            out.push(OwnedRow {
                columns: columns.clone(),
                values,
            });
        }
        Ok(out)
    }

    fn exec_in(conn: &Connection, name: &str, mut params: &[&dyn ToSql]) -> FFResult<u64> {
        let sql = Self::read_sql(name)?;
        let mut total: u64 = 0;
        for stmt_sql in sql.split(';') {
            if stmt_sql.trim().is_empty() {
                continue;
            }
            let n = Self::count_params(stmt_sql);
            let mut stmt = conn.prepare_cached(stmt_sql)?;
            let r = stmt.execute(params_from_iter(params[..n].iter().copied()))?;
            total += r as u64;
            params = &params[n..];
        }
        Ok(total)
    }

    fn exec(conn: &mut Connection, name: &str, params: &[&dyn ToSql]) -> FFResult<u64> {
        let tx = conn.transaction()?;
        let n = Self::exec_in(&tx, name, params)?;
        tx.commit()?;
        Ok(n)
    }

    fn save_player_sync(tx: &rusqlite::Transaction, player: &Player) -> FFResult<()> {
        let pc_uid = player.get_uid();

        let position = if player.instance_id.instance_num.is_some() {
            player.get_pre_warp().position
        } else {
            player.get_position()
        };

        let nano_slots = player.nano_data.as_slots();
        let skyway_bytes = player.flags.skyway_flags.to_bytes();
        let tip_bytes = player.flags.tip_flags.to_bytes();
        let quest_bytes = player.mission_journal.completed_mission_flags.to_bytes();

        Self::exec_in(
            tx,
            "save_player",
            &[
                &pc_uid,
                &(player.get_level() as Int),
                &(nano_slots[0] as Int),
                &(nano_slots[1] as Int),
                &(nano_slots[2] as Int),
                &(player.flags.tutorial_flag as Int),
                &(player.flags.payzone_flag as Int),
                &position.x,
                &position.y,
                &position.z,
                &player.get_rotation(),
                &player.get_hp(),
                &(player.get_fusion_matter() as Int),
                &(player.get_taros() as Int),
                &(player.get_weapon_boosts() as Int),
                &(player.get_nano_potions() as Int),
                &((player.get_guide() as i16) as Int),
                &player.mission_journal.get_active_mission_id().unwrap_or(0),
                &player.flags.scamper_flags.get_chunk(0).unwrap(),
                &skyway_bytes.as_slice(),
                &tip_bytes.as_slice(),
                &quest_bytes.as_slice(),
            ],
        )?;

        Self::exec_in(tx, "clear_nanos", &[&pc_uid])?;
        {
            let save_nano_sql = Self::read_sql("save_nano")?;
            let mut save_nano = tx.prepare_cached(save_nano_sql)?;
            for nano in player.get_nano_iter() {
                let nano_raw: sNano = Some(nano).into_proto();
                save_nano.execute(params_from_iter(
                    [
                        &pc_uid as &dyn ToSql,
                        &(nano_raw.iID as Int),
                        &(nano_raw.iSkillID as Int),
                        &(nano_raw.iStamina as Int),
                    ]
                    .iter()
                    .copied(),
                ))?;
            }
        }

        Self::exec_in(tx, "clear_items", &[&pc_uid])?;
        {
            let save_item_sql = Self::read_sql("save_item")?;
            let mut save_item = tx.prepare_cached(save_item_sql)?;
            for (slot_num, item) in player.get_item_iter() {
                let item_raw: v0104::sItemBase = Some(*item).into_proto();
                save_item.execute(params_from_iter(
                    [
                        &pc_uid as &dyn ToSql,
                        &(slot_num as Int),
                        &(item_raw.iID as Int),
                        &(item_raw.iType as Int),
                        &item_raw.iOpt,
                        &item_raw.iTimeLimit,
                    ]
                    .iter()
                    .copied(),
                ))?;
            }
        }

        Self::exec_in(tx, "clear_quest_items", &[&pc_uid])?;
        {
            let save_qi_sql = Self::read_sql("save_quest_item")?;
            let mut save_qi = tx.prepare_cached(save_qi_sql)?;
            for (virtual_slot, (item_id, count)) in player.get_quest_item_iter().enumerate() {
                save_qi.execute(params_from_iter(
                    [
                        &pc_uid as &dyn ToSql,
                        &(item_id as Int),
                        &(count as Int),
                        &(virtual_slot as Int),
                    ]
                    .iter()
                    .copied(),
                ))?;
            }
        }

        Self::exec_in(tx, "clear_running_quests", &[&pc_uid])?;
        {
            let save_rq_sql = Self::read_sql("save_running_quest")?;
            let mut save_rq = tx.prepare_cached(save_rq_sql)?;
            for task in player.mission_journal.get_running_quests() {
                if task.m_aCurrTaskID == 0 {
                    continue;
                }
                save_rq.execute(params_from_iter(
                    [
                        &pc_uid as &dyn ToSql,
                        &(task.m_aCurrTaskID as Int),
                        &(task.m_aKillNPCCount[0] as Int),
                        &(task.m_aKillNPCCount[1] as Int),
                        &(task.m_aKillNPCCount[2] as Int),
                    ]
                    .iter()
                    .copied(),
                ))?;
            }
        }

        Self::exec_in(tx, "clear_buddies", &[&pc_uid])?;
        for buddy_uid in player.get_buddy_uids() {
            Self::exec_in(tx, "save_buddy", &[&pc_uid, &buddy_uid])?;
        }

        Self::exec_in(tx, "clear_blocks", &[&pc_uid])?;
        for blocked_uid in player.get_blocked_uids() {
            Self::exec_in(tx, "save_block", &[&pc_uid, &blocked_uid])?;
        }

        Ok(())
    }

    fn load_player_sync(conn: &Connection, row: &OwnedRow, load_buddies: bool) -> FFResult<Player> {
        let pc_uid: BigInt = row.get("PlayerId");
        let slot_num: Int = row.get("Slot");
        let mut player = Player::new(pc_uid, slot_num as usize);
        let appearance_flag: Int = row.get("AppearanceFlag");
        player.style = if appearance_flag != 0 {
            Some(PlayerStyle {
                gender: row.get::<Int>("Gender") as i8,
                face_style: row.get::<Int>("FaceStyle") as i8,
                hair_style: row.get::<Int>("HairStyle") as i8,
                hair_color: row.get::<Int>("HairColor") as i8,
                skin_color: row.get::<Int>("SkinColor") as i8,
                eye_color: row.get::<Int>("EyeColor") as i8,
                height: row.get::<Int>("Height") as i8,
                body: row.get::<Int>("Body") as i8,
            })
        } else {
            None
        };

        let first_name: String = row.get("FirstName");
        let last_name: String = row.get("LastName");
        player.first_name = first_name;
        player.last_name = last_name;

        player.set_position(Position {
            x: row.get("XCoordinate"),
            y: row.get("YCoordinate"),
            z: row.get("ZCoordinate"),
        });
        player.set_rotation(row.get("Angle"));

        player.set_taros(row.get::<Int>("Taros") as u32);
        player.set_level(row.get::<Int>("Level") as i16)?;
        player.set_fusion_matter(row.get::<Int>("FusionMatter") as u32);
        player.set_hp(row.get("HP"));
        player.set_weapon_boosts(row.get::<Int>("BatteryW") as u32);
        player.set_nano_potions(row.get::<Int>("BatteryN") as u32);

        let nano_col_names = ["Nano1", "Nano2", "Nano3"];
        for (slot, col_name) in nano_col_names.iter().enumerate() {
            let nano_id = row.get::<Int>(col_name) as i16;
            player.change_nano(slot, if nano_id == 0 { None } else { Some(nano_id) })?;
        }

        let nanos = Self::query(conn, "load_nanos", &[&pc_uid])?;
        for nano in &nanos {
            let nano_raw = sNano {
                iID: nano.get::<Int>("ID") as i16,
                iSkillID: nano.get::<Int>("Skill") as i16,
                iStamina: nano.get::<Int>("Stamina") as i16,
            };
            let nano: Option<Nano> = nano_raw.into_proto();
            if let Some(nano) = nano {
                player.set_nano(nano);
            }
        }

        let mut player_flags = PlayerFlags::default();
        let first_use_bytes: Vec<u8> = row.get("FirstUseFlag");
        player_flags.tip_flags = Bitfield::from_bytes(&first_use_bytes, SIZEOF_TIP_FLAGS)?;
        player_flags.tutorial_flag = row.get::<Int>("TutorialFlag") != 0;
        player_flags.name_check = (row.get::<Int>("NameCheck") as i8).try_into()?;
        player.flags = player_flags;

        let guide: PlayerGuide = (row.get::<Int>("Mentor") as i16).try_into()?;
        if guide != PlayerGuide::Computress {
            player.update_guide(guide);
        }

        let skyway_bytes: Vec<u8> = row.get("SkywayLocationFlag");
        player.flags.skyway_flags =
            Bitfield::from_bytes(&skyway_bytes, WYVERN_LOCATION_FLAG_SIZE as usize)?;

        player
            .flags
            .scamper_flags
            .set_chunk(0, row.get("WarpLocationFlag"))
            .unwrap();

        let quest_bytes: Vec<u8> = row.get("Quests");
        player.mission_journal.completed_mission_flags =
            Bitfield::from_bytes(&quest_bytes, SIZEOF_QUESTFLAG_NUMBER as usize)?;

        let running_quests = Self::query(conn, "load_running_quests", &[&pc_uid])?;
        for quest in &running_quests {
            let task_id: Int = quest.get("TaskID");
            let task_def = tdata_get().get_task_definition(task_id)?;
            let npc_count_1: Int = quest.get("RemainingNPCCount1");
            let npc_count_2: Int = quest.get("RemainingNPCCount2");
            let npc_count_3: Int = quest.get("RemainingNPCCount3");
            let mut task: Task = task_def.into();
            task.fail_time = None;
            task.set_remaining_enemy_defeats([
                npc_count_1 as usize,
                npc_count_2 as usize,
                npc_count_3 as usize,
            ]);
            player
                .mission_journal
                .start_task(task, player.get_level())?;
        }

        let active_mission_id: Int = row.get("CurrentMissionID");
        if active_mission_id != 0 {
            log_if_failed(
                player
                    .mission_journal
                    .set_active_mission_id(row.get("CurrentMissionID")),
            );
        }

        let items = Self::query(conn, "load_items", &[&pc_uid])?;
        for item in &items {
            let slot_num = item.get::<Int>("Slot") as usize;
            let item_raw = v0104::sItemBase {
                iType: item.get::<Int>("Type") as i16,
                iID: item.get::<Int>("ID") as i16,
                iOpt: item.get::<Int>("Opt"),
                iTimeLimit: item.get::<Int>("TimeLimit"),
            };

            let item: Option<Item> = item_raw.try_into_proto()?;
            let (loc, local_slot) = util::slot_num_to_loc_and_slot_num(slot_num)?;
            if (loc != crate::enums::ItemLocation::Bank
                || config_get().shard.remove_expired_items_from_bank.get())
                && item.is_some_and(|item| item.has_expired(SystemTime::now()))
            {
                continue;
            }

            player.set_item(loc, local_slot, item)?;
        }

        let quest_items = Self::query(conn, "load_quest_items", &[&pc_uid])?;
        for quest_item in &quest_items {
            let item_id: Int = quest_item.get("ID");
            let count: Int = quest_item.get("Opt");
            player.set_quest_item_count(item_id as i16, count as usize)?;
        }

        if load_buddies {
            log_if_failed(Self::load_buddies_sync(conn, &mut player));
            log_if_failed(Self::load_blocks_sync(conn, &mut player));
        }

        let perms: Int = row.get("AccountLevel");
        player.perms = perms as i16;

        Ok(player)
    }

    fn load_buddy_entry_sync(
        conn: &Connection,
        buddy_uid: BigInt,
    ) -> FFResult<Option<BuddyListEntry>> {
        let rows = Self::query(conn, "load_player_lite", &[&buddy_uid])?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let appearance_flag: Int = row.get("AppearanceFlag");
        if appearance_flag == 0 {
            return Err(FFError::build(
                Severity::Warning,
                format!("Buddy {} has no appearance set", buddy_uid),
            ));
        }
        let style = PlayerStyle {
            gender: row.get::<Int>("Gender") as i8,
            face_style: row.get::<Int>("FaceStyle") as i8,
            hair_style: row.get::<Int>("HairStyle") as i8,
            hair_color: row.get::<Int>("HairColor") as i8,
            skin_color: row.get::<Int>("SkinColor") as i8,
            eye_color: row.get::<Int>("EyeColor") as i8,
            height: row.get::<Int>("Height") as i8,
            body: row.get::<Int>("Body") as i8,
        };
        Ok(Some(BuddyListEntry {
            pc_uid: row.get("PlayerId"),
            first_name: row.get("FirstName"),
            last_name: row.get("LastName"),
            style,
            name_check: (row.get::<Int>("NameCheck") as i8).try_into()?,
            free_chat: true,
            blocked: false,
        }))
    }

    fn load_buddies_sync(conn: &Connection, player: &mut Player) -> FFResult<()> {
        let rows = Self::query(conn, "load_buddy_ids", &[&player.get_uid()])?;
        for row in &rows {
            let buddy_uid: BigInt = row.get("PlayerBId");
            match Self::load_buddy_entry_sync(conn, buddy_uid) {
                Ok(Some(buddy_info)) => {
                    log_if_failed(player.add_buddy(buddy_info));
                }
                Ok(None) => {
                    log(
                        Severity::Warning,
                        &format!("Buddy with UID {} not found", buddy_uid),
                    );
                }
                Err(e) => {
                    log(
                        Severity::Warning,
                        &format!(
                            "Failed to load buddy with UID {}: {}",
                            buddy_uid,
                            e.get_msg()
                        ),
                    );
                }
            }
        }
        Ok(())
    }

    fn load_blocks_sync(conn: &Connection, player: &mut Player) -> FFResult<()> {
        let rows = Self::query(conn, "load_blocked_ids", &[&player.get_uid()])?;
        for row in &rows {
            let blocked_uid: BigInt = row.get("BlockedPlayerId");
            log_if_failed(player.block_player(blocked_uid));
        }
        Ok(())
    }
}

#[async_trait]
impl DbImpl for SqliteDatabase {
    async fn get_db_version(&self) -> FFResult<Int> {
        let conn = self.pool.get().await?;
        conn.interact(|conn| -> FFResult<Int> {
            let sql = Self::read_sql("get_db_version")?;
            let mut stmt = conn.prepare_cached(sql)?;
            let mut rows = stmt.query([])?;
            match rows.next()? {
                Some(row) => Ok(row.get::<_, Int>(0)?),
                None => Err(FFError::build(
                    db_error_severity(),
                    "Meta table has no DatabaseVersion row".to_string(),
                )),
            }
        })
        .await?
    }

    async fn get_protocol_version(&self) -> FFResult<Int> {
        let conn = self.pool.get().await?;
        conn.interact(|conn| -> FFResult<Int> {
            let sql = Self::read_sql("get_protocol_version")?;
            let mut stmt = conn.prepare_cached(sql)?;
            let mut rows = stmt.query([])?;
            match rows.next()? {
                Some(row) => Ok(row.get::<_, Int>(0)?),
                None => Err(FFError::build(
                    db_error_severity(),
                    "Meta table has no ProtocolVersion row".to_string(),
                )),
            }
        })
        .await?
    }

    async fn init_player(&self, acc_id: BigInt, player: &Player) -> FFResult<()> {
        let conn = self.pool.get().await?;
        let player = player.clone();
        let updated = conn
            .interact(move |conn| -> FFResult<u64> {
                let pc_uid = player.get_uid();
                let position = player.get_position();
                let skyway_bytes = player.flags.skyway_flags.to_bytes();
                let tip_bytes = player.flags.tip_flags.to_bytes();
                let quest_bytes = player.mission_journal.completed_mission_flags.to_bytes();
                Self::exec(
                    conn,
                    "init_player",
                    &[
                        &pc_uid,
                        &acc_id,
                        &player.first_name,
                        &player.last_name,
                        &(player.get_style().iNameCheck as Int),
                        &(player.get_slot_num() as Int),
                        &position.x,
                        &position.y,
                        &position.z,
                        &player.get_rotation(),
                        &player.get_hp(),
                        &skyway_bytes.as_slice(),
                        &tip_bytes.as_slice(),
                        &quest_bytes.as_slice(),
                        &pc_uid,
                    ],
                )
            })
            .await??;
        assert_eq!(updated, 1 + 1);
        Ok(())
    }

    async fn update_player_appearance(&self, player: &Player) -> FFResult<()> {
        let conn = self.pool.get().await?;
        let player = player.clone();
        let updated = conn
            .interact(move |conn| -> FFResult<u64> {
                let style = player.style.unwrap_or_default();
                let appearance_flag: Int = if player.style.is_some() { 1 } else { 0 };
                Self::exec(
                    conn,
                    "update_appearance",
                    &[
                        &player.get_uid(),
                        &(style.body as Int),
                        &(style.eye_color as Int),
                        &(style.face_style as Int),
                        &(style.gender as Int),
                        &(style.hair_color as Int),
                        &(style.hair_style as Int),
                        &(style.height as Int),
                        &(style.skin_color as Int),
                        &player.get_uid(),
                        &appearance_flag,
                    ],
                )
            })
            .await??;
        assert_eq!(updated, 1 + 1);
        Ok(())
    }

    async fn find_account_from_username(&self, username: &str) -> FFResult<Option<Account>> {
        let conn = self.pool.get().await?;
        let username = username.to_string();
        conn.interact(move |conn| -> FFResult<Option<Account>> {
            let rows = Self::query(conn, "find_account", &[&username])?;
            assert!(rows.len() <= 1);
            let row = match rows.first() {
                Some(r) => r,
                None => return Ok(None),
            };

            let cookie_str: Option<String> = row.get("Cookie");
            let cookie = cookie_str.map(|token| {
                let expires_sec: Int = row.get("Expires");
                Cookie {
                    token,
                    expires: util::get_systime_from_sec(expires_sec as u64),
                }
            });

            let account = Account {
                id: row.get("AccountId"),
                username,
                password_hashed: row.get("Password"),
                cookie,
                selected_slot: row.get::<Int>("Selected") as u8,
                account_level: row.get::<Int>("AccountLevel") as i16,
                banned_until: util::get_systime_from_sec(row.get::<Int>("BannedUntil") as u64),
                ban_reason: row.get("BanReason"),
            };

            log_if_failed(Self::exec(
                conn,
                "invalidate_cookie_for_account",
                &[&account.id],
            ));
            Ok(Some(account))
        })
        .await?
    }

    async fn find_account_from_player(&self, pc_uid: BigInt) -> FFResult<Option<Account>> {
        let conn = self.pool.get().await?;
        conn.interact(move |conn| -> FFResult<Option<Account>> {
            let rows = Self::query(conn, "find_account_from_player", &[&pc_uid])?;
            if rows.is_empty() {
                return Ok(None);
            }
            let row = &rows[0];
            Ok(Some(Account {
                id: row.get("AccountId"),
                username: row.get("Login"),
                password_hashed: row.get("Password"),
                cookie: None,
                selected_slot: row.get::<Int>("Selected") as u8,
                account_level: row.get::<Int>("AccountLevel") as i16,
                banned_until: util::get_systime_from_sec(row.get::<Int>("BannedUntil") as u64),
                ban_reason: row.get("BanReason"),
            }))
        })
        .await?
    }

    async fn create_account(&self, username: &str, password_hashed: &str) -> FFResult<Account> {
        {
            let conn = self.pool.get().await?;
            let username = username.to_string();
            let password_hashed = password_hashed.to_string();
            let updated = conn
                .interact(move |conn| -> FFResult<u64> {
                    let acc_level = if Self::query(conn, "enum_account_ids", &[])
                        .is_ok_and(|rows| rows.is_empty())
                    {
                        CN_ACCOUNT_LEVEL__MASTER
                    } else {
                        config_get().login.default_account_level.get()
                    } as Int;

                    Self::exec(
                        conn,
                        "create_account",
                        &[&username, &password_hashed, &acc_level],
                    )
                })
                .await??;
            assert_eq!(updated, 1);
        }

        let new_acc = self.find_account_from_username(username).await?.unwrap();
        Ok(new_acc)
    }

    async fn change_account_level(&self, acc_id: BigInt, new_level: Int) -> FFResult<()> {
        let conn = self.pool.get().await?;
        let updated = conn
            .interact(move |conn| Self::exec(conn, "change_account_level", &[&acc_id, &new_level]))
            .await??;
        if updated == 0 {
            log(
                Severity::Warning,
                &format!(
                    "Failed to change account level for account with ID {}",
                    acc_id
                ),
            );
        }
        Ok(())
    }

    async fn ban_account(
        &self,
        acc_id: BigInt,
        banned_until: SystemTime,
        ban_reason: &str,
    ) -> FFResult<()> {
        let conn = self.pool.get().await?;
        let banned_since = util::get_timestamp_sec(SystemTime::now()) as Int;
        let banned_until = util::get_timestamp_sec(banned_until) as Int;
        let ban_reason = ban_reason.to_string();
        let updated = conn
            .interact(move |conn| {
                Self::exec(
                    conn,
                    "ban_account",
                    &[&acc_id, &banned_since, &banned_until, &ban_reason],
                )
            })
            .await??;
        if updated == 0 {
            log(
                Severity::Warning,
                &format!("Failed to ban account with ID {}", acc_id),
            );
        }
        Ok(())
    }

    async fn unban_account(&self, acc_id: BigInt) -> FFResult<()> {
        let conn = self.pool.get().await?;
        let updated = conn
            .interact(move |conn| Self::exec(conn, "unban_account", &[&acc_id]))
            .await??;
        if updated == 0 {
            log(
                Severity::Warning,
                &format!("Failed to unban account with ID {}", acc_id),
            );
        }
        Ok(())
    }

    async fn update_selected_player(&self, acc_id: BigInt, slot_num: Int) -> FFResult<()> {
        let conn = self.pool.get().await?;
        let timestamp_now = util::get_timestamp_sec(SystemTime::now()) as Int;
        let updated = conn
            .interact(move |conn| {
                Self::exec(
                    conn,
                    "update_selected",
                    &[&acc_id, &slot_num, &timestamp_now],
                )
            })
            .await??;
        assert_eq!(updated, 1);
        Ok(())
    }

    async fn load_player(&self, acc_id: BigInt, pc_uid: BigInt) -> FFResult<Option<Player>> {
        let conn = self.pool.get().await?;
        conn.interact(move |conn| -> FFResult<Option<Player>> {
            let rows = Self::query(conn, "load_players", &[&acc_id])?;
            for row in &rows {
                if row.get::<BigInt>("PlayerId") == pc_uid {
                    return Self::load_player_sync(conn, row, true).map(Some);
                }
            }
            Ok(None)
        })
        .await?
    }

    async fn load_players(&self, acc_id: BigInt) -> FFResult<Vec<Player>> {
        let conn = self.pool.get().await?;
        conn.interact(move |conn| -> FFResult<Vec<Player>> {
            let rows = Self::query(conn, "load_players", &[&acc_id])?;
            let mut players = Vec::with_capacity(rows.len());
            for row in &rows {
                match Self::load_player_sync(conn, row, true) {
                    Ok(p) => players.push(p),
                    Err(e) => {
                        let pc_uid: BigInt = row.get("PlayerId");
                        log(
                            Severity::Warning,
                            &format!("Failed to load player {}: {}", pc_uid, e.get_msg()),
                        );
                    }
                }
            }
            Ok(players)
        })
        .await?
    }

    async fn save_player(&self, player: &Player) -> FFResult<()> {
        let conn = self.pool.get().await?;
        let player = player.clone();
        conn.interact(move |conn| -> FFResult<()> {
            let tx = conn.transaction()?;
            Self::save_player_sync(&tx, &player)?;
            tx.commit()?;
            Ok(())
        })
        .await?
    }

    async fn save_players(&self, players: &[&Player]) -> FFResult<()> {
        let conn = self.pool.get().await?;
        let players: Vec<Player> = players.iter().map(|p| (*p).clone()).collect();
        conn.interact(move |conn| -> FFResult<()> {
            let tx = conn.transaction()?;
            for player in &players {
                Self::save_player_sync(&tx, player)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?
    }

    async fn delete_player(&self, pc_uid: BigInt) -> FFResult<()> {
        let conn = self.pool.get().await?;
        let updated = conn
            .interact(move |conn| Self::exec(conn, "delete_player", &[&pc_uid]))
            .await??;
        assert_eq!(updated, 1);
        Ok(())
    }

    async fn is_code_redeemed(&self, pc_uid: BigInt, code: &str) -> FFResult<bool> {
        let conn = self.pool.get().await?;
        let code = code.to_string();
        let rows = conn
            .interact(move |conn| Self::query(conn, "is_code_redeemed", &[&pc_uid, &code]))
            .await??;
        Ok(!rows.is_empty())
    }

    async fn record_code_redemption(&self, pc_uid: BigInt, code: &str) -> FFResult<()> {
        let conn = self.pool.get().await?;
        let code = code.to_string();
        let updated = conn
            .interact(move |conn| Self::exec(conn, "record_code_redemption", &[&pc_uid, &code]))
            .await??;
        assert_eq!(updated, 1);
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn init_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        let sql = std::fs::read_to_string("sql/sqlite/create_tables.sql").unwrap();
        let sql = sql.replace("?1", "0");
        conn.execute_batch(&sql).unwrap();
        conn
    }

    fn preprocess(sql: &str) -> String {
        Regex::new(r"\$([0-9]+)")
            .unwrap()
            .replace_all(sql, "?$1")
            .into_owned()
    }

    #[test]
    fn test_all_sql_files_prepare() {
        let conn = init_db();
        let skip = |name: &str| name == "create_tables.sql" || name == "reset_db.sql";

        let overrides: std::collections::HashSet<String> = std::fs::read_dir("sql/sqlite")
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();

        let mut tried = 0;
        for entry in std::fs::read_dir("sql").unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if skip(&name) || overrides.contains(&name) {
                continue;
            }
            let sql = preprocess(&std::fs::read_to_string(&path).unwrap());
            for stmt_sql in sql.split(';') {
                if stmt_sql.trim().is_empty() {
                    continue;
                }
                conn.prepare(stmt_sql).unwrap_or_else(|e| {
                    panic!("Failed to prepare sql/{}: {}\nSQL:\n{}", name, e, stmt_sql)
                });
            }
            tried += 1;
        }

        for name in &overrides {
            if skip(name) {
                continue;
            }
            let sql = std::fs::read_to_string(format!("sql/sqlite/{}", name)).unwrap();
            for stmt_sql in sql.split(';') {
                if stmt_sql.trim().is_empty() {
                    continue;
                }
                conn.prepare(stmt_sql).unwrap_or_else(|e| {
                    panic!(
                        "Failed to prepare sql/sqlite/{}: {}\nSQL:\n{}",
                        name, e, stmt_sql
                    )
                });
            }
            tried += 1;
        }

        assert!(
            tried > 30,
            "expected to exercise >30 SQL files, got {}",
            tried
        );
    }

    use crate::database::{test_suite, Database};
    use crate::util::TempFile;

    async fn setup_db() -> (TempFile, Database<SqliteDatabase>) {
        test_suite::ensure_init();
        let tmp = TempFile::new().expect("temp file");
        let path = tmp.path().to_str().expect("temp path utf-8").to_string();
        let cfg = test_suite::build_config(&path);
        let inner = SqliteDatabase::connect(&cfg.general)
            .await
            .expect("sqlite connect");
        (tmp, Database::new(inner))
    }

    macro_rules! run {
        ($name:ident) => {
            #[tokio::test]
            async fn $name() {
                let (_tmp, db) = setup_db().await;
                test_suite::$name(&db).await;
            }
        };
    }

    crate::for_each_db_test!(run);

    #[tokio::test]
    async fn test_load_sample_db_players() {
        test_suite::ensure_init();

        let tmp = TempFile::new().expect("temp file");
        std::fs::copy("sample.db", tmp.path()).expect("copy sample.db");
        let path = tmp.path().to_str().expect("temp path utf-8").to_string();
        let cfg = test_suite::build_config(&path);

        let inner = SqliteDatabase::connect(&cfg.general)
            .await
            .expect("sqlite connect to sample.db");
        let db = Database::new(inner);

        let expected_by_account: std::collections::BTreeMap<BigInt, Vec<BigInt>> = {
            let raw = Connection::open(tmp.path()).expect("raw open sample.db");
            let mut stmt = raw
                .prepare(
                    "SELECT p.AccountID, p.PlayerID FROM Players p \
                     INNER JOIN Accounts a ON p.AccountID = a.AccountID \
                     ORDER BY p.AccountID, p.PlayerID",
                )
                .expect("prepare players");
            let rows: Vec<(BigInt, BigInt)> = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
                .expect("query players")
                .map(|r| r.expect("row"))
                .collect();
            let mut map: std::collections::BTreeMap<BigInt, Vec<BigInt>> =
                std::collections::BTreeMap::new();
            for (acc, uid) in rows {
                map.entry(acc).or_default().push(uid);
            }
            map
        };

        let expected_total: usize = expected_by_account.values().map(|v| v.len()).sum();
        assert!(expected_total > 0, "sample.db should contain players");

        let mut missing: Vec<BigInt> = Vec::new();
        for (acc_id, expected_uids) in &expected_by_account {
            let loaded = db
                .load_players(*acc_id)
                .await
                .unwrap_or_else(|e| panic!("load_players({}) errored: {}", acc_id, e.get_msg()));
            let loaded_uids: std::collections::HashSet<BigInt> =
                loaded.iter().map(|p| p.get_uid()).collect();
            for uid in expected_uids {
                if !loaded_uids.contains(uid) {
                    missing.push(*uid);
                }
            }
        }

        if !missing.is_empty() {
            missing.sort();
            panic!(
                "{} of {} players failed to load from sample.db; missing PlayerIDs: {:?}",
                missing.len(),
                expected_total,
                missing
            );
        }
    }
}

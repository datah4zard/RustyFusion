use std::{sync::LazyLock, time::Duration};

use async_trait::async_trait;
use deadpool_postgres::*;
use tokio_postgres as postgres;

use postgres::{tls, types::ToSql, Row};
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

impl From<postgres::Error> for FFError {
    fn from(e: postgres::Error) -> Self {
        let e = if let Some(e) = e.as_db_error() {
            e.to_string()
        } else {
            e.to_string()
        };

        FFError::build(db_error_severity(), "Database error".to_string())
            .with_parent(FFError::build(Severity::Debug, e))
    }
}
impl From<deadpool_postgres::PoolError> for FFError {
    fn from(e: deadpool_postgres::PoolError) -> Self {
        let e = if let deadpool_postgres::PoolError::Backend(ref e) = e {
            if let Some(e) = e.as_db_error() {
                e.to_string()
            } else {
                e.to_string()
            }
        } else {
            e.to_string()
        };

        FFError::build(db_error_severity(), "Database pool error".to_string())
            .with_parent(FFError::build(Severity::Debug, e))
    }
}

pub struct PostgresDatabase {
    pool: Pool,
    config: Config,
}
impl std::fmt::Debug for PostgresDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Postgres Database ({}@{}:{})",
            self.config.user.as_deref().unwrap(),
            self.config.host.as_deref().unwrap(),
            self.config.port.unwrap()
        )
    }
}
impl PostgresDatabase {
    pub async fn connect(config: &GeneralConfig) -> FFResult<Self> {
        let mut db_config = Config::new();
        db_config.host = Some(config.db_host.get());
        db_config.port = Some(config.db_port.get());
        db_config.user = Some(config.db_username.get());
        db_config.password = Some(config.db_password.get());
        db_config.dbname = Some(config.db_name.get());
        db_config.connect_timeout = Some(Duration::from_secs(5));
        db_config.manager = Some(ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        });

        let pool = db_config
            .create_pool(Some(Runtime::Tokio1), tls::NoTls)
            .map_err(|e| {
                FFError::build(
                    Severity::Warning,
                    format!("Failed to create database pool: {}", e),
                )
            })?;

        let mut db_client = pool.get().await?;

        let meta_table_exists: bool =
            Self::query(&db_client, "meta_table_exists", &[]).await?[0].get(0);

        let protocol_version = config.protocol.get().version_number();

        if !meta_table_exists {
            log(
                Severity::Info,
                "Meta table missing; initializing database...",
            );
            Self::exec(
                &mut db_client,
                "create_tables",
                &[&protocol_version, &DB_VERSION],
            )
            .await?;
        }

        Ok(Self {
            pool,
            config: db_config,
        })
    }

    fn read_sql(name: &str) -> FFResult<&'static str> {
        crate::database::get_sql_string(name)
    }

    async fn query(
        client: &impl GenericClient,
        name: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> FFResult<Vec<Row>> {
        let query = Self::read_sql(name)?;
        let rows = client.query(query, params).await?;

        Ok(rows)
    }

    async fn prep(client: &impl GenericClient, name: &str) -> FFResult<postgres::Statement> {
        let query = Self::read_sql(name)?;
        let stmt = client.prepare(query).await?;
        Ok(stmt)
    }

    async fn exec(
        client: &mut impl GenericClient,
        name: &str,
        mut params: &[&(dyn ToSql + Sync)],
    ) -> FFResult<u64> {
        static SQL_PARAMETER_REGEX: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\$[0-9]+").unwrap());
        let calc_num_params = |s: &str| {
            // we can use the parameter with the highest number to determine the number of parameters
            let max_param = SQL_PARAMETER_REGEX
                .find_iter(s)
                .map(|m| m.as_str()[1..].parse::<usize>().unwrap())
                .max();
            max_param.unwrap_or(0)
        };

        let queries = Self::read_sql(name)?;
        let queries: Vec<&str> = queries.split(';').collect();

        // implicit transaction
        let tsct = client.transaction().await?;

        let mut num_updated = 0;
        for query in queries {
            if query.trim().is_empty() {
                continue;
            }

            let num_params = calc_num_params(query);
            let r = tsct.execute(query, &params[..num_params]).await?;
            num_updated += r;
            params = &params[num_params..];
        }

        tsct.commit().await?;
        Ok(num_updated)
    }

    async fn save_player_internal(
        client: &mut impl GenericClient,
        player: &Player,
    ) -> FFResult<()> {
        let mut tsct = client.transaction().await?;
        let client = &mut tsct;
        let save_item = Self::prep(client, "save_item").await?;
        let save_quest_item = Self::prep(client, "save_quest_item").await?;
        let save_nano = Self::prep(client, "save_nano").await?;
        let save_running_quest = Self::prep(client, "save_running_quest").await?;
        let pc_uid = player.get_uid();

        let position = if player.instance_id.instance_num.is_some() {
            player.get_pre_warp().position
        } else {
            player.get_position()
        };

        let nano_slots = player.nano_data.as_slots();

        Self::exec(
            client,
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
                &player.flags.skyway_flags.to_bytes().as_slice(),
                &player.flags.tip_flags.to_bytes().as_slice(),
                &player
                    .mission_journal
                    .completed_mission_flags
                    .to_bytes()
                    .as_slice(),
            ],
        )
        .await?;

        Self::exec(client, "clear_nanos", &[&pc_uid]).await?;
        for nano in player.get_nano_iter() {
            let nano_raw: sNano = Some(nano).into_proto();
            client
                .execute(
                    &save_nano,
                    &[
                        &pc_uid,
                        &(nano_raw.iID as Int),
                        &(nano_raw.iSkillID as Int),
                        &(nano_raw.iStamina as Int),
                    ],
                )
                .await?;
        }

        Self::exec(client, "clear_items", &[&pc_uid]).await?;
        for (slot_num, item) in player.get_item_iter() {
            let item_raw: v0104::sItemBase = Some(*item).into_proto();
            client
                .execute(
                    &save_item,
                    &[
                        &pc_uid,
                        &(slot_num as Int),
                        &(item_raw.iID as Int),
                        &(item_raw.iType as Int),
                        &item_raw.iOpt,
                        &item_raw.iTimeLimit,
                    ],
                )
                .await?;
        }

        Self::exec(client, "clear_quest_items", &[&pc_uid]).await?;
        for (virtual_slot, (item_id, count)) in player.get_quest_item_iter().enumerate() {
            client
                .execute(
                    &save_quest_item,
                    &[
                        &pc_uid,
                        &(item_id as Int),
                        &(count as Int),
                        &(virtual_slot as Int),
                    ],
                )
                .await?;
        }

        Self::exec(client, "clear_running_quests", &[&pc_uid]).await?;
        for task in player.mission_journal.get_running_quests() {
            if task.m_aCurrTaskID == 0 {
                continue;
            }

            client
                .execute(
                    &save_running_quest,
                    &[
                        &pc_uid,
                        &(task.m_aCurrTaskID as Int),
                        &(task.m_aKillNPCCount[0] as Int),
                        &(task.m_aKillNPCCount[1] as Int),
                        &(task.m_aKillNPCCount[2] as Int),
                    ],
                )
                .await?;
        }

        Self::exec(client, "clear_buddies", &[&pc_uid]).await?;
        for buddy_uid in player.get_buddy_uids() {
            Self::exec(client, "save_buddy", &[&pc_uid, &buddy_uid]).await?;
        }

        Self::exec(client, "clear_blocks", &[&pc_uid]).await?;
        for blocked_uid in player.get_blocked_uids() {
            Self::exec(client, "save_block", &[&pc_uid, &blocked_uid]).await?;
        }

        tsct.commit().await?;
        Ok(())
    }

    async fn load_player_internal(
        client: &impl GenericClient,
        row: &Row,
        load_buddies: bool,
    ) -> FFResult<Player> {
        let pc_uid = row.get("PlayerId");
        let slot_num: Int = row.get("Slot");
        let mut player = Player::new(pc_uid, slot_num as usize);
        let appearance_flag: Int = row.get("AppearanceFlag");
        player.style = if appearance_flag != 0 {
            Some(PlayerStyle {
                gender: row.get::<_, Int>("Gender") as i8,
                face_style: row.get::<_, Int>("FaceStyle") as i8,
                hair_style: row.get::<_, Int>("HairStyle") as i8,
                hair_color: row.get::<_, Int>("HairColor") as i8,
                skin_color: row.get::<_, Int>("SkinColor") as i8,
                eye_color: row.get::<_, Int>("EyeColor") as i8,
                height: row.get::<_, Int>("Height") as i8,
                body: row.get::<_, Int>("Body") as i8,
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

        player.set_taros(row.get::<_, Int>("Taros") as u32);
        player.set_level(row.get::<_, Int>("Level") as i16)?;
        // fusion matter must be set after level
        player.set_fusion_matter(row.get::<_, Int>("FusionMatter") as u32);
        player.set_hp(row.get("HP"));
        player.set_weapon_boosts(row.get::<_, Int>("BatteryW") as u32);
        player.set_nano_potions(row.get::<_, Int>("BatteryN") as u32);

        let nano_col_names = ["Nano1", "Nano2", "Nano3"];
        for (slot, col_name) in nano_col_names.iter().enumerate() {
            let nano_id = row.get::<_, Int>(col_name) as i16;
            player.change_nano(slot, if nano_id == 0 { None } else { Some(nano_id) })?;
        }
        let nanos = Self::query(client, "load_nanos", &[&pc_uid]).await?;
        for nano in nanos {
            let nano_raw = sNano {
                iID: nano.get::<_, Int>("ID") as i16,
                iSkillID: nano.get::<_, Int>("Skill") as i16,
                iStamina: nano.get::<_, Int>("Stamina") as i16,
            };
            let nano: Option<Nano> = nano_raw.into_proto();
            if let Some(nano) = nano {
                player.set_nano(nano);
            }
        }

        let mut player_flags = PlayerFlags::default();
        let first_use_bytes: &[u8] = row.get("FirstUseFlag");
        player_flags.tip_flags = Bitfield::from_bytes(first_use_bytes, SIZEOF_TIP_FLAGS)?;
        player_flags.tutorial_flag = row.get::<_, Int>("TutorialFlag") != 0;
        player_flags.name_check = (row.get::<_, Int>("NameCheck") as i8).try_into()?;
        player.flags = player_flags;

        let guide: PlayerGuide = (row.get::<_, Int>("Mentor") as i16).try_into()?;
        // TODO get total number of guides from DB (currently not stored)
        if guide != PlayerGuide::Computress {
            player.update_guide(guide);
        }

        let skyway_bytes: &[u8] = row.get("SkywayLocationFlag");
        player.flags.skyway_flags =
            Bitfield::from_bytes(skyway_bytes, WYVERN_LOCATION_FLAG_SIZE as usize)?;

        player
            .flags
            .scamper_flags
            .set_chunk(0, row.get("WarpLocationFlag"))
            .unwrap();

        let quest_bytes: &[u8] = row.get("Quests");
        player.mission_journal.completed_mission_flags =
            Bitfield::from_bytes(quest_bytes, SIZEOF_QUESTFLAG_NUMBER as usize)?;

        let running_quests = Self::query(client, "load_running_quests", &[&pc_uid]).await?;
        for quest in running_quests {
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

        let items = Self::query(client, "load_items", &[&pc_uid]).await?;
        for item in items {
            let slot_num = item.get::<_, Int>("Slot") as usize;
            let item_raw = v0104::sItemBase {
                iType: item.get::<_, Int>("Type") as i16,
                iID: item.get::<_, Int>("ID") as i16,
                iOpt: item.get::<_, Int>("Opt"),
                iTimeLimit: item.get::<_, Int>("TimeLimit"),
            };

            let item: Option<Item> = item_raw.try_into_proto()?;
            let (loc, local_slot) = util::slot_num_to_loc_and_slot_num(slot_num)?;
            if (loc != crate::enums::ItemLocation::Bank
                || config_get().shard.remove_expired_items_from_bank.get())
                && item.is_some_and(|item| item.has_expired(SystemTime::now()))
            {
                // item is expired; skip it
                continue;
            }

            player.set_item(loc, local_slot, item)?;
        }

        let quest_items = Self::query(client, "load_quest_items", &[&pc_uid]).await?;
        for quest_item in quest_items {
            let item_id: Int = quest_item.get("ID");
            let count: Int = quest_item.get("Opt");
            player.set_quest_item_count(item_id as i16, count as usize)?;
        }

        if load_buddies {
            log_if_failed(Self::load_buddies(client, &mut player).await);
            log_if_failed(Self::load_blocks(client, &mut player).await);
        }

        let perms: Int = row.get("AccountLevel");
        player.perms = perms as i16;

        Ok(player)
    }

    async fn load_buddy_entry(
        client: &impl GenericClient,
        buddy_uid: BigInt,
    ) -> FFResult<Option<BuddyListEntry>> {
        let rows = Self::query(client, "load_player_lite", &[&buddy_uid]).await?;
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
            gender: row.get::<_, Int>("Gender") as i8,
            face_style: row.get::<_, Int>("FaceStyle") as i8,
            hair_style: row.get::<_, Int>("HairStyle") as i8,
            hair_color: row.get::<_, Int>("HairColor") as i8,
            skin_color: row.get::<_, Int>("SkinColor") as i8,
            eye_color: row.get::<_, Int>("EyeColor") as i8,
            height: row.get::<_, Int>("Height") as i8,
            body: row.get::<_, Int>("Body") as i8,
        };
        Ok(Some(BuddyListEntry {
            pc_uid: row.get("PlayerId"),
            first_name: row.get("FirstName"),
            last_name: row.get("LastName"),
            style,
            name_check: (row.get::<_, Int>("NameCheck") as i8).try_into()?,
            free_chat: true,
            blocked: false,
        }))
    }

    async fn load_buddies(client: &impl GenericClient, player: &mut Player) -> FFResult<()> {
        let rows = Self::query(client, "load_buddy_ids", &[&player.get_uid()]).await?;
        for row in rows {
            let buddy_uid: BigInt = row.get("PlayerBId");
            match Self::load_buddy_entry(client, buddy_uid).await {
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

    async fn load_blocks(client: &impl GenericClient, player: &mut Player) -> FFResult<()> {
        let rows = Self::query(client, "load_blocked_ids", &[&player.get_uid()]).await?;
        for row in rows {
            let blocked_uid: BigInt = row.get("BlockedPlayerId");
            log_if_failed(player.block_player(blocked_uid));
        }
        Ok(())
    }

    async fn get_client(&self) -> FFResult<Object> {
        let client = self.pool.get().await?;
        Ok(client)
    }
}
#[async_trait]
impl DbImpl for PostgresDatabase {
    async fn get_db_version(&self) -> FFResult<Int> {
        let client = self.get_client().await?;
        let rows = Self::query(&client, "get_db_version", &[]).await?;
        if rows.is_empty() {
            return Err(FFError::build(
                db_error_severity(),
                "Meta table has no DatabaseVersion row".to_string(),
            ));
        }
        Ok(rows[0].get(0))
    }

    async fn get_protocol_version(&self) -> FFResult<Int> {
        let client = self.get_client().await?;
        let rows = Self::query(&client, "get_protocol_version", &[]).await?;
        if rows.is_empty() {
            return Err(FFError::build(
                db_error_severity(),
                "Meta table has no ProtocolVersion row".to_string(),
            ));
        }
        Ok(rows[0].get(0))
    }

    async fn init_player(&self, acc_id: BigInt, player: &Player) -> FFResult<()> {
        let mut client = self.get_client().await?;
        let updated = Self::exec(
            &mut client,
            "init_player",
            &[
                &player.get_uid(),
                &acc_id,
                &player.first_name,
                &player.last_name,
                &(player.get_style().iNameCheck as Int),
                &(player.get_slot_num() as Int),
                &player.get_position().x,
                &player.get_position().y,
                &player.get_position().z,
                &player.get_rotation(),
                &player.get_hp(),
                &player.flags.skyway_flags.to_bytes().as_slice(),
                &player.flags.tip_flags.to_bytes().as_slice(),
                &player
                    .mission_journal
                    .completed_mission_flags
                    .to_bytes()
                    .as_slice(),
                //
                &player.get_uid(),
            ],
        )
        .await?;
        assert_eq!(updated, 1 + 1);
        Ok(())
    }

    async fn update_player_appearance(&self, player: &Player) -> FFResult<()> {
        let mut client = self.get_client().await?;
        let style = player.style.unwrap_or_default();
        let apperance_flag: Int = if player.style.is_some() { 1 } else { 0 };
        let updated = Self::exec(
            &mut client,
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
                //
                &player.get_uid(),
                &apperance_flag,
            ],
        )
        .await?;
        assert_eq!(updated, 1 + 1);
        Ok(())
    }

    async fn find_account_from_username(&self, username: &str) -> FFResult<Option<Account>> {
        let mut client = self.get_client().await?;
        let rows = Self::query(&client, "find_account", &[&username]).await?;
        assert!(rows.len() <= 1);

        let row = match rows.first() {
            Some(r) => r,
            None => return Ok(None),
        };

        let cookie = match row.try_get::<_, String>("Cookie") {
            Ok(cookie_str) => {
                let expires_sec: Int = row.get("Expires");
                Some(Cookie {
                    token: cookie_str,
                    expires: util::get_systime_from_sec(expires_sec as u64),
                })
            }
            Err(_) => None,
        };

        let account = Account {
            id: row.get("AccountId"),
            username: username.to_string(),
            password_hashed: row.get("Password"),
            cookie,
            selected_slot: row.get::<_, Int>("Selected") as u8,
            account_level: row.get::<_, Int>("AccountLevel") as i16,
            banned_until: util::get_systime_from_sec(row.get::<_, Int>("BannedUntil") as u64),
            ban_reason: row.get("BanReason"),
        };

        log_if_failed(
            Self::exec(&mut client, "invalidate_cookie_for_account", &[&account.id]).await,
        );
        Ok(Some(account))
    }

    async fn find_account_from_player(&self, pc_uid: BigInt) -> FFResult<Option<Account>> {
        let client = self.get_client().await?;
        let rows = Self::query(&client, "find_account_from_player", &[&pc_uid]).await?;
        if rows.is_empty() {
            return Ok(None);
        }
        let row = &rows[0];
        Ok(Some(Account {
            id: row.get("AccountId"),
            username: row.get("Login"),
            password_hashed: row.get("Password"),
            cookie: None, // this query is not for auth
            selected_slot: row.get::<_, Int>("Selected") as u8,
            account_level: row.get::<_, Int>("AccountLevel") as i16,
            banned_until: util::get_systime_from_sec(row.get::<_, Int>("BannedUntil") as u64),
            ban_reason: row.get("BanReason"),
        }))
    }

    async fn create_account(&self, username: &str, password_hashed: &str) -> FFResult<Account> {
        {
            let mut client = self.get_client().await?;

            let acc_level = if Self::query(&client, "enum_account_ids", &[])
                .await
                .is_ok_and(|rows| rows.is_empty())
            {
                CN_ACCOUNT_LEVEL__MASTER
            } else {
                config_get().login.default_account_level.get()
            } as Int;

            let updated = Self::exec(
                &mut client,
                "create_account",
                &[&username, &password_hashed, &acc_level],
            )
            .await?;
            assert_eq!(updated, 1);
        }

        let new_acc = self.find_account_from_username(username).await?.unwrap();
        Ok(new_acc)
    }

    async fn change_account_level(&self, acc_id: BigInt, new_level: Int) -> FFResult<()> {
        let mut client = self.get_client().await?;
        let updated =
            Self::exec(&mut client, "change_account_level", &[&acc_id, &new_level]).await?;
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
        let mut client = self.get_client().await?;
        let banned_since = util::get_timestamp_sec(SystemTime::now()) as Int;
        let banned_until = util::get_timestamp_sec(banned_until) as Int;
        let updated = Self::exec(
            &mut client,
            "ban_account",
            &[&acc_id, &banned_since, &banned_until, &ban_reason],
        )
        .await?;
        if updated == 0 {
            log(
                Severity::Warning,
                &format!("Failed to ban account with ID {}", acc_id),
            );
        }
        Ok(())
    }

    async fn unban_account(&self, acc_id: BigInt) -> FFResult<()> {
        let mut client = self.get_client().await?;
        let updated = Self::exec(&mut client, "unban_account", &[&acc_id]).await?;
        if updated == 0 {
            log(
                Severity::Warning,
                &format!("Failed to unban account with ID {}", acc_id),
            );
        }
        Ok(())
    }

    async fn update_selected_player(&self, acc_id: BigInt, slot_num: Int) -> FFResult<()> {
        let mut client = self.get_client().await?;
        let timestamp_now = util::get_timestamp_sec(SystemTime::now()) as Int;
        let updated = Self::exec(
            &mut client,
            "update_selected",
            &[&acc_id, &slot_num, &timestamp_now],
        )
        .await?;
        assert_eq!(updated, 1);
        Ok(())
    }

    async fn load_player(&self, acc_id: BigInt, pc_uid: BigInt) -> FFResult<Option<Player>> {
        let client = self.get_client().await?;
        let rows = Self::query(&client, "load_players", &[&acc_id]).await?;
        for row in &rows {
            if row.get::<_, BigInt>("PlayerId") == pc_uid {
                return Self::load_player_internal(&client, row, true)
                    .await
                    .map(Some);
            }
        }
        Ok(None)
    }

    async fn load_players(&self, acc_id: BigInt) -> FFResult<Vec<Player>> {
        let client = self.get_client().await?;
        let chars = Self::query(&client, "load_players", &[&acc_id]).await?;
        let mut players = Vec::with_capacity(chars.len());
        for row in chars {
            match Self::load_player_internal(&client, &row, true).await {
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
    }

    async fn save_player(&self, player: &Player) -> FFResult<()> {
        let mut client = self.get_client().await?;
        Self::save_player_internal(&mut client, player).await
    }

    async fn save_players(&self, players: &[&Player]) -> FFResult<()> {
        let mut client = self.get_client().await?;
        let mut tsct = client.transaction().await?;
        for player in players {
            Self::save_player_internal(&mut tsct, player).await?;
        }
        tsct.commit().await?;
        Ok(())
    }

    async fn delete_player(&self, pc_uid: BigInt) -> FFResult<()> {
        let mut client = self.get_client().await?;
        let updated: u64 = Self::exec(&mut client, "delete_player", &[&pc_uid]).await?;
        assert_eq!(updated, 1);
        Ok(())
    }

    async fn is_code_redeemed(&self, pc_uid: BigInt, code: &str) -> FFResult<bool> {
        let client = self.get_client().await?;
        let rows = Self::query(&client, "is_code_redeemed", &[&pc_uid, &code]).await?;
        Ok(!rows.is_empty())
    }

    async fn record_code_redemption(&self, pc_uid: BigInt, code: &str) -> FFResult<()> {
        let mut client = self.get_client().await?;
        let updated = Self::exec(&mut client, "record_code_redemption", &[&pc_uid, &code]).await?;
        assert_eq!(updated, 1);
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::sync::LazyLock;

    use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};
    use tokio::sync::Mutex;

    fn list_sql_files() -> Vec<std::path::PathBuf> {
        std::fs::read_dir("sql")
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("sql"))
            .collect()
    }

    fn pg_conn_str() -> String {
        let config = crate::config::Config::load("config.toml").expect("config.toml");
        let g = &config.general;
        format!(
            "host={} port={} user={} password={} dbname={}",
            g.db_host.get(),
            g.db_port.get(),
            g.db_username.get(),
            g.db_password.get(),
            g.db_name.get(),
        )
    }

    #[test]
    fn test_all_sql_files_parse() {
        let dialect = PostgreSqlDialect {};
        let mut tried = 0;
        for path in list_sql_files() {
            let sql = std::fs::read_to_string(&path).unwrap();
            Parser::parse_sql(&dialect, &sql)
                .unwrap_or_else(|e| panic!("Failed to parse {}: {}", path.display(), e));
            tried += 1;
        }
        assert!(
            tried > 30,
            "expected to exercise >30 SQL files, got {}",
            tried
        );
    }

    /// Live-DB validation that every shared `sql/*.sql` file `PREPARE`s
    /// against a real Postgres server. Ignored by default; run with:
    ///
    /// ```sh
    /// cargo test --features postgres -- --ignored
    /// ```
    ///
    /// Connection details are read from `config.toml` (the same file
    /// used by the running server). The entire test runs inside a
    /// transaction that is rolled back at the end, so the target DB
    /// is left unmodified.
    #[tokio::test]
    #[ignore]
    async fn test_all_sql_files_prepare() {
        use crate::config::config_get;
        use crate::defines::DB_VERSION;
        use tokio_postgres::NoTls;

        let conn_str = pg_conn_str();

        let (mut client, connection) = tokio_postgres::connect(&conn_str, NoTls)
            .await
            .expect("failed to connect to test Postgres");

        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("connection error: {}", e);
            }
        });

        // Run everything inside a transaction so the live DB is unchanged on exit.
        let tx = client.transaction().await.unwrap();

        let reset_sql = std::fs::read_to_string("sql/reset_db.sql").unwrap();
        tx.batch_execute(&reset_sql).await.expect("reset_db failed");

        let protocol_version = config_get().general.protocol.get().version_number();

        let create_tables_sql = std::fs::read_to_string("sql/create_tables.sql").unwrap();
        let param_regex = regex::Regex::new(r"\$([0-9]+)").unwrap();
        let mut params: &[&(dyn tokio_postgres::types::ToSql + Sync)] =
            &[&protocol_version, &DB_VERSION];

        for stmt in create_tables_sql.split(';') {
            if stmt.trim().is_empty() {
                continue;
            }
            let num_params = param_regex
                .find_iter(stmt)
                .map(|m| m.as_str()[1..].parse::<usize>().unwrap())
                .max()
                .unwrap_or(0);
            tx.execute(stmt, &params[..num_params])
                .await
                .unwrap_or_else(|e| panic!("create_tables stmt failed: {}\nSQL:\n{}", e, stmt));
            params = &params[num_params..];
        }

        let skip = |name: &str| name == "create_tables.sql" || name == "reset_db.sql";

        let mut tried = 0;
        for path in list_sql_files() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if skip(&name) {
                continue;
            }
            let sql = std::fs::read_to_string(&path).unwrap();
            for stmt_sql in sql.split(';') {
                if stmt_sql.trim().is_empty() {
                    continue;
                }
                tx.prepare(stmt_sql).await.unwrap_or_else(|e| {
                    panic!("Failed to prepare sql/{}: {}\nSQL:\n{}", name, e, stmt_sql)
                });
            }
            tried += 1;
        }

        tx.rollback().await.unwrap();

        assert!(
            tried > 30,
            "expected to exercise >30 SQL files, got {}",
            tried
        );
    }

    // DB API functional tests against a live PG (ignored by default)

    async fn reset_pg() {
        use tokio_postgres::NoTls;
        let (client, connection) = tokio_postgres::connect(&pg_conn_str(), NoTls)
            .await
            .expect("pg connect");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let reset_sql = std::fs::read_to_string("sql/reset_db.sql").unwrap();
        client.batch_execute(&reset_sql).await.expect("reset_db");
    }

    async fn connect_pg_db() -> super::super::Database<super::PostgresDatabase> {
        let config = crate::config::Config::load("config.toml").expect("config.toml");
        let inner = super::PostgresDatabase::connect(&config.general)
            .await
            .expect("postgres connect");
        super::super::Database::new(inner)
    }

    // Tests run in parallel, but since we want each one to run on clean state,
    // we need to serailze them. Just lock on this mutex.
    static PG_TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    macro_rules! pg_suite_test {
        ($name:ident) => {
            #[tokio::test]
            #[ignore]
            async fn $name() {
                let _guard = PG_TEST_LOCK.lock().await;
                crate::database::test_suite::ensure_init();
                reset_pg().await;
                let db = connect_pg_db().await;
                crate::database::test_suite::$name(&db).await;
            }
        };
    }

    crate::for_each_db_test!(pg_suite_test);
}

use std::sync::Once;
use std::time::{Duration, SystemTime};

use crate::config::Config;
use crate::database::{Database, DbImpl};
use crate::defines::DB_VERSION;
use crate::entity::Player;

#[macro_export]
macro_rules! for_each_db_test {
    ($macro:ident) => {
        $macro!(test_meta);
        $macro!(test_account_crud);
        $macro!(test_player_init_load);
        $macro!(test_player_save_reload);
        $macro!(test_save_players_batch);
        $macro!(test_player_appearance);
        $macro!(test_player_selected);
        $macro!(test_player_delete);
        $macro!(test_code_redemption);
        $macro!(test_load_buddy_info);
    };
}

pub fn ensure_init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        crate::tabledata::tdata_init().expect("tdata_init failed");
    });
}

pub fn build_config(db_path: &str) -> Config {
    let toml = format!(
        "[general]\ndb_path = \"{}\"\n",
        db_path.replace('\\', "\\\\")
    );
    Config::from_str(&toml).expect("test config parse")
}

fn make_player(uid: i64, slot_num: usize) -> Player {
    let mut p = Player::new(uid, slot_num);
    p.first_name = format!("Test{}", uid);
    p.last_name = "Player".to_string();
    p
}

// Tests //

pub async fn test_meta<D: DbImpl>(db: &Database<D>) {
    let v = db.get_db_version().await.expect("get_db_version");
    assert_eq!(v, DB_VERSION, "db version should equal compiled DB_VERSION");
}

pub async fn test_account_crud<D: DbImpl>(db: &Database<D>) {
    // create + lookup
    let acc = db
        .create_account("cake", "$2b$10$hash")
        .await
        .expect("create_account");
    assert_eq!(acc.username, "cake");
    assert!(acc.id > 0);

    let found = db
        .find_account_from_username("cake")
        .await
        .expect("find_account_from_username")
        .expect("account exists");
    assert_eq!(found.id, acc.id);
    assert_eq!(found.password_hashed, "$2b$10$hash");

    // missing
    let missing = db
        .find_account_from_username("nobody")
        .await
        .expect("find_account_from_username (missing)");
    assert!(missing.is_none());

    // change level
    db.change_account_level(acc.id, 50)
        .await
        .expect("change_account_level");
    let after = db
        .find_account_from_username("cake")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.account_level, 50);

    // ban + unban
    let until = SystemTime::now() + Duration::from_secs(3600);
    db.ban_account(acc.id, until, "spamming")
        .await
        .expect("ban_account");
    let banned = db
        .find_account_from_username("cake")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(banned.ban_reason, "spamming");
    assert!(
        banned.banned_until > SystemTime::now(),
        "banned_until should be in the future"
    );

    db.unban_account(acc.id).await.expect("unban_account");
    let unbanned = db
        .find_account_from_username("cake")
        .await
        .unwrap()
        .unwrap();
    assert!(
        unbanned.banned_until <= SystemTime::now(),
        "banned_until should be cleared"
    );
}

pub async fn test_player_init_load<D: DbImpl>(db: &Database<D>) {
    let acc = db.create_account("dong", "h").await.unwrap();
    let uid: i64 = 1001;
    let player = make_player(uid, 1);

    db.init_player(acc.id, &player).await.expect("init_player");

    // load by UID
    let loaded = db
        .load_player(acc.id, uid)
        .await
        .expect("load_player")
        .expect("player exists");
    assert_eq!(loaded.get_uid(), uid);
    assert_eq!(loaded.get_slot_num(), 1);
    assert_eq!(loaded.first_name, "Test1001");

    // load all players for account
    let all = db.load_players(acc.id).await.expect("load_players");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].get_uid(), uid);

    // find_account_from_player
    let acc2 = db
        .find_account_from_player(uid)
        .await
        .expect("find_account_from_player")
        .expect("account exists");
    assert_eq!(acc2.id, acc.id);
}

pub async fn test_player_save_reload<D: DbImpl>(db: &Database<D>) {
    let acc = db.create_account("cpunch", "h").await.unwrap();
    let uid: i64 = 2002;
    let player = make_player(uid, 2);
    db.init_player(acc.id, &player).await.unwrap();

    // mutate then save
    let mut p = db.load_player(acc.id, uid).await.unwrap().unwrap();
    p.set_taros(12345);
    db.save_player(&p).await.expect("save_player");

    let reloaded = db.load_player(acc.id, uid).await.unwrap().unwrap();
    assert_eq!(
        reloaded.get_taros(),
        12345,
        "taros should persist across save/load"
    );
}

pub async fn test_save_players_batch<D: DbImpl>(db: &Database<D>) {
    let acc = db.create_account("sane", "h").await.unwrap();
    let mut players = Vec::new();
    for i in 0..3 {
        let p = make_player(3000 + i as i64, i);
        db.init_player(acc.id, &p).await.unwrap();
        players.push(p);
    }
    // load + mutate + batch-save
    let mut loaded: Vec<Player> = Vec::new();
    for p in &players {
        loaded.push(db.load_player(acc.id, p.get_uid()).await.unwrap().unwrap());
    }
    for (i, p) in loaded.iter_mut().enumerate() {
        p.set_taros(100 * (i as u32 + 1));
    }
    let refs: Vec<&Player> = loaded.iter().collect();
    db.save_players(&refs).await.expect("save_players");

    for (i, p) in players.iter().enumerate() {
        let r = db.load_player(acc.id, p.get_uid()).await.unwrap().unwrap();
        assert_eq!(r.get_taros(), 100 * (i as u32 + 1));
    }
}

pub async fn test_player_appearance<D: DbImpl>(db: &Database<D>) {
    use crate::entity::PlayerStyle;
    let acc = db.create_account("kevman", "h").await.unwrap();
    let uid: i64 = 4004;
    let p = make_player(uid, 0);
    db.init_player(acc.id, &p).await.unwrap();

    let mut p = db.load_player(acc.id, uid).await.unwrap().unwrap();
    p.style = Some(PlayerStyle {
        gender: 1,
        face_style: 2,
        hair_style: 3,
        hair_color: 4,
        skin_color: 5,
        eye_color: 6,
        height: 7,
        body: 8,
    });
    db.update_player_appearance(&p)
        .await
        .expect("update_player_appearance");

    let r = db.load_player(acc.id, uid).await.unwrap().unwrap();
    let s = r
        .style
        .expect("style should be set after appearance update");
    assert_eq!(s.gender, 1);
    assert_eq!(s.face_style, 2);
    assert_eq!(s.body, 8);
}

pub async fn test_player_selected<D: DbImpl>(db: &Database<D>) {
    let acc = db.create_account("finn", "h").await.unwrap();
    let uid: i64 = 5005;
    let p = make_player(uid, 3);
    db.init_player(acc.id, &p).await.unwrap();

    db.update_selected_player(acc.id, 3)
        .await
        .expect("update_selected_player");

    let after = db
        .find_account_from_username("finn")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.selected_slot, 3);
}

pub async fn test_player_delete<D: DbImpl>(db: &Database<D>) {
    let acc = db.create_account("jade", "h").await.unwrap();
    let uid: i64 = 6006;
    let p = make_player(uid, 0);
    db.init_player(acc.id, &p).await.unwrap();
    assert!(db.load_player(acc.id, uid).await.unwrap().is_some());

    db.delete_player(uid).await.expect("delete_player");

    let after = db.load_player(acc.id, uid).await.unwrap();
    assert!(after.is_none(), "player should be gone after delete");
}

pub async fn test_code_redemption<D: DbImpl>(db: &Database<D>) {
    let acc = db.create_account("radio", "h").await.unwrap();
    let uid: i64 = 7007;
    let other_uid: i64 = 7008;
    db.init_player(acc.id, &make_player(uid, 0)).await.unwrap();
    db.init_player(acc.id, &make_player(other_uid, 1))
        .await
        .unwrap();

    let code = "ffcshnitzelbooms";
    assert!(!db
        .is_code_redeemed(uid, code)
        .await
        .expect("is_code_redeemed"));

    db.record_code_redemption(uid, code)
        .await
        .expect("record_code_redemption");
    assert!(db.is_code_redeemed(uid, code).await.unwrap());

    // redemptions are per-player and per-code
    assert!(!db.is_code_redeemed(other_uid, code).await.unwrap());
    assert!(!db.is_code_redeemed(uid, "ffcnaturalskills").await.unwrap());

    // redemptions are removed along with the player
    db.delete_player(uid).await.unwrap();
    assert!(!db.is_code_redeemed(uid, code).await.unwrap());
}

pub async fn test_load_buddy_info<D: DbImpl>(db: &Database<D>) {
    use crate::entity::PlayerStyle;
    let acc = db.create_account("eddy", "h").await.unwrap();
    let buddy_uids: [i64; 2] = [8008, 8009];
    let missing_uid: i64 = 8999;

    for (slot, uid) in buddy_uids.iter().enumerate() {
        let mut p = make_player(*uid, slot);
        db.init_player(acc.id, &p).await.unwrap();
        p.style = Some(PlayerStyle {
            gender: 2,
            face_style: 1,
            hair_style: 1,
            hair_color: 1,
            skin_color: 1,
            eye_color: 1,
            height: 1,
            body: 1,
        });
        db.update_player_appearance(&p).await.unwrap();
    }

    // requested out of UID order to check that input order is preserved
    let requested = [buddy_uids[1], buddy_uids[0]];
    let buddies = db
        .load_buddy_info(&requested, false)
        .await
        .expect("load_buddy_info");
    let blocked = db
        .load_buddy_info(&requested, true)
        .await
        .expect("load_buddy_info (blocked)");

    for (infos, blocked_flag) in [(&buddies, 0), (&blocked, 1)] {
        let uids: Vec<i64> = infos.iter().map(|info| info.iPCUID).collect();
        assert_eq!(uids, requested, "results should be in input order");
        for info in infos {
            assert_eq!({ info.bBlocked }, blocked_flag);
            assert_eq!({ info.iGender }, 2);
            assert_eq!(
                { info.szFirstName },
                crate::util::encode_utf16(&format!("Test{}", { info.iPCUID })).unwrap()
            );
        }
    }

    // missing players are skipped rather than failing the whole load
    let partial = db
        .load_buddy_info(&[buddy_uids[0], missing_uid], false)
        .await
        .expect("load_buddy_info (partial)");
    let uids: Vec<i64> = partial.iter().map(|info| info.iPCUID).collect();
    assert_eq!(uids, vec![buddy_uids[0]]);

    let empty = db
        .load_buddy_info(&[], false)
        .await
        .expect("load_buddy_info (empty)");
    assert!(empty.is_empty());
}

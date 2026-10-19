CREATE TABLE Meta(
    Key TEXT NOT NULL UNIQUE,
    Value INTEGER NOT NULL
);

INSERT INTO Meta(
    Key,
    Value
)
VALUES (
    'ProtocolVersion',
    ?1
);

INSERT INTO Meta(
    Key,
    Value
)
VALUES (
    'DatabaseVersion',
    ?1
);

CREATE TABLE IF NOT EXISTS Accounts (
    AccountID           INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    Login               TEXT    NOT NULL,
    Password            TEXT    NOT NULL,
    Selected            INTEGER DEFAULT 1 NOT NULL,
    AccountLevel        INTEGER NOT NULL,
    Created             INTEGER DEFAULT (unixepoch()) NOT NULL,
    LastLogin           INTEGER DEFAULT (unixepoch()) NOT NULL,
    BannedUntil         INTEGER DEFAULT 0 NOT NULL,
    BannedSince         INTEGER DEFAULT 0 NOT NULL,
    BanReason           TEXT    DEFAULT '' NOT NULL,
    Email               TEXT    DEFAULT '' NOT NULL,
    LastPasswordReset   INTEGER DEFAULT 0 NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS Accounts_Login_Lower_Idx ON Accounts (LOWER(Login));

CREATE TABLE IF NOT EXISTS Players (
    PlayerID           INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    AccountID          INTEGER NOT NULL,
    FirstName          TEXT NOT NULL,
    LastName           TEXT NOT NULL,
    NameCheck          INTEGER NOT NULL,
    Slot               INTEGER NOT NULL,
    Created            INTEGER DEFAULT (unixepoch()) NOT NULL,
    LastLogin          INTEGER DEFAULT (unixepoch()) NOT NULL,
    Level              INTEGER DEFAULT 1 NOT NULL,
    Nano1              INTEGER DEFAULT 0 NOT NULL,
    Nano2              INTEGER DEFAULT 0 NOT NULL,
    Nano3              INTEGER DEFAULT 0 NOT NULL,
    AppearanceFlag     INTEGER DEFAULT 0 NOT NULL,
    TutorialFlag       INTEGER DEFAULT 0 NOT NULL,
    PayZoneFlag        INTEGER DEFAULT 0 NOT NULL,
    XCoordinate        INTEGER NOT NULL,
    YCoordinate        INTEGER NOT NULL,
    ZCoordinate        INTEGER NOT NULL,
    Angle              INTEGER NOT NULL,
    HP                 INTEGER NOT NULL,
    FusionMatter       INTEGER DEFAULT 0 NOT NULL,
    Taros              INTEGER DEFAULT 0 NOT NULL,
    BatteryW           INTEGER DEFAULT 0 NOT NULL,
    BatteryN           INTEGER DEFAULT 0 NOT NULL,
    Mentor             INTEGER DEFAULT 5 NOT NULL,
    CurrentMissionID   INTEGER DEFAULT 0 NOT NULL,
    WarpLocationFlag   INTEGER DEFAULT 0 NOT NULL,
    SkywayLocationFlag BLOB NOT NULL,
    FirstUseFlag       BLOB NOT NULL,
    Quests             BLOB NOT NULL,
    FOREIGN KEY(AccountID) REFERENCES Accounts(AccountID) ON DELETE CASCADE,
    UNIQUE (AccountID, Slot)
);

CREATE UNIQUE INDEX IF NOT EXISTS Players_Name_Lower_Idx ON Players (LOWER(FirstName), LOWER(LastName));

CREATE TABLE IF NOT EXISTS Auth (
    AccountID   INTEGER NOT NULL,
    Cookie      TEXT NOT NULL,
    Expires     INTEGER DEFAULT 0 NOT NULL,
    FOREIGN KEY(AccountID) REFERENCES Accounts(AccountID) ON DELETE CASCADE,
    UNIQUE (AccountID)
);

CREATE TABLE IF NOT EXISTS Appearances (
    PlayerID    INTEGER UNIQUE NOT NULL,
    Body        INTEGER DEFAULT 0 NOT NULL,
    EyeColor    INTEGER DEFAULT 1 NOT NULL,
    FaceStyle   INTEGER DEFAULT 1 NOT NULL,
    Gender      INTEGER DEFAULT 1 NOT NULL,
    HairColor   INTEGER DEFAULT 1 NOT NULL,
    HairStyle   INTEGER DEFAULT 1 NOT NULL,
    Height      INTEGER DEFAULT 0 NOT NULL,
    SkinColor   INTEGER DEFAULT 1 NOT NULL,
    FOREIGN KEY(PlayerID) REFERENCES Players(PlayerID) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS Inventory (
    PlayerID    INTEGER NOT NULL,
    Slot        INTEGER NOT NULL,
    ID          INTEGER NOT NULL,
    Type        INTEGER NOT NULL,
    Opt         INTEGER NOT NULL,
    TimeLimit   INTEGER DEFAULT 0 NOT NULL,
    FOREIGN KEY(PlayerID) REFERENCES Players(PlayerID) ON DELETE CASCADE,
    UNIQUE (PlayerID, Slot)
);

CREATE TABLE IF NOT EXISTS QuestItems (
    PlayerID    INTEGER NOT NULL,
    Slot        INTEGER NOT NULL,
    ID          INTEGER NOT NULL,
    Opt         INTEGER NOT NULL,
    FOREIGN KEY(PlayerID) REFERENCES Players(PlayerID) ON DELETE CASCADE,
    UNIQUE (PlayerID, Slot)
);

CREATE TABLE IF NOT EXISTS Nanos (
    PlayerID    INTEGER NOT NULL,
    ID          INTEGER NOT NULL,
    Skill       INTEGER NOT NULL,
    Stamina     INTEGER DEFAULT 150 NOT NULL,
    FOREIGN KEY(PlayerID) REFERENCES Players(PlayerID) ON DELETE CASCADE,
    UNIQUE (PlayerID, ID)
);

CREATE TABLE IF NOT EXISTS RunningQuests (
    PlayerID              INTEGER NOT NULL,
    TaskID                INTEGER NOT NULL,
    RemainingNPCCount1    INTEGER NOT NULL,
    RemainingNPCCount2    INTEGER NOT NULL,
    RemainingNPCCount3    INTEGER NOT NULL,
    FOREIGN KEY(PlayerID) REFERENCES Players(PlayerID) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS Buddyships (
    PlayerAID    INTEGER NOT NULL,
    PlayerBID    INTEGER NOT NULL,
    FOREIGN KEY(PlayerAID) REFERENCES Players(PlayerID) ON DELETE CASCADE,
    FOREIGN KEY(PlayerBID) REFERENCES Players(PlayerID) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS Blocks (
    PlayerID           INTEGER NOT NULL,
    BlockedPlayerID    INTEGER NOT NULL,
    FOREIGN KEY(PlayerID)        REFERENCES Players(PlayerID) ON DELETE CASCADE,
    FOREIGN KEY(BlockedPlayerID) REFERENCES Players(PlayerID) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS EmailData (
    PlayerID        INTEGER NOT NULL,
    MsgIndex        INTEGER NOT NULL,
    ReadFlag        INTEGER NOT NULL,
    ItemFlag        INTEGER NOT NULL,
    SenderID        INTEGER NOT NULL,
    SenderFirstName TEXT NOT NULL,
    SenderLastName  TEXT NOT NULL,
    SubjectLine     TEXT NOT NULL,
    MsgBody         TEXT NOT NULL,
    Taros           INTEGER NOT NULL,
    SendTime        INTEGER NOT NULL,
    DeleteTime      INTEGER NOT NULL,
    FOREIGN KEY(PlayerID) REFERENCES Players(PlayerID) ON DELETE CASCADE,
    UNIQUE(PlayerID, MsgIndex)
);

CREATE TABLE IF NOT EXISTS EmailItems (
    PlayerID    INTEGER NOT NULL,
    MsgIndex    INTEGER NOT NULL,
    Slot        INTEGER NOT NULL,
    ID          INTEGER NOT NULL,
    Type        INTEGER NOT NULL,
    Opt         INTEGER NOT NULL,
    TimeLimit   INTEGER NOT NULL,
    FOREIGN KEY(PlayerID) REFERENCES Players(PlayerID) ON DELETE CASCADE,
    UNIQUE (PlayerID, MsgIndex, Slot)
);

CREATE TABLE IF NOT EXISTS RaceResults(
    EPID      INTEGER NOT NULL,
    PlayerID  INTEGER NOT NULL,
    Score     INTEGER NOT NULL,
    RingCount INTEGER NOT NULL,
    Time      INTEGER NOT NULL,
    Timestamp INTEGER NOT NULL,
    FOREIGN KEY(PlayerID) REFERENCES Players(PlayerID) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS RedeemedCodes(
    PlayerID    INTEGER NOT NULL,
    Code        TEXT NOT NULL,
    FOREIGN KEY(PlayerID) REFERENCES Players(PlayerID) ON DELETE CASCADE,
    UNIQUE (PlayerID, Code)
);

CREATE TABLE IF NOT EXISTS RaceRankings (
    PlayerUID BIGINT NOT NULL, EPID INTEGER NOT NULL, Score INTEGER NOT NULL,
    Pods INTEGER NOT NULL, Seconds INTEGER NOT NULL, Timestamp BIGINT NOT NULL,
    FOREIGN KEY (PlayerUID) REFERENCES Players(PlayerID) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS RaceRankingsPlayerEP ON RaceRankings(PlayerUID, EPID, Score);

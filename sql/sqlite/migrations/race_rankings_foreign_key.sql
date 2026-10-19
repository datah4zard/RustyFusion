ALTER TABLE RaceRankings RENAME TO RaceRankingsWithoutFK;
CREATE TABLE RaceRankings (
    PlayerUID BIGINT NOT NULL, EPID INTEGER NOT NULL, Score INTEGER NOT NULL,
    Pods INTEGER NOT NULL, Seconds INTEGER NOT NULL, Timestamp BIGINT NOT NULL,
    FOREIGN KEY (PlayerUID) REFERENCES Players(PlayerID) ON DELETE CASCADE
);
INSERT INTO RaceRankings (PlayerUID, EPID, Score, Pods, Seconds, Timestamp)
    SELECT R.PlayerUID, R.EPID, R.Score, R.Pods, R.Seconds, R.Timestamp
    FROM RaceRankingsWithoutFK R INNER JOIN Players P ON P.PlayerID = R.PlayerUID;
DROP TABLE RaceRankingsWithoutFK;
CREATE INDEX RaceRankingsPlayerEP ON RaceRankings(PlayerUID, EPID, Score);

SELECT PlayerUID, EPID, Score, Pods, Seconds, Timestamp FROM RaceRankings WHERE PlayerUID=$1 AND EPID=$2 ORDER BY Score DESC LIMIT 1;

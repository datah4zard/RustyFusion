use crate::chunk::InstanceID;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
    time::Instant,
};

// Truncate the shared monotonic clock before recording each race endpoint.
static CLOCK: LazyLock<Instant> = LazyLock::new(Instant::now);

pub fn get_time_seconds() -> u64 {
    CLOCK.elapsed().as_secs()
}

#[derive(Debug, Clone, Deserialize)]
pub struct RaceInfo {
    #[serde(rename = "EPID")]
    pub ep_id: i32,
    #[serde(rename = "ScoreCap")]
    pub score_cap: i32,
    #[serde(rename = "TimeLimit")]
    pub time_limit: u64,
    #[serde(rename = "TotalPods")]
    pub total_pods: usize,
    #[serde(rename = "ScaleFactor")]
    pub scale_factor: f64,
    #[serde(rename = "PodFactor")]
    pub pod_factor: f64,
    #[serde(rename = "TimeFactor")]
    pub time_factor: f64,
    #[serde(rename = "RankScores")]
    pub rank_scores: Vec<i32>,
    #[serde(rename = "Rewards")]
    pub rewards: Vec<i16>,
}
impl RaceInfo {
    pub fn has_timed_out(&self, seconds: u64) -> bool {
        // Allow one extra whole-clock second for client/server timing differences.
        seconds > self.time_limit.saturating_add(1)
    }

    pub fn rank(&self, score: i32) -> usize {
        self.rank_scores
            .iter()
            .position(|cutoff| score >= *cutoff)
            .unwrap_or(self.rank_scores.len() - 1)
    }
    pub fn score(&self, pods: usize, seconds: u64, capped: bool) -> i32 {
        let exponent = self.pod_factor * pods as f64 / self.total_pods as f64
            - self.time_factor * seconds as f64 / self.time_limit as f64
            + self.scale_factor;
        let score = exponent.exp() as i32;
        if capped {
            score.min(self.score_cap)
        } else {
            score
        }
    }
    pub fn fusion_matter(&self, pods: usize) -> u32 {
        ((1.0 + (self.scale_factor - 1.0).exp() * self.pod_factor * pods as f64)
            / self.total_pods as f64) as u32
    }
}
#[derive(Debug, Clone)]
pub struct RaceState {
    pub instance: InstanceID,
    pub mode: i32,
    pub started: u64,
    pub id: uuid::Uuid,
    pub finishing: bool,
    pub pods: HashSet<i32>,
}
#[derive(Debug, Clone, Default)]
pub struct RaceRanking {
    pub player_uid: i64,
    pub ep_id: i32,
    pub score: i32,
    pub pods: i32,
    pub seconds: i32,
    pub timestamp: i64,
}

pub fn load_races(root: &Map<String, Value>) -> Result<HashMap<i32, RaceInfo>, String> {
    // Initialize the shared clock during table loading, before races can start.
    LazyLock::force(&CLOCK);
    let mut races = HashMap::new();
    let Some(Value::Object(entries)) = root.get("Racing") else {
        return Ok(races);
    };
    for entry in entries.values() {
        let race: RaceInfo =
            serde_json::from_value(entry.clone()).map_err(|e| format!("Invalid race: {}", e))?;
        if race.total_pods == 0
            || race.time_limit == 0
            || race.score_cap < 0
            || race.rank_scores.len() != 5
            || race.rewards.len() != 5
            || race.rank_scores.windows(2).any(|w| w[0] < w[1])
            || ![race.scale_factor, race.pod_factor, race.time_factor]
                .iter()
                .all(|v| v.is_finite())
        {
            return Err(format!("Invalid racing parameters for EP {}", race.ep_id));
        }
        races.insert(race.ep_id, race);
    }
    Ok(races)
}

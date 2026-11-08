//! Daily totals, persisted as JSON. Also turns the pad's per-session counters
//! (which reset whenever the belt stops) into monotonic daily deltas.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::protocol::{Record, Status};

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Day {
    pub steps: u64,
    pub distance_m: u64,
    pub active_s: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    /// Keyed by local date, YYYY-MM-DD.
    pub days: BTreeMap<String, Day>,
    #[serde(skip)]
    path: Option<PathBuf>,
    #[serde(skip)]
    dirty: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DaySummary {
    pub date: String,
    pub steps: u64,
    pub distance_m: u64,
    pub active_s: u64,
}

impl Store {
    pub fn load(path: PathBuf) -> Store {
        let mut s: Store = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        s.path = Some(path);
        s
    }

    pub fn save_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        if let Some(p) = &self.path {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let tmp = p.with_extension("json.tmp");
            if let Ok(b) = serde_json::to_vec_pretty(self) {
                if std::fs::write(&tmp, b).is_ok() && std::fs::rename(&tmp, p).is_ok() {
                    self.dirty = false;
                }
            }
        }
    }

    pub fn add(&mut self, date: NaiveDate, delta: Day) {
        if delta == Day::default() {
            return;
        }
        let d = self.days.entry(date.format("%Y-%m-%d").to_string()).or_default();
        d.steps += delta.steps;
        d.distance_m += delta.distance_m;
        d.active_s += delta.active_s;
        self.dirty = true;
    }

    pub fn day(&self, date: NaiveDate) -> Day {
        self.days.get(&date.format("%Y-%m-%d").to_string()).copied().unwrap_or_default()
    }

    /// The last `n` days ending today, oldest first, including empty days.
    pub fn recent(&self, today: NaiveDate, n: u32) -> Vec<DaySummary> {
        (0..n)
            .rev()
            .map(|i| {
                let date = today - chrono::Days::new(i as u64);
                let d = self.day(date);
                DaySummary {
                    date: date.format("%Y-%m-%d").to_string(),
                    steps: d.steps,
                    distance_m: d.distance_m,
                    active_s: d.active_s,
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counters {
    time_s: u32,
    dist_10m: u32,
    steps: u32,
}

/// Converts pad session counters into deltas.
#[derive(Debug, Default)]
pub struct SessionTracker {
    last: Option<Counters>,
    /// Counters as last seen before the pad reset them; reconciled against
    /// the pad's stored "last record" to catch steps between the final poll and the reset.
    awaiting_record: Option<Counters>,
    /// Steps credited to keep-alive blips rather than walking.
    pub phantom_steps: u64,
}

impl SessionTracker {
    /// Feed a status frame. `phantom` = this delta was caused by a keep-alive blip.
    /// Returns the delta to add, and whether the session just ended (caller should ask for the last record).
    pub fn on_status(&mut self, s: &Status, phantom: bool) -> (Day, bool) {
        let cur = Counters { time_s: s.time_s, dist_10m: s.dist_10m, steps: s.steps };
        let Some(prev) = self.last.replace(cur) else {
            // First frame after connect: baseline only. Counting what is already on the
            // counters would double-count after an app restart mid-session.
            return (Day::default(), false);
        };
        if cur.steps < prev.steps || cur.dist_10m < prev.dist_10m || cur.time_s < prev.time_s {
            // Counters reset: session over. Anything after the reset (normally 0) is new.
            self.awaiting_record = Some(prev);
            return (to_day(cur, Counters::default(), false), true);
        }
        let mut d = to_day(cur, prev, false);
        if phantom {
            self.phantom_steps += d.steps;
            d.steps = 0;
        }
        (d, false)
    }

    pub fn on_record(&mut self, r: &Record) -> Day {
        let Some(prev) = self.awaiting_record.take() else { return Day::default() };
        let rec = Counters { time_s: r.time_s, dist_10m: r.dist_10m, steps: r.steps };
        // Only trust a record that plausibly belongs to the session we were tracking.
        if rec.steps < prev.steps || rec.time_s < prev.time_s || rec.dist_10m < prev.dist_10m {
            return Day::default();
        }
        to_day(rec, prev, false)
    }

    pub fn reset_connection(&mut self) {
        self.last = None;
        self.awaiting_record = None;
    }
}

fn to_day(cur: Counters, prev: Counters, _phantom: bool) -> Day {
    Day {
        steps: cur.steps.saturating_sub(prev.steps) as u64,
        distance_m: cur.dist_10m.saturating_sub(prev.dist_10m) as u64 * 10,
        active_s: cur.time_s.saturating_sub(prev.time_s) as u64,
    }
}

pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Belt, Mode};

    fn st(t: u32, d: u32, s: u32) -> Status {
        Status { belt: Belt::Running, speed: 20, mode: Some(Mode::Manual), time_s: t, dist_10m: d, steps: s }
    }

    #[test]
    fn accumulates_deltas_and_handles_reset() {
        let mut tr = SessionTracker::default();
        let mut total = Day::default();
        let mut add = |d: Day| {
            total.steps += d.steps;
            total.distance_m += d.distance_m;
            total.active_s += d.active_s;
        };
        add(tr.on_status(&st(0, 0, 0), false).0);
        add(tr.on_status(&st(10, 1, 15), false).0);
        add(tr.on_status(&st(20, 2, 30), true).0); // blip: steps are phantom
        add(tr.on_status(&st(30, 3, 45), false).0);
        let (d, ended) = tr.on_status(&st(0, 0, 0), false);
        assert!(ended);
        add(d);
        // pad stored 2 more steps after our last poll
        add(tr.on_record(&Record { time_s: 31, dist_10m: 3, steps: 47 }));
        assert_eq!(total, Day { steps: 32, distance_m: 30, active_s: 31 });
        assert_eq!(tr.phantom_steps, 15);
    }

    #[test]
    fn ignores_unrelated_record() {
        let mut tr = SessionTracker::default();
        tr.on_status(&st(0, 0, 0), false);
        tr.on_status(&st(100, 10, 200), false);
        tr.on_status(&st(0, 0, 0), false);
        assert_eq!(tr.on_record(&Record { time_s: 5, dist_10m: 0, steps: 3 }), Day::default());
    }

    #[test]
    fn recent_fills_empty_days() {
        let mut s = Store::default();
        let d = NaiveDate::from_ymd_opt(2026, 11, 7).unwrap();
        s.add(d, Day { steps: 100, distance_m: 70, active_s: 60 });
        let r = s.recent(d, 7);
        assert_eq!(r.len(), 7);
        assert_eq!(r[6].steps, 100);
        assert_eq!(r[0].date, "2026-11-01");
    }
}

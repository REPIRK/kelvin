//! Fan steering that the EC does not offer itself.
//!
//! **Auto+**: the fans run N% faster than the EC's Auto would at the same temperature. The Auto curve is not
//! readable, so it is learned: while the fans are in Auto, each telemetry tick records each fan's RPM against
//! the hotter of CPU/GPU, one table per power mode (Quiet's Auto is not Turbo's). In Auto+ the fans run in the
//! EC's custom mode and the duty is steered until the measured RPM is the learned RPM × (1 + N%). Steering by
//! RPM means no fan's duty-to-RPM curve has to be known. Where nothing is learned yet, the duty comes from the
//! fan curve plus N points.
//!
//! **Smoothing**: the temperature that drives Auto+ and the curve rises at once and falls slowly, so a short
//! spike does not make the fans surge and drop.
//!
//! **Guard**: in any mode this app steers, 95 °C held for 10 s sends the fans to Max until 85 °C.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::settings::data_dir;

const MIN_TEMP: i32 = 25;
const MAX_TEMP: i32 = 105;
const BUCKETS: usize = (MAX_TEMP - MIN_TEMP + 1) as usize;
const LEARN_RATE: f32 = 0.15;
/// Ticks after entering Auto before learning: the fans are still coming down from a boost.
const SETTLE_TICKS: u32 = 10;
/// Below this an Auto fan counts as stopped, and Auto+ stops it too.
const STOPPED_RPM: f32 = 300.0;
/// °C the smoothed temperature may fall per 2 s tick.
const FALL_PER_TICK: f32 = 0.5;

type Table = [Vec<Option<f32>>; 2];

#[derive(Serialize, Deserialize, Default)]
struct Stored {
    /// Power mode → [cpu, gpu] RPM per °C from 25 to 105.
    tables: HashMap<String, [Vec<Option<u32>>; 2]>,
}

pub struct Fans {
    tables: HashMap<u8, Table>,
    dirty: bool,
    settle: u32,
    duty: [f32; 2],
    rpm_per_duty: [f32; 2],
    smoothed: Option<f32>,
    hot_ticks: u32,
    cool_ticks: u32,
    pub guard: bool,
    /// Ticks the duty sat at a limit while the RPM stayed on the wrong side of the target.
    stuck: [u32; 2],
    /// The firmware takes duty writes but the fans do not follow them: Auto+ hands over to Auto.
    pub ignored: bool,
}

impl Fans {
    pub fn load() -> Self {
        let mut tables = HashMap::new();
        if let Some(stored) = std::fs::read_to_string(data_dir().join("fan_learning.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Stored>(&s).ok())
        {
            for (k, [a, b]) in stored.tables {
                if let Ok(mode) = k.parse::<u8>() {
                    let conv = |v: Vec<Option<u32>>| {
                        let mut t: Vec<Option<f32>> =
                            v.into_iter().map(|x| x.map(|x| x as f32)).collect();
                        t.resize(BUCKETS, None);
                        t
                    };
                    tables.insert(mode, [conv(a), conv(b)]);
                }
            }
        }
        Self {
            tables,
            dirty: false,
            settle: SETTLE_TICKS,
            duty: [-1.0; 2],
            rpm_per_duty: [55.0; 2],
            smoothed: None,
            hot_ticks: 0,
            cool_ticks: 0,
            guard: false,
            stuck: [0; 2],
            ignored: false,
        }
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let stored = Stored {
            tables: self
                .tables
                .iter()
                .map(|(m, [a, b])| {
                    let conv =
                        |t: &Vec<Option<f32>>| t.iter().map(|x| x.map(|x| x as u32)).collect();
                    (m.to_string(), [conv(a), conv(b)])
                })
                .collect(),
        };
        if let Ok(json) = serde_json::to_string(&stored)
            && std::fs::write(data_dir().join("fan_learning.json"), json).is_ok()
        {
            self.dirty = false;
        }
    }

    /// Feeds one telemetry tick into the smoothed temperature and the guard. Returns the smoothed value.
    pub fn observe(&mut self, temp: u32, steered: bool) -> f32 {
        let t = temp as f32;
        let s = match self.smoothed {
            Some(prev) if t < prev => (prev - FALL_PER_TICK).max(t),
            _ => t,
        };
        self.smoothed = Some(s);

        if steered && temp >= 95 {
            self.hot_ticks += 1;
            self.cool_ticks = 0;
            if self.hot_ticks >= 5 {
                self.guard = true;
            }
        } else if self.guard && temp <= 85 {
            self.cool_ticks += 1;
            self.hot_ticks = 0;
            if self.cool_ticks >= 5 {
                self.guard = false;
                self.reset_duty();
            }
        } else {
            self.hot_ticks = 0;
        }
        if !steered {
            self.guard = false;
        }
        s
    }

    pub fn enter_auto(&mut self) {
        self.settle = SETTLE_TICKS;
    }

    pub fn reset_duty(&mut self) {
        self.duty = [-1.0; 2];
    }

    fn table(&mut self, power: u8) -> &mut Table {
        self.tables
            .entry(power)
            .or_insert_with(|| [vec![None; BUCKETS], vec![None; BUCKETS]])
    }

    pub fn learn(&mut self, power: u8, temp: u32, rpm: [u32; 2]) {
        if temp == 0 {
            return;
        }
        if self.settle > 0 {
            self.settle -= 1;
            return;
        }
        let i = bucket(temp as f32);
        let t = self.table(power);
        for f in 0..2 {
            let r = rpm[f] as f32;
            t[f][i] = Some(match t[f][i] {
                Some(v) => v + (r - v) * LEARN_RATE,
                None => r,
            });
        }
        self.dirty = true;
    }

    /// True when Auto+ has learned data near this temperature for this power mode.
    pub fn knows(&mut self, power: u8, temp: f32) -> bool {
        lookup(&self.table(power)[0], temp).is_some()
    }

    /// Auto+ duty (0-100) for CPU and GPU fans this tick.
    pub fn boost_step(
        &mut self,
        power: u8,
        temp: f32,
        rpm: [u32; 2],
        boost: u8,
        fallback: [u8; 2],
    ) -> [u8; 2] {
        let autos = {
            let t = self.table(power);
            [lookup(&t[0], temp), lookup(&t[1], temp)]
        };
        let mut out = [0u8; 2];
        for f in 0..2 {
            out[f] = self.steer(f, autos[f], rpm[f] as f32, boost, fallback[f]);
        }
        out
    }

    fn steer(&mut self, f: usize, auto: Option<f32>, rpm: f32, boost: u8, fallback: u8) -> u8 {
        let Some(auto) = auto else {
            let open = (fallback as f32 + boost as f32).min(100.0);
            self.duty[f] = open;
            return open as u8;
        };
        if auto < STOPPED_RPM {
            self.duty[f] = 0.0;
            return 0;
        }
        let target = auto * (1.0 + boost as f32 / 100.0);
        if self.duty[f] <= 0.0 {
            self.duty[f] = (target / self.rpm_per_duty[f]).clamp(10.0, 100.0);
        } else {
            let error = target - rpm;
            // About 1% duty per 150 RPM of error per tick, limited so a bad reading cannot swing the fan.
            self.duty[f] = (self.duty[f] + (error / 150.0).clamp(-3.0, 3.0)).clamp(10.0, 100.0);
            // At a limit and still far off for 40 s: the fan is not following the duty.
            let at_floor = self.duty[f] <= 10.0 && error < -400.0;
            let at_top = self.duty[f] >= 100.0 && error > 400.0;
            self.stuck[f] = if at_floor || at_top {
                self.stuck[f] + 1
            } else {
                0
            };
            if self.stuck[f] >= 20 && !self.ignored {
                self.ignored = true;
                crate::diag::log(format!(
                    "Fan {f} does not follow the duty: {rpm:.0} rpm at {:.0}%, target {target:.0}",
                    self.duty[f]
                ));
            }
            if rpm > STOPPED_RPM && error.abs() < 200.0 {
                self.rpm_per_duty[f] += (rpm / self.duty[f] - self.rpm_per_duty[f]) * 0.1;
            }
        }
        self.duty[f].round() as u8
    }
}

fn bucket(temp: f32) -> usize {
    (temp.round() as i32).clamp(MIN_TEMP, MAX_TEMP) as usize - MIN_TEMP as usize
}

/// The bucket's value, else a line between the nearest known buckets on each side, else the nearest one
/// within 8 °C.
fn lookup(t: &[Option<f32>], temp: f32) -> Option<f32> {
    let i = bucket(temp);
    if let Some(v) = t[i] {
        return Some(v);
    }
    let lo = (0..i).rev().find(|&j| t[j].is_some());
    let hi = (i + 1..t.len()).find(|&j| t[j].is_some());
    match (lo, hi) {
        (Some(l), Some(h)) => {
            let (a, b) = (t[l].unwrap(), t[h].unwrap());
            Some(a + (i - l) as f32 / (h - l) as f32 * (b - a))
        }
        (Some(l), None) if i - l <= 8 => t[l],
        (None, Some(h)) if h - i <= 8 => t[h],
        _ => None,
    }
}

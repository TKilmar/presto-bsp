//! The evening plan: how cold will the room get tonight, and what should baby wear?
//!
//! Every house cools differently overnight, so the best predictor is the room itself:
//!
//! * **From last night** (needs ≥ 60 % of last night recorded): tonight follows last night's
//!   curve, shifted by how much warmer or cooler the room is now than at the same time
//!   yesterday ("current temperature offset"). With a forecast, the shift grows during the night
//!   by 15 % of the difference between tonight's and last night's outdoor low, so a colder night
//!   outside deepens the predicted drop.
//! * **Estimate** (no usable history): the room cools by 1.5 °C to its low at 05:00, plus 0.05 °C
//!   for every degree the forecast outdoor low is more than 8 °C below the room.
//!
//! The morning summary shows what really happened, so the plan can be checked.

use crate::clock::{Minutes, tonight};
use crate::forecast::Forecast;
use crate::history::History;
use crate::sleepwear::{Recommendation, recommend_for};

/// Drop used when there is no history.
pub const DEFAULT_DROP: f32 = 1.5;
/// How much of the change in outdoor low reaches the room by the coldest hour.
pub const OUTDOOR_COUPLING: f32 = 0.15;
/// Fraction of last night that must be recorded to use it as the model.
pub const MIN_COVERAGE: f32 = 0.6;
/// The coldest time of a typical night.
const COLDEST: Minutes = Minutes::hm(5, 0);
const DAY: u32 = 24 * 60;

/// Where the prediction came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Basis {
    /// Last night's curve shifted by `offset` °C (room now minus the same time yesterday).
    LastNight {
        /// Current temperature offset.
        offset: f32,
    },
    /// The default-drop heuristic.
    Estimate,
}

/// Tonight's plan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    /// Current room temperature.
    pub room_now: f32,
    /// Expected room minimum for the rest of the night.
    pub expected_low: f32,
    /// When the minimum is expected.
    pub expected_low_at: Minutes,
    /// Forecast outdoor minimum tonight, if known.
    pub outdoor_low: Option<f32>,
    /// Tonight's forecast outdoor low minus last night's, if both are known.
    pub outdoor_change: Option<f32>,
    /// Recommendation for the expected minimum.
    pub recommendation: Recommendation,
    /// Expected (or, for past hours, measured) room temperature for each hour 19:00–07:00.
    pub hourly: [f32; 12],
    /// How the prediction was made.
    pub basis: Basis,
}

/// Fill gaps in a curve by linear interpolation (and nearest value at the ends).
fn fill_gaps(slots: &[Option<f32>; 12]) -> Option<[f32; 12]> {
    let known: usize = slots.iter().filter(|s| s.is_some()).count();
    if known < 6 {
        return None;
    }
    let mut out = [0.0; 12];
    for i in 0..12 {
        out[i] = match slots[i] {
            Some(v) => v,
            None => {
                let before = (0..i).rev().find_map(|j| slots[j].map(|v| (j, v)));
                let after = (i + 1..12).find_map(|j| slots[j].map(|v| (j, v)));
                match (before, after) {
                    (Some((a, va)), Some((b, vb))) => va + (vb - va) * (i - a) as f32 / (b - a) as f32,
                    (Some((_, v)), None) | (None, Some((_, v))) => v,
                    (None, None) => return None,
                }
            }
        };
    }
    Some(out)
}

/// Plan tonight from the current room temperature. `sensitivity` is the baby's thermal
/// preference (see [`crate::sleepwear::recommend_for`]).
pub fn plan(now: Minutes, room_now: f32, history: &History, forecast: &Forecast, sensitivity: i8) -> Plan {
    let (start, end) = tonight(now);
    let coldest = Minutes(start.0 + (COLDEST.0 + DAY - start.of_day()) % DAY);
    let slot_time = |i: usize| Minutes(start.0 + 60 * i as u32 + 30);
    let outdoor_low = forecast.low_between(start, end);

    // Last night, as recorded.
    let last = start
        .0
        .checked_sub(DAY)
        .and_then(|s| history.summarize(Minutes(s), Minutes(s + 12 * 60)))
        .filter(|s| s.coverage() >= MIN_COVERAGE);
    let last_curve = last.as_ref().and_then(|s| fill_gaps(&s.hourly.map(|slot| slot.celsius)));
    let outdoor_change = match (outdoor_low, last.and_then(|s| s.outdoor.min)) {
        (Some(tonight_low), Some((last_low, _))) => Some(tonight_low - last_low),
        _ => None,
    };
    // What has been measured so far tonight.
    let tonight_so_far = history.summarize(start, end);

    let mut hourly = [0.0; 12];
    // The estimate defines its low exactly; the last-night model takes the minimum of its curve.
    let mut estimated_low = None;
    let basis = match last_curve {
        Some(curve) => {
            // Same time yesterday: from the history, or (before the night starts, or if that
            // part is missing) from last night's curve.
            let yesterday = Minutes(now.0.saturating_sub(DAY));
            let reference = history.celsius_near(yesterday).unwrap_or_else(|| {
                if now <= start {
                    curve[0]
                } else {
                    curve[(((now.0 - start.0) / 60) as usize).min(11)]
                }
            });
            let offset = room_now - reference;
            for (i, t) in hourly.iter_mut().enumerate() {
                let at = slot_time(i);
                // Outdoor effect builds up until the coldest hour.
                let ramp = if at <= now {
                    0.0
                } else {
                    ((at.0 - now.0.max(start.0)) as f32 / (coldest.0.saturating_sub(now.0.max(start.0))).max(60) as f32)
                        .min(1.0)
                };
                *t = curve[i] + offset + OUTDOOR_COUPLING * outdoor_change.unwrap_or(0.0) * ramp;
            }
            Basis::LastNight { offset }
        }
        None => {
            let mut drop = DEFAULT_DROP;
            if let Some(outside) = outdoor_low {
                drop += 0.05 * (room_now - outside - 8.0).max(0.0);
            }
            // Once the night is under way, only the remaining part of the drop is still to come.
            let progress =
                if now > start { ((now.0 - start.0) as f32 / (coldest.0 - start.0) as f32).min(1.0) } else { 0.0 };
            let remaining = drop * (1.0 - progress);
            let low = room_now - remaining;
            estimated_low = Some((low, if now > coldest { now } else { coldest }));
            for (i, t) in hourly.iter_mut().enumerate() {
                let at = slot_time(i).0;
                *t = if at <= now.0 {
                    room_now
                } else if at <= coldest.0 {
                    let f = (at - now.0.max(start.0)) as f32 / (coldest.0 - now.0.max(start.0)).max(1) as f32;
                    room_now - remaining * f
                } else {
                    // Slight recovery after the coldest hour.
                    low + 0.15 * (at - coldest.0) as f32 / 60.0
                };
            }
            Basis::Estimate
        }
    };

    // Hours already past show what was measured.
    if let Some(so_far) = tonight_so_far {
        for (i, t) in hourly.iter_mut().enumerate() {
            if slot_time(i) < now {
                if let Some(measured) = so_far.hourly[i].celsius {
                    *t = measured;
                }
            }
        }
    }

    // The low is over the rest of the night (including now).
    let (expected_low, expected_low_at) = estimated_low.unwrap_or_else(|| {
        let mut low = (room_now, now.max(start));
        for (i, &t) in hourly.iter().enumerate() {
            let at = slot_time(i);
            if at >= now && t < low.0 {
                low = (t, Minutes(at.0 - 30));
            }
        }
        low
    });

    Plan {
        room_now,
        expected_low,
        expected_low_at,
        outdoor_low,
        outdoor_change,
        recommendation: recommend_for(expected_low, sensitivity),
        hourly,
        basis,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forecast::Hour;
    use crate::history::Reading;

    fn reading(t: f32) -> Reading {
        Reading { celsius: Some(t), ..Default::default() }
    }

    /// Last night: 22 °C at 19:00, cooling 3 °C by 05:00 (outdoor low `outdoor`).
    fn last_night(outdoor: Option<f32>) -> History {
        let mut h = History::default();
        for m in (19 * 60..DAY + 7 * 60 + 10).step_by(10) {
            let t = 22.0 - 3.0 * ((m - 19 * 60) as f32 / (10.0 * 60.0)).min(1.0);
            h.record(Minutes(m), Reading { outdoor, ..reading(t) });
        }
        h
    }

    #[test]
    fn default_drop() {
        let p = plan(Minutes::hm(19, 0), 21.0, &History::default(), &Forecast::default(), 0);
        assert!((p.expected_low - 19.5).abs() < 0.01);
        assert_eq!(p.recommendation.bag_tog, 2.5);
        assert_eq!(p.basis, Basis::Estimate);
        assert!(p.hourly.windows(2).take(9).all(|w| w[1] <= w[0]));
    }

    #[test]
    fn cold_forecast_deepens_drop() {
        let forecast = Forecast::from_hours((0..24).map(|h| Hour {
            start: Minutes(19 * 60 + 60 * h),
            celsius: 2.0,
            rain_chance: 0,
        }));
        let p = plan(Minutes::hm(19, 0), 21.0, &History::default(), &forecast, 0);
        assert_eq!(p.outdoor_low, Some(2.0));
        assert!((p.expected_low - (21.0 - 1.5 - 0.55)).abs() < 0.01);
    }

    #[test]
    fn follows_last_night_shifted_by_offset() {
        let h = last_night(None);
        // 19:00 the next day, the room is 1 °C warmer than yesterday at 19:00.
        let p = plan(Minutes(DAY + 19 * 60), 23.0, &h, &Forecast::default(), 0);
        let Basis::LastNight { offset } = p.basis else { panic!("{:?}", p.basis) };
        assert!((offset - 1.0).abs() < 0.1, "{offset}");
        assert!((p.expected_low - 20.0).abs() < 0.15, "{}", p.expected_low);
        assert_eq!(p.expected_low_at.hour(), 5);
    }

    #[test]
    fn colder_night_outside_lowers_prediction() {
        let h = last_night(Some(8.0));
        let forecast = Forecast::from_hours((0..36).map(|i| Hour {
            start: Minutes(DAY + 18 * 60 + 60 * i),
            celsius: 0.0,
            rain_chance: 0,
        }));
        let p = plan(Minutes(DAY + 19 * 60), 22.0, &h, &forecast, 0);
        assert_eq!(p.outdoor_change, Some(-8.0));
        // 19 °C from last night's curve, minus 15 % of 8 °C.
        assert!((p.expected_low - (19.0 - 1.2)).abs() < 0.15, "{}", p.expected_low);
    }

    #[test]
    fn sensitivity_changes_recommendation_not_prediction() {
        let warm = plan(Minutes::hm(19, 0), 21.5, &History::default(), &Forecast::default(), 2);
        let cool = plan(Minutes::hm(19, 0), 21.5, &History::default(), &Forecast::default(), -2);
        assert_eq!(warm.expected_low, cool.expected_low);
        assert!(warm.recommendation.total_tog() < cool.recommendation.total_tog());
    }

    #[test]
    fn gaps_are_interpolated() {
        let mut slots = [Some(1.0); 12];
        slots[3] = None;
        slots[0] = None;
        let filled = fill_gaps(&slots).unwrap();
        assert_eq!(filled[3], 1.0);
        assert_eq!(filled[0], 1.0);
        assert!(fill_gaps(&[None; 12]).is_none());
    }
}

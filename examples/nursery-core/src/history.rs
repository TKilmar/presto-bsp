//! A 30-hour log of readings in 10-minute buckets, and the overnight summary.

use crate::air::AirQuality;
use crate::clock::Minutes;

/// Minutes per log bucket.
pub const BUCKET_MINUTES: u32 = 10;
/// Buckets kept (30 hours: enough to look up "this time yesterday" in the evening).
pub const CAPACITY: usize = 30 * 60 / BUCKET_MINUTES as usize;

/// One sensor reading. Each field is `None` when that sensor is missing or failed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reading {
    /// Room temperature, °C.
    pub celsius: Option<f32>,
    /// Relative humidity, %.
    pub humidity: Option<f32>,
    /// PM2.5, µg/m³.
    pub pm25: Option<f32>,
    /// Outdoor temperature from the forecast for the current hour, °C.
    pub outdoor: Option<f32>,
}

/// Averages of the readings within one bucket.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bucket {
    /// Start of the bucket.
    pub start: Minutes,
    /// Mean values.
    pub mean: Reading,
}

#[derive(Clone, Copy, Debug, Default)]
struct Mean {
    sum: f32,
    count: u16,
}

impl Mean {
    fn add(&mut self, value: Option<f32>) {
        if let Some(v) = value {
            self.sum += v;
            self.count += 1;
        }
    }

    fn get(&self) -> Option<f32> {
        (self.count > 0).then(|| self.sum / self.count as f32)
    }
}

/// Ring buffer of the last 24 hours.
#[derive(Clone, Debug)]
pub struct History {
    buckets: [Bucket; CAPACITY],
    len: usize,
    head: usize,
    open: Option<(Minutes, [Mean; 4])>,
}

impl Default for History {
    fn default() -> Self {
        Self { buckets: [Bucket::default(); CAPACITY], len: 0, head: 0, open: None }
    }
}

impl History {
    /// Record a reading taken at `now`. Readings are averaged into 10-minute buckets; a bucket
    /// is closed when the first reading of a later bucket arrives.
    pub fn record(&mut self, now: Minutes, reading: Reading) {
        let start = Minutes(now.0 - now.0 % BUCKET_MINUTES);
        match &mut self.open {
            Some((open_start, _)) if *open_start == start => {}
            Some((open_start, _)) if *open_start > start => return, // clock moved backwards
            _ => {
                self.close();
                self.open = Some((start, [Mean::default(); 4]));
            }
        }
        let (_, means) = self.open.as_mut().unwrap();
        means[0].add(reading.celsius);
        means[1].add(reading.humidity);
        means[2].add(reading.pm25);
        means[3].add(reading.outdoor);
    }

    fn close(&mut self) {
        if let Some((start, means)) = self.open.take() {
            let bucket = Bucket {
                start,
                mean: Reading {
                    celsius: means[0].get(),
                    humidity: means[1].get(),
                    pm25: means[2].get(),
                    outdoor: means[3].get(),
                },
            };
            self.buckets[self.head] = bucket;
            self.head = (self.head + 1) % CAPACITY;
            self.len = (self.len + 1).min(CAPACITY);
        }
    }

    /// Forget everything (for example after the clock was changed).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Closed buckets, oldest first.
    pub fn buckets(&self) -> impl Iterator<Item = &Bucket> {
        let start = (self.head + CAPACITY - self.len) % CAPACITY;
        (0..self.len).map(move |i| &self.buckets[(start + i) % CAPACITY])
    }

    /// Mean room temperature of the buckets starting within 15 minutes of `at`.
    pub fn celsius_near(&self, at: Minutes) -> Option<f32> {
        let from = Minutes(at.0.saturating_sub(15));
        let mut mean = Mean::default();
        for b in self.between(from, Minutes(at.0 + 15)) {
            mean.add(b.mean.celsius);
        }
        mean.get()
    }

    /// Closed buckets starting within `[from, to)`.
    pub fn between(&self, from: Minutes, to: Minutes) -> impl Iterator<Item = &Bucket> {
        self.buckets().filter(move |b| b.start >= from && b.start < to)
    }

    /// Summarise `[from, to)`, or `None` if nothing was recorded then.
    pub fn summarize(&self, from: Minutes, to: Minutes) -> Option<NightSummary> {
        let mut s = NightSummary {
            from,
            to,
            covered_minutes: 0,
            celsius: Stats::default(),
            humidity: Stats::default(),
            pm25: Stats::default(),
            outdoor: Stats::default(),
            hourly: [HourSlot::default(); 12],
        };
        let mut slot_sums = [(Mean::default(), Mean::default()); 12];
        for b in self.between(from, to) {
            s.covered_minutes += BUCKET_MINUTES;
            s.celsius.add(b.start, b.mean.celsius);
            s.humidity.add(b.start, b.mean.humidity);
            s.pm25.add(b.start, b.mean.pm25);
            s.outdoor.add(b.start, b.mean.outdoor);
            let slot = ((b.start.0 - from.0) * 12 / (to.0 - from.0).max(1)) as usize;
            if let Some((t, pm)) = slot_sums.get_mut(slot) {
                t.add(b.mean.celsius);
                pm.add(b.mean.pm25);
            }
        }
        if s.covered_minutes == 0 {
            return None;
        }
        for (slot, (t, pm)) in s.hourly.iter_mut().zip(slot_sums) {
            *slot = HourSlot { celsius: t.get(), pm25: pm.get() };
        }
        Some(s)
    }
}

/// Minimum, mean and maximum of one quantity.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    /// Lowest value and when it occurred.
    pub min: Option<(f32, Minutes)>,
    /// Highest value and when it occurred.
    pub max: Option<(f32, Minutes)>,
    sum: f32,
    count: u32,
}

impl Stats {
    fn add(&mut self, at: Minutes, value: Option<f32>) {
        let Some(v) = value else { return };
        if self.min.is_none_or(|(m, _)| v < m) {
            self.min = Some((v, at));
        }
        if self.max.is_none_or(|(m, _)| v > m) {
            self.max = Some((v, at));
        }
        self.sum += v;
        self.count += 1;
    }

    /// Mean value.
    pub fn mean(&self) -> Option<f32> {
        (self.count > 0).then(|| self.sum / self.count as f32)
    }
}

/// Mean values for one twelfth of the night (one hour of a 12-hour night).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HourSlot {
    /// Mean temperature.
    pub celsius: Option<f32>,
    /// Mean PM2.5.
    pub pm25: Option<f32>,
}

/// Overnight summary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NightSummary {
    /// Start of the summarised period.
    pub from: Minutes,
    /// End of the summarised period.
    pub to: Minutes,
    /// Minutes with data.
    pub covered_minutes: u32,
    /// Room temperature.
    pub celsius: Stats,
    /// Relative humidity.
    pub humidity: Stats,
    /// PM2.5.
    pub pm25: Stats,
    /// Outdoor temperature (from the forecast).
    pub outdoor: Stats,
    /// Twelve equal slots across the period, for the charts.
    pub hourly: [HourSlot; 12],
}

impl NightSummary {
    /// Air quality from the mean PM2.5.
    pub fn air_quality(&self) -> Option<AirQuality> {
        self.pm25.mean().map(AirQuality::from_pm25)
    }

    /// Air quality during the worst 10 minutes.
    pub fn worst_air_quality(&self) -> Option<AirQuality> {
        self.pm25.max.map(|(v, _)| AirQuality::from_pm25(v))
    }

    /// Fraction of the period with data, 0..=1.
    pub fn coverage(&self) -> f32 {
        self.covered_minutes as f32 / (self.to.0 - self.from.0).max(1) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(t: f32, pm: f32) -> Reading {
        Reading { celsius: Some(t), humidity: Some(50.0), pm25: Some(pm), outdoor: None }
    }

    #[test]
    fn buckets_average_and_ring() {
        let mut h = History::default();
        h.record(Minutes(0), reading(20.0, 4.0));
        h.record(Minutes(5), reading(22.0, 6.0));
        h.record(Minutes(10), reading(30.0, 0.0));
        let b: std::vec::Vec<_> = h.buckets().copied().collect();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].mean.celsius, Some(21.0));
        assert_eq!(b[0].mean.pm25, Some(5.0));

        for m in (20..3 * 24 * 60).step_by(10) {
            h.record(Minutes(m), reading(m as f32, 1.0));
        }
        assert_eq!(h.buckets().count(), CAPACITY);
        let first = h.buckets().next().unwrap().start;
        assert_eq!(first.0, 3 * 24 * 60 - 10 - CAPACITY as u32 * 10);
    }

    #[test]
    fn night_summary() {
        let mut h = History::default();
        let from = Minutes::hm(19, 0);
        let to = Minutes(from.0 + 12 * 60);
        for m in (from.0..to.0 + 10).step_by(5) {
            // Cools from 22 °C to 18 °C, with a PM spike at 23:00.
            let t = 22.0 - 4.0 * (m - from.0) as f32 / (12.0 * 60.0);
            let pm = if (23 * 60..23 * 60 + 10).contains(&m) { 40.0 } else { 5.0 };
            h.record(Minutes(m), reading(t, pm));
        }
        let s = h.summarize(from, to).unwrap();
        assert_eq!(s.covered_minutes, 12 * 60);
        let (min, at) = s.celsius.min.unwrap();
        assert!((min - 18.0).abs() < 0.1, "{min}");
        assert!(at.0 > to.0 - 20);
        assert_eq!(s.pm25.max.unwrap().1, Minutes::hm(23, 0));
        assert_eq!(s.air_quality(), Some(AirQuality::Good));
        assert_eq!(s.worst_air_quality(), Some(AirQuality::Sensitive));
        assert!(s.hourly.iter().all(|slot| slot.celsius.is_some()));
        assert!(h.summarize(Minutes(to.0 + 600), Minutes(to.0 + 700)).is_none());
    }
}

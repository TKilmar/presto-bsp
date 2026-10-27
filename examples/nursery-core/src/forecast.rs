//! Outdoor weather forecast and the "will it rain today" summary.
//!
//! The firmware has no network connection yet, so on the device [`Forecast`] is empty and the
//! UI says so. The desktop simulator fills it from its controls.

use crate::clock::Minutes;

/// One forecast hour.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hour {
    /// Start of the hour.
    pub start: Minutes,
    /// Outdoor air temperature, °C.
    pub celsius: f32,
    /// Probability of precipitation, 0–100 %.
    pub rain_chance: u8,
}

/// Hourly forecast covering at least the next 24 hours.
#[derive(Clone, Debug)]
pub struct Forecast {
    hours: [Hour; 36],
    len: usize,
}

impl Default for Forecast {
    fn default() -> Self {
        Self { hours: [Hour::default(); 36], len: 0 }
    }
}

impl Forecast {
    /// Build a forecast from consecutive hours (extra hours beyond 36 are ignored).
    pub fn from_hours(hours: impl IntoIterator<Item = Hour>) -> Self {
        let mut forecast = Self::default();
        for hour in hours.into_iter().take(36) {
            forecast.hours[forecast.len] = hour;
            forecast.len += 1;
        }
        forecast
    }

    /// The forecast hours.
    pub fn hours(&self) -> &[Hour] {
        &self.hours[..self.len]
    }

    /// `true` if there is no forecast.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Hours overlapping `[from, to)`.
    pub fn between(&self, from: Minutes, to: Minutes) -> impl Iterator<Item = &Hour> {
        self.hours().iter().filter(move |h| h.start.0 + 60 > from.0 && h.start.0 < to.0)
    }

    /// Lowest outdoor temperature in `[from, to)`.
    pub fn low_between(&self, from: Minutes, to: Minutes) -> Option<f32> {
        self.between(from, to).map(|h| h.celsius).reduce(f32::min)
    }

    /// Rain outlook for `[from, to)`.
    pub fn rain_between(&self, from: Minutes, to: Minutes) -> Option<RainOutlook> {
        let mut outlook: Option<RainOutlook> = None;
        for hour in self.between(from, to) {
            let o = outlook.get_or_insert(RainOutlook { max_chance: 0, likely_from: None, likely_until: None });
            o.max_chance = o.max_chance.max(hour.rain_chance);
            if hour.rain_chance >= RainOutlook::LIKELY {
                o.likely_from.get_or_insert(hour.start);
                o.likely_until = Some(Minutes(hour.start.0 + 60));
            }
        }
        outlook
    }
}

/// Summary of the rain forecast for a period.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RainOutlook {
    /// Highest hourly probability.
    pub max_chance: u8,
    /// Start of the first hour with a likely chance of rain.
    pub likely_from: Option<Minutes>,
    /// End of the last hour with a likely chance of rain.
    pub likely_until: Option<Minutes>,
}

impl RainOutlook {
    /// Hourly probability at which rain counts as likely.
    pub const LIKELY: u8 = 50;

    /// 0 = dry, 1 = possible showers, 2 = rain likely (for the UI icon).
    pub fn level(&self) -> i32 {
        match self.max_chance {
            c if c >= Self::LIKELY => 2,
            c if c >= 25 => 1,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day() -> Forecast {
        Forecast::from_hours((0..24).map(|h| Hour {
            start: Minutes::hm(h, 0),
            celsius: 10.0 + h as f32 * 0.5,
            rain_chance: if (14..17).contains(&h) { 70 } else { 10 },
        }))
    }

    #[test]
    fn rain_window() {
        let rain = day().rain_between(Minutes::hm(7, 0), Minutes::hm(20, 0)).unwrap();
        assert_eq!(rain.max_chance, 70);
        assert_eq!(rain.likely_from, Some(Minutes::hm(14, 0)));
        assert_eq!(rain.likely_until, Some(Minutes::hm(17, 0)));
        assert_eq!(rain.level(), 2);
        let dry = day().rain_between(Minutes::hm(7, 0), Minutes::hm(12, 0)).unwrap();
        assert_eq!((dry.level(), dry.likely_from), (0, None));
    }

    #[test]
    fn low() {
        assert_eq!(day().low_between(Minutes::hm(2, 0), Minutes::hm(6, 0)), Some(11.0));
        assert!(Forecast::default().rain_between(Minutes(0), Minutes(60)).is_none());
    }
}

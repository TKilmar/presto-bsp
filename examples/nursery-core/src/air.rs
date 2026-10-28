//! Air-quality categories: PM2.5 (US EPA AQI breakpoints as revised in 2024) and CO₂
//! (ventilation, after the German UBA guidance for indoor air: below 1000 ppm is fine, above
//! 2000 ppm is unacceptable).

/// Air-quality category for a PM2.5 concentration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AirQuality {
    /// 0 – 9.0 µg/m³.
    Good,
    /// 9.1 – 35.4 µg/m³.
    Moderate,
    /// 35.5 – 55.4 µg/m³: unhealthy for sensitive groups, which includes infants.
    Sensitive,
    /// 55.5 – 125.4 µg/m³.
    Unhealthy,
    /// 125.5 – 225.4 µg/m³.
    VeryUnhealthy,
    /// Above 225.4 µg/m³.
    Hazardous,
}

impl AirQuality {
    /// Categorise a PM2.5 concentration in µg/m³ (the sensor reports whole numbers).
    pub fn from_pm25(ug_m3: f32) -> Self {
        // Breakpoints are defined on values truncated to one decimal place.
        let c = libm::truncf(ug_m3 * 10.0) / 10.0;
        match c {
            c if c <= 9.0 => Self::Good,
            c if c <= 35.4 => Self::Moderate,
            c if c <= 55.4 => Self::Sensitive,
            c if c <= 125.4 => Self::Unhealthy,
            c if c <= 225.4 => Self::VeryUnhealthy,
            _ => Self::Hazardous,
        }
    }

    /// Short label for the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::Good => "Good",
            Self::Moderate => "Moderate",
            Self::Sensitive => "Poor for baby",
            Self::Unhealthy => "Unhealthy",
            Self::VeryUnhealthy => "Very unhealthy",
            Self::Hazardous => "Hazardous",
        }
    }

    /// Index 0..=5, used by the UI to pick a colour.
    pub fn level(self) -> i32 {
        self as i32
    }
}

/// Ventilation category for a CO₂ concentration. Outdoor air is about 420 ppm; a closed bedroom
/// with a sleeping baby and a parent climbs past 1000 ppm within hours.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Co2Level {
    /// Up to 800 ppm.
    Fresh,
    /// 801 – 1000 ppm.
    Fine,
    /// 1001 – 1400 ppm: open a window or the door.
    Stuffy,
    /// 1401 – 2000 ppm.
    Poor,
    /// Above 2000 ppm.
    VeryPoor,
}

impl Co2Level {
    /// Categorise a CO₂ concentration in ppm.
    pub fn from_ppm(ppm: f32) -> Self {
        match ppm {
            p if p <= 800.0 => Self::Fresh,
            p if p <= 1000.0 => Self::Fine,
            p if p <= 1400.0 => Self::Stuffy,
            p if p <= 2000.0 => Self::Poor,
            _ => Self::VeryPoor,
        }
    }

    /// Short label for the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::Fresh => "Fresh",
            Self::Fine => "Fine",
            Self::Stuffy => "Stuffy",
            Self::Poor => "Ventilate",
            Self::VeryPoor => "Very stuffy",
        }
    }

    /// Index 0..=4 on the same colour scale as [`AirQuality::level`].
    pub fn level(self) -> i32 {
        self as i32
    }
}

/// The worse of the PM2.5 and CO₂ categories as (label, colour level), or `None` without either
/// sensor. On a tie the PM2.5 label wins ("Good" rather than "Fresh").
pub fn overall(pm: Option<AirQuality>, co2: Option<Co2Level>) -> Option<(&'static str, i32)> {
    match (pm, co2) {
        (Some(pm), Some(co2)) if co2.level() > pm.level() => Some((co2.label(), co2.level())),
        (Some(pm), _) => Some((pm.label(), pm.level())),
        (None, Some(co2)) => Some((co2.label(), co2.level())),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn co2() {
        assert_eq!(Co2Level::from_ppm(420.0), Co2Level::Fresh);
        assert_eq!(Co2Level::from_ppm(1000.0), Co2Level::Fine);
        assert_eq!(Co2Level::from_ppm(1200.0), Co2Level::Stuffy);
        assert_eq!(Co2Level::from_ppm(2500.0), Co2Level::VeryPoor);
        assert_eq!(overall(Some(AirQuality::Good), Some(Co2Level::Fresh)), Some(("Good", 0)));
        assert_eq!(overall(Some(AirQuality::Good), Some(Co2Level::Stuffy)), Some(("Stuffy", 2)));
        assert_eq!(overall(None, Some(Co2Level::Fine)), Some(("Fine", 1)));
        assert_eq!(overall(None, None), None);
    }

    #[test]
    fn breakpoints() {
        assert_eq!(AirQuality::from_pm25(0.0), AirQuality::Good);
        assert_eq!(AirQuality::from_pm25(9.0), AirQuality::Good);
        assert_eq!(AirQuality::from_pm25(9.05), AirQuality::Good);
        assert_eq!(AirQuality::from_pm25(9.1), AirQuality::Moderate);
        assert_eq!(AirQuality::from_pm25(35.4), AirQuality::Moderate);
        assert_eq!(AirQuality::from_pm25(35.5), AirQuality::Sensitive);
        assert_eq!(AirQuality::from_pm25(125.5), AirQuality::VeryUnhealthy);
        assert_eq!(AirQuality::from_pm25(500.0), AirQuality::Hazardous);
    }
}

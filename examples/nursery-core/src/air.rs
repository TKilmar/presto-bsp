//! PM2.5 air-quality categories (US EPA AQI breakpoints as revised in 2024).

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

#[cfg(test)]
mod tests {
    use super::*;

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

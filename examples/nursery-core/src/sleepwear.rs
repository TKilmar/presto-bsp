//! Baby sleepwear recommendations by room temperature.
//!
//! The ladder follows the room-temperature / TOG charts published by sleeping-bag makers and
//! the UK Lullaby Trust's advice (aim for a 16–20 °C room, dress lighter rather than warmer, no
//! hats indoors). The clothing TOG values are approximate. Adjust [`LADDER`] to taste: the
//! desktop simulator lets you check the result for every temperature.

/// What the baby wears under the sleeping bag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outfit {
    /// Nappy / diaper only.
    DiaperOnly,
    /// Short-sleeved bodysuit.
    ShortSleeve,
    /// Long-sleeved bodysuit.
    LongSleeve,
    /// Long-sleeved bodysuit under a footed sleepsuit (pyjamas).
    LongSleeveAndSleepsuit,
    /// Footed sleepsuit with no bodysuit underneath.
    SleepsuitOnly,
}

impl Outfit {
    /// Approximate insulation of the clothing on its own, in TOG.
    pub fn tog(self) -> f32 {
        match self {
            Self::DiaperOnly => 0.0,
            Self::ShortSleeve => 0.2,
            Self::LongSleeve => 0.3,
            Self::LongSleeveAndSleepsuit => 1.3,
            Self::SleepsuitOnly => 1.0,
        }
    }

    /// Whether the outfit includes a bodysuit.
    pub fn has_bodysuit(self) -> bool {
        matches!(self, Self::ShortSleeve | Self::LongSleeve | Self::LongSleeveAndSleepsuit)
    }

    /// The same outfit with the bodysuit left out.
    pub fn without_bodysuit(self) -> Self {
        match self {
            Self::ShortSleeve | Self::LongSleeve => Self::DiaperOnly,
            Self::LongSleeveAndSleepsuit => Self::SleepsuitOnly,
            other => other,
        }
    }

    /// Description for the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::DiaperOnly => "Nappy only",
            Self::ShortSleeve => "Short-sleeve bodysuit",
            Self::LongSleeve => "Long-sleeve bodysuit",
            Self::LongSleeveAndSleepsuit => "Long-sleeve bodysuit + sleepsuit",
            Self::SleepsuitOnly => "Footed sleepsuit, no bodysuit",
        }
    }

    /// Index used by the UI to pick the illustration.
    pub fn index(self) -> i32 {
        self as i32
    }
}

/// How the room temperature compares with the recommended 16–20 °C.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comfort {
    /// Below 16 °C.
    Cold,
    /// 16–20 °C.
    Ideal,
    /// 20–24 °C.
    Warm,
    /// 24 °C and above: risk of overheating.
    Hot,
}

impl Comfort {
    /// Classify a room temperature.
    pub fn from_room(celsius: f32) -> Self {
        match celsius {
            t if t < 16.0 => Self::Cold,
            t if t <= 20.0 => Self::Ideal,
            t if t < 24.0 => Self::Warm,
            _ => Self::Hot,
        }
    }

    /// Index 0..=3 for the UI.
    pub fn index(self) -> i32 {
        self as i32
    }
}

/// One rung of the ladder: applies from `min_celsius` up to the next warmer rung.
#[derive(Clone, Copy, Debug)]
pub struct Band {
    /// Lowest room temperature this band covers.
    pub min_celsius: f32,
    /// Clothing under the bag.
    pub outfit: Outfit,
    /// Sleeping bag rating in TOG (0 means no bag).
    pub bag_tog: f32,
}

/// Recommendation ladder, warmest first.
pub const LADDER: &[Band] = &[
    Band { min_celsius: 27.0, outfit: Outfit::DiaperOnly, bag_tog: 0.2 },
    Band { min_celsius: 24.0, outfit: Outfit::ShortSleeve, bag_tog: 0.5 },
    Band { min_celsius: 22.0, outfit: Outfit::ShortSleeve, bag_tog: 1.0 },
    Band { min_celsius: 20.0, outfit: Outfit::LongSleeve, bag_tog: 1.0 },
    Band { min_celsius: 18.0, outfit: Outfit::LongSleeve, bag_tog: 2.5 },
    Band { min_celsius: 16.0, outfit: Outfit::LongSleeveAndSleepsuit, bag_tog: 2.5 },
    Band { min_celsius: f32::NEG_INFINITY, outfit: Outfit::LongSleeveAndSleepsuit, bag_tog: 3.5 },
];

/// A sleepwear recommendation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Recommendation {
    /// Room temperature the recommendation is for.
    pub room_celsius: f32,
    /// Clothing under the bag.
    pub outfit: Outfit,
    /// Sleeping bag TOG.
    pub bag_tog: f32,
    /// How the room compares with the recommended range.
    pub comfort: Comfort,
    /// Set when the user skips the bodysuit: the outfit the chart asked for.
    pub skipped_from: Option<Outfit>,
    /// TOG still missing when even the warmest bag cannot replace the bodysuit.
    pub short_by: f32,
}

/// Sleeping bag ratings, lightest first.
pub const BAGS: [f32; 5] = [0.2, 0.5, 1.0, 2.5, 3.5];

impl Recommendation {
    /// Approximate total insulation (bag plus clothing).
    pub fn total_tog(&self) -> f32 {
        self.bag_tog + self.outfit.tog()
    }

    /// Index of the bag in the sequence 0.2, 0.5, 1.0, 2.5, 3.5 (for the UI colour).
    pub fn bag_index(&self) -> i32 {
        BAGS.iter()
            .position(|&t| (t - self.bag_tog).abs() < 0.01)
            .unwrap_or(0) as i32
    }

    /// Adapt to a baby who sleeps without a bodysuit: drop it, and pick the lightest bag that
    /// keeps at least the same total insulation (never a lighter bag than before).
    pub fn without_bodysuit(self) -> Self {
        if !self.outfit.has_bodysuit() {
            return self;
        }
        let outfit = self.outfit.without_bodysuit();
        let target = self.total_tog();
        let clothes = outfit.tog();
        let bag = BAGS
            .iter()
            .copied()
            .find(|&bag| bag >= self.bag_tog && bag + clothes >= target - 0.05)
            .unwrap_or(BAGS[BAGS.len() - 1]);
        Self {
            outfit,
            bag_tog: bag,
            skipped_from: Some(self.outfit),
            short_by: (target - bag - clothes).max(0.0),
            ..self
        }
    }

    /// One-line advice for the parent, or `""`.
    pub fn advice(&self) -> &'static str {
        if self.short_by > 0.05 {
            return "Warmest bag still a little light: add a layer";
        }
        match self.comfort {
            Comfort::Hot => "Hot room: cool it, no blankets",
            Comfort::Warm => "Check chest: warm, not sweaty",
            Comfort::Ideal => "Ideal room temperature",
            Comfort::Cold => "Cold room: heat to 16 °C+",
        }
    }
}

/// Thermal sensitivity steps: how warm the baby tends to sleep.
pub const SENSITIVITY_RANGE: core::ops::RangeInclusive<i8> = -2..=2;
/// Each sensitivity step moves the ladder by this many °C.
pub const SENSITIVITY_STEP_CELSIUS: f32 = 1.0;

/// Label for a sensitivity step.
pub fn sensitivity_label(step: i8) -> &'static str {
    match step {
        i8::MIN..=-2 => "Runs cool",
        -1 => "A little cool",
        0 => "Typical",
        1 => "A little warm",
        _ => "Runs warm",
    }
}

/// What a sensitivity step does, for the settings screen.
pub fn sensitivity_explanation(step: i8) -> &'static str {
    match step {
        i8::MIN..=-2 => "Dressed as if the room were 2 °C cooler",
        -1 => "Dressed as if the room were 1 °C cooler",
        0 => "Standard sleeping-bag chart",
        1 => "Dressed as if the room were 1 °C warmer",
        _ => "Dressed as if the room were 2 °C warmer",
    }
}

/// Recommend sleepwear for a room temperature in °C.
pub fn recommend(room_celsius: f32) -> Recommendation {
    recommend_for(room_celsius, 0)
}

/// Recommend sleepwear for a baby who runs warm (`sensitivity` > 0, dress lighter) or cool
/// (< 0, dress warmer). The comfort rating still describes the real room.
pub fn recommend_for(room_celsius: f32, sensitivity: i8) -> Recommendation {
    let step = sensitivity.clamp(*SENSITIVITY_RANGE.start(), *SENSITIVITY_RANGE.end());
    let effective = room_celsius + step as f32 * SENSITIVITY_STEP_CELSIUS;
    let band = LADDER
        .iter()
        .find(|band| effective >= band.min_celsius)
        .unwrap_or(&LADDER[LADDER.len() - 1]);
    Recommendation {
        room_celsius,
        outfit: band.outfit,
        bag_tog: band.bag_tog,
        comfort: Comfort::from_room(room_celsius),
        skipped_from: None,
        short_by: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_is_ordered_warmest_first() {
        for pair in LADDER.windows(2) {
            assert!(pair[0].min_celsius > pair[1].min_celsius);
            let warmer = pair[0].bag_tog + pair[0].outfit.tog();
            let cooler = pair[1].bag_tog + pair[1].outfit.tog();
            assert!(cooler > warmer, "insulation must increase as the room cools");
        }
    }

    #[test]
    fn without_bodysuit_keeps_insulation() {
        // 21 °C: 1.0 TOG + long sleeve (1.3) -> nappy only needs the 2.5 TOG bag.
        let r = recommend(21.0).without_bodysuit();
        assert_eq!((r.outfit, r.bag_tog, r.skipped_from), (Outfit::DiaperOnly, 2.5, Some(Outfit::LongSleeve)));
        // 25 °C: 0.5 TOG + short sleeve (0.7) -> 1.0 TOG bag.
        assert_eq!(recommend(25.0).without_bodysuit().bag_tog, 1.0);
        // 23 °C: 1.0 + short sleeve (1.2) -> 2.5 bag (no 1.2 bag exists).
        assert_eq!(recommend(23.0).without_bodysuit().bag_tog, 2.5);
        // 12 °C: 3.5 + sleepsuit + bodysuit (4.8) -> sleepsuit only, 3.5 bag, 0.3 short.
        let r = recommend(12.0).without_bodysuit();
        assert_eq!((r.outfit, r.bag_tog), (Outfit::SleepsuitOnly, 3.5));
        assert!((r.short_by - 0.3).abs() < 0.01);
        assert!(r.advice().contains("add a layer"));
        // Nothing to skip at 28 °C.
        assert_eq!(recommend(28.0).without_bodysuit(), recommend(28.0));
    }

    #[test]
    fn sensitivity_shifts_bands() {
        // 21 °C: typical is 1.0 TOG; a warm sleeper gets the 22 °C band, a cool one the 19 °C band.
        assert_eq!(recommend_for(21.0, 0).bag_tog, 1.0);
        assert_eq!(recommend_for(21.0, 1).outfit, Outfit::ShortSleeve);
        assert_eq!(recommend_for(21.0, -2).bag_tog, 2.5);
        assert_eq!(recommend_for(21.0, 2).comfort, Comfort::Warm, "comfort describes the real room");
        assert_eq!(recommend_for(21.0, 9), recommend_for(21.0, 2), "clamped");
    }

    #[test]
    fn examples() {
        let r = recommend(28.0);
        assert_eq!((r.outfit, r.bag_tog, r.comfort), (Outfit::DiaperOnly, 0.2, Comfort::Hot));
        let r = recommend(21.0);
        assert_eq!((r.outfit, r.bag_tog), (Outfit::LongSleeve, 1.0));
        let r = recommend(18.0);
        assert_eq!((r.outfit, r.bag_tog, r.comfort), (Outfit::LongSleeve, 2.5, Comfort::Ideal));
        let r = recommend(12.0);
        assert_eq!((r.outfit, r.bag_tog, r.comfort), (Outfit::LongSleeveAndSleepsuit, 3.5, Comfort::Cold));
        assert_eq!(r.bag_index(), 4);
    }
}

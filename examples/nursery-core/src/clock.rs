//! Local wall-clock time and the display schedule.

/// Minutes in a day.
pub const DAY: u32 = 24 * 60;

/// Local time as minutes since an arbitrary midnight. The platform decides where "day 0" is;
/// only the time of day and differences between timestamps matter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Minutes(pub u32);

impl Minutes {
    /// Build a time on day 0.
    pub const fn hm(hour: u32, minute: u32) -> Self {
        Self(hour * 60 + minute)
    }

    /// Minutes since midnight, 0..1440.
    pub fn of_day(self) -> u32 {
        self.0 % DAY
    }

    /// Hour of day, 0..24.
    pub fn hour(self) -> u32 {
        self.of_day() / 60
    }

    /// Minute of the hour, 0..60.
    pub fn minute(self) -> u32 {
        self.of_day() % 60
    }

    /// Day number.
    pub fn day(self) -> u32 {
        self.0 / DAY
    }

    /// The most recent time (at or before `self`) whose time of day is `hour:minute`.
    pub fn previous(self, hour: u32, minute: u32) -> Self {
        let target = hour * 60 + minute;
        let today = self.day() * DAY + target;
        if today <= self.0 { Self(today) } else { Self(today.saturating_sub(DAY)) }
    }
}

/// Which screen to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// Overnight summary, from wake-up until late morning.
    Morning,
    /// Tonight's plan, the rest of the time.
    Evening,
}

/// The overnight window used for summaries and plans.
pub const NIGHT_START: Minutes = Minutes::hm(19, 0);
/// End of the overnight window.
pub const NIGHT_END: Minutes = Minutes::hm(7, 0);
/// The morning screen shows from 05:00 until this time.
pub const MORNING_UNTIL: Minutes = Minutes::hm(11, 0);

/// Pick the screen for a time of day.
pub fn view_at(now: Minutes) -> View {
    let t = now.of_day();
    if (Minutes::hm(5, 0).0..MORNING_UNTIL.0).contains(&t) { View::Morning } else { View::Evening }
}

/// The night that ended most recently (or is in progress): `(start, end)`.
pub fn last_night(now: Minutes) -> (Minutes, Minutes) {
    let end = now.previous(NIGHT_END.hour(), NIGHT_END.minute());
    let end = if now.0 - end.0 < 12 * 60 { end } else { Minutes(end.0 + DAY) };
    (Minutes(end.0 - 12 * 60), end)
}

/// The coming (or current) night: `(start, end)`.
pub fn tonight(now: Minutes) -> (Minutes, Minutes) {
    let start = now.previous(NIGHT_START.hour(), NIGHT_START.minute());
    // Before 07:00 we are still inside the night that started yesterday evening.
    let start = if now.0 - start.0 < 12 * 60 { start } else { Minutes(start.0 + DAY) };
    (start, Minutes(start.0 + 12 * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views() {
        assert_eq!(view_at(Minutes::hm(6, 30)), View::Morning);
        assert_eq!(view_at(Minutes::hm(10, 59)), View::Morning);
        assert_eq!(view_at(Minutes::hm(11, 0)), View::Evening);
        assert_eq!(view_at(Minutes::hm(2, 0)), View::Evening);
    }

    #[test]
    fn windows() {
        let morning = Minutes(DAY + Minutes::hm(8, 0).0);
        assert_eq!(last_night(morning), (Minutes::hm(19, 0), Minutes(DAY + 7 * 60)));
        let evening = Minutes(DAY + Minutes::hm(20, 0).0);
        assert_eq!(tonight(evening), (Minutes(DAY + 19 * 60), Minutes(2 * DAY + 7 * 60)));
        let afternoon = Minutes(DAY + Minutes::hm(15, 0).0);
        assert_eq!(tonight(afternoon), (Minutes(DAY + 19 * 60), Minutes(2 * DAY + 7 * 60)));
        let small_hours = Minutes(DAY + Minutes::hm(3, 0).0);
        assert_eq!(tonight(small_hours), (Minutes(19 * 60), Minutes(DAY + 7 * 60)));
    }
}

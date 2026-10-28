//! Application state and the bridge to the Slint UI, shared by the Presto firmware
//! (`examples/src/bin/nursery.rs`) and the desktop simulator (`examples/nursery-desktop`).
//!
//! Each crate includes this file with `#[path = ...] mod app;` after `slint::include_modules!()`,
//! so it compiles against that crate's generated UI types.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use nursery_core::air::{self, AirQuality, Co2Level};
use nursery_core::clock::{self, Minutes, View};
use nursery_core::forecast::Forecast;
use nursery_core::history::{History, NightSummary, Reading};
use nursery_core::planner::{self, Basis, Plan};
use nursery_core::settings::Settings;
use nursery_core::sleepwear::{self, Comfort, Recommendation};
use slint::{Model, ModelRc, SharedString, VecModel};

use crate::{AboutRow, Bar, Evening, Morning, NurseryScreen, SettingsState, TextOps, WifiNetwork};

/// Pages, left to right.
pub const PAGE_MORNING: i32 = 0;
/// Tonight's plan.
pub const PAGE_TONIGHT: i32 = 1;
/// Settings.
#[allow(dead_code)]
pub const PAGE_SETTINGS: i32 = 2;

/// Where the Wi-Fi connection is.
#[derive(Clone, Debug, PartialEq)]
pub enum WifiLink {
    /// Radio unpowered.
    Off,
    /// Loading firmware / bringing the radio up.
    Starting,
    /// Radio on, not associated.
    Idle,
    /// Joining a network.
    Connecting(String),
    /// Associated and has an IP address.
    Connected { ssid: String, ip: String },
    /// The last attempt failed.
    Failed(String),
}

/// A network found by a scan.
#[derive(Clone, Debug, PartialEq)]
pub struct ScannedNetwork {
    /// Network name.
    pub ssid: String,
    /// Signal strength, dBm.
    pub rssi: i16,
    /// Needs a password.
    pub secure: bool,
}

/// Wi-Fi state shown on the settings page. The platform updates it.
#[derive(Clone, Debug)]
pub struct WifiState {
    /// Connection state.
    pub link: WifiLink,
    /// A scan is running.
    pub scanning: bool,
    /// Results of the last scan, strongest first.
    pub networks: Vec<ScannedNetwork>,
}

impl Default for WifiState {
    fn default() -> Self {
        Self { link: WifiLink::Off, scanning: false, networks: Vec::new() }
    }
}

/// How long the screen stays on after the last touch.
pub const SCREEN_TIMEOUT_MS: u64 = 3 * 60 * 1000;

/// Everything the screen shows.
pub struct App {
    /// Room readings over the last 24 hours.
    pub history: History,
    /// Outdoor forecast (empty until the device has a network connection).
    pub forecast: Forecast,
    /// Most recent reading.
    pub latest: Reading,
    /// Local time, or `None` until it has been set.
    pub now: Option<Minutes>,
    /// Page the user swiped to; cleared when the screen goes to sleep.
    pub page_override: Option<i32>,
    /// Shown when there is no forecast.
    pub no_forecast_reason: &'static str,
    /// User settings.
    pub settings: Settings,
    /// Wi-Fi state.
    pub wifi: WifiState,
    /// Firmware version line, e.g. "v0.1.0 (abc1234)".
    pub version: String,
    /// Rows for the About panel (label, value), filled in by the platform.
    pub about: Vec<(String, String)>,
    last_activity_ms: u64,
    asleep: bool,
}

impl App {
    /// New state; `now_ms` is the platform's monotonic time.
    pub fn new(now_ms: u64) -> Self {
        Self {
            history: History::default(),
            forecast: Forecast::default(),
            latest: Reading::default(),
            now: None,
            page_override: None,
            no_forecast_reason: "No forecast: Wi-Fi is off",
            settings: Settings::default(),
            wifi: WifiState::default(),
            version: String::new(),
            about: Vec::new(),
            last_activity_ms: now_ms,
            asleep: false,
        }
    }

    /// Record a sensor reading at the current time (the forecast supplies the outdoor value).
    pub fn record(&mut self, mut reading: Reading) {
        self.latest = reading;
        if let Some(now) = self.now {
            reading.outdoor = self.forecast.between(now, Minutes(now.0 + 1)).next().map(|h| h.celsius);
            self.history.record(now, reading);
        }
    }

    /// Set the clock to an absolute local time (e.g. from the network), keeping the history if
    /// the change is small.
    pub fn set_time(&mut self, now: Minutes) {
        if self.now.is_none_or(|old| old.0.abs_diff(now.0) > 30) {
            self.history.clear();
        }
        self.now = Some(now);
    }

    /// Move the clock by `minutes`. History recorded against the old time no longer lines up,
    /// so it is cleared.
    pub fn adjust_clock(&mut self, minutes: i32) {
        let base = self.now.unwrap_or(Minutes::hm(19, 0)).0 as i64 + clock::DAY as i64;
        let adjusted = (base + minutes as i64).max(0) as u32;
        // Keep the day number, change only the time of day.
        let day = self.now.map_or(1, |n| n.day());
        self.now = Some(Minutes(day * clock::DAY + adjusted % clock::DAY));
        self.history.clear();
    }

    /// Note a touch. Returns `true` if it woke the screen (the touch should then be ignored).
    pub fn touch(&mut self, now_ms: u64) -> bool {
        self.last_activity_ms = now_ms;
        core::mem::replace(&mut self.asleep, false)
    }

    /// Check the idle timeout. Returns `Some(asleep)` when the state changed.
    pub fn poll_sleep(&mut self, now_ms: u64, timeout_ms: u64) -> Option<bool> {
        let should_sleep = now_ms.saturating_sub(self.last_activity_ms) >= timeout_ms;
        if should_sleep && !self.asleep {
            self.asleep = true;
            self.page_override = None;
            return Some(true);
        }
        None
    }

    /// Whether the screen is off.
    pub fn asleep(&self) -> bool {
        self.asleep
    }

    /// The page to show: the one the user swiped to, else morning or tonight by time of day.
    pub fn page(&self) -> i32 {
        self.page_override.unwrap_or(match self.now.map(clock::view_at) {
            Some(View::Morning) => PAGE_MORNING,
            _ => PAGE_TONIGHT,
        })
    }

    /// Change the brightness (10–100 %). Returns the new settings to apply and store.
    pub fn set_brightness(&mut self, percent: i32) -> Settings {
        self.settings.brightness = percent.clamp(10, 100) as u8;
        self.settings = self.settings.sanitized();
        self.settings
    }

    /// Toggle sleeping without the sleeping bag. Returns the new settings to store.
    pub fn toggle_bag(&mut self) -> Settings {
        self.settings.no_bag = !self.settings.no_bag;
        self.settings
    }

    /// Change the thermal sensitivity (−2..=2). Returns the new settings to store.
    pub fn set_sensitivity(&mut self, step: i32) -> Settings {
        self.settings.sensitivity = step.clamp(-2, 2) as i8;
        self.settings
    }

    /// Hook up the Rust implementations of `TextOps`. Call once after creating the UI.
    pub fn install_text_ops(ui: &NurseryScreen) {
        use slint::ComponentHandle;
        ui.global::<TextOps>().on_drop_last(|text| {
            let mut s = String::from(text.as_str());
            s.pop();
            s.into()
        });
        ui.global::<TextOps>().on_mask(|text| "•".repeat(text.chars().count()).into());
    }

    /// Push the current state into the UI.
    pub fn update_ui(&self, ui: &NurseryScreen) {
        ui.set_clock(match self.now {
            Some(now) => format!("{:02}:{:02}", now.hour(), now.minute()).into(),
            None => "--:--".into(),
        });
        ui.set_page(self.page());
        ui.set_asleep(self.asleep);
        if self.asleep {
            ui.set_settings_panel(0);
            ui.set_clock_editor_open(false);
        }
        // When the clock is unset, assume a typical evening for the plan.
        let now = self.now.unwrap_or(Minutes(clock::DAY + Minutes::hm(19, 0).0));
        // Only hand Slint what changed: a new model, even with the same rows, redraws its whole
        // chart, and on the Presto every redraw competes with scan-out for the QSPI bus.
        let (old_morning, old_evening, old_settings) = (ui.get_morning(), ui.get_evening(), ui.get_settings());
        let mut morning = self.morning(now);
        keep_model(&mut morning.temp_bars, &old_morning.temp_bars);
        keep_model(&mut morning.air_bars, &old_morning.air_bars);
        if morning != old_morning {
            ui.set_morning(morning);
        }
        let mut evening = self.evening(now);
        keep_model(&mut evening.plan_bars, &old_evening.plan_bars);
        let mut settings = self.settings_state(&evening.recommendation, &evening.dress_for);
        keep_model(&mut settings.networks, &old_settings.networks);
        keep_model(&mut settings.about, &old_settings.about);
        if settings != old_settings {
            ui.set_settings(settings);
        }
        if evening != old_evening {
            ui.set_evening(evening);
        }
    }

    fn settings_state(&self, tonight: &crate::Recommendation, dress_for: &str) -> SettingsState {
        let s = &self.settings;
        let (status, summary): (String, String) = match &self.wifi.link {
            WifiLink::Off => ("Radio off".into(), "Off".into()),
            WifiLink::Starting => ("Starting the radio…".into(), "Starting…".into()),
            WifiLink::Idle => ("Not connected".into(), "Not connected".into()),
            WifiLink::Connecting(ssid) => (format!("Connecting to {ssid}…"), "Connecting…".into()),
            WifiLink::Connected { ssid, ip } => (format!("Connected to {ssid} · {ip}"), ssid.clone()),
            WifiLink::Failed(why) => (why.clone(), "Not connected".into()),
        };
        let connected = match &self.wifi.link {
            WifiLink::Connected { ssid, .. } => Some(ssid.as_str()),
            _ => None,
        };
        let networks: Vec<WifiNetwork> = self
            .wifi
            .networks
            .iter()
            .map(|n| WifiNetwork {
                ssid: n.ssid.as_str().into(),
                bars: match n.rssi {
                    r if r >= -55 => 3,
                    r if r >= -67 => 2,
                    r if r >= -78 => 1,
                    _ => 0,
                },
                secure: n.secure,
                connected: connected == Some(n.ssid.as_str()),
            })
            .collect();
        let about: Vec<AboutRow> = self
            .about
            .iter()
            .map(|(label, value)| AboutRow { label: label.as_str().into(), value: value.as_str().into() })
            .collect();
        SettingsState {
            brightness: s.brightness as i32,
            sensitivity: s.sensitivity as i32,
            sensitivity_label: sleepwear::sensitivity_label(s.sensitivity).into(),
            sensitivity_explanation: sleepwear::sensitivity_explanation(s.sensitivity).into(),
            sensitivity_preview: if tonight.valid {
                let bag = if tonight.bag_used { format!("{} TOG sleeping bag", tonight.bag_tog) } else { "No sleeping bag".into() };
                format!("{bag} + {} ({})", tonight.outfit_label.to_lowercase(), dress_for.to_lowercase()).into()
            } else {
                "A room temperature is needed for a recommendation.".into()
            },
            wifi_enabled: s.wifi_enabled,
            wifi_status: status.into(),
            wifi_summary: summary.into(),
            wifi_busy: self.wifi.scanning || matches!(self.wifi.link, WifiLink::Starting | WifiLink::Connecting(_)),
            networks: ModelRc::new(VecModel::from(networks)),
            version: self.version.as_str().into(),
            about: ModelRc::new(VecModel::from(about)),
        }
    }

    fn morning(&self, now: Minutes) -> Morning {
        let (from, to) = clock::last_night(now);
        let (rain_level, rain_title, rain_detail) =
            self.rain(Minutes(to.0), Minutes(to.0 + 13 * 60), "today", "07:00 – 20:00");
        let Some(night) = self.history.summarize(from, to) else {
            let note = if self.now.is_none() {
                "Set the clock (tap the time) so the night can be recorded."
            } else {
                "No readings from last night yet. The summary appears once the display has been running overnight."
            };
            return Morning {
                has_data: false,
                note: note.into(),
                rain_level,
                rain_title,
                rain_detail,
                ..Default::default()
            };
        };
        let coolest = night.celsius.min;
        let recommendation = coolest
            .map(|(t, _)| adapt(sleepwear::recommend_for(t, self.settings.sensitivity), self.settings.no_bag));
        Morning {
            has_data: true,
            note: SharedString::new(),
            temp_range: match (night.celsius.min, night.celsius.max) {
                (Some((lo, _)), Some((hi, _))) => format!("{lo:.1}° – {hi:.1}°").into(),
                _ => "No temperature sensor".into(),
            },
            temp_detail: match (night.celsius.mean(), coolest) {
                (Some(mean), Some((_, at))) => {
                    format!("avg {mean:.1}° · coolest {:02}:{:02}", at.hour(), at.minute()).into()
                }
                _ => SharedString::new(),
            },
            temp_bars: temperature_bars(night.hourly.iter().map(|slot| slot.celsius)),
            air_label: air::overall(night.air_quality(), night.co2_level()).map_or("No air sensor", |(l, _)| l).into(),
            air_level: air::overall(night.air_quality(), night.co2_level()).map_or(0, |(_, level)| level),
            air_detail: air_detail(&night),
            co2_detail: co2_detail(&night),
            // CO₂ shows how the night went (it climbs while the room is shut); PM2.5 is usually
            // flat, so it is charted only without a CO₂ sensor.
            air_bars: if night.co2.mean().is_some() {
                co2_bars(night.hourly.iter().map(|slot| slot.co2))
            } else {
                air_bars(night.hourly.iter().map(|slot| slot.pm25))
            },
            called_for: match coolest {
                Some((t, _)) => format!("The coolest part ({t:.1}°) called for").into(),
                None => SharedString::new(),
            },
            recommendation: recommendation.map(ui_recommendation).unwrap_or_else(|| {
                missing_recommendation("No temperature readings last night, so no clothing summary.")
            }),
            rain_level,
            rain_title,
            rain_detail,
        }
    }

    fn evening(&self, now: Minutes) -> Evening {
        let (rain_level, rain_title, rain_detail) = {
            let (_, end) = clock::tonight(now);
            self.rain(end, Minutes(end.0 + 5 * 60), "tomorrow morning", "07:00 – 12:00")
        };
        let (air_label, air_level) = air::overall(
            self.latest.pm25.map(AirQuality::from_pm25),
            self.latest.co2.map(Co2Level::from_ppm),
        )
        .map_or(("No air sensor".into(), 0), |(label, level)| (label.into(), level));
        let air_detail = match (self.latest.pm25, self.latest.co2) {
            (Some(pm), Some(co2)) => format!("PM {pm:.0} · CO₂ {co2:.0} ppm"),
            (Some(pm), None) => format!("PM2.5 {pm:.0} µg/m³"),
            (None, Some(co2)) => format!("CO₂ {co2:.0} ppm"),
            (None, None) => String::new(),
        }
        .into();
        let Some(room) = self.latest.celsius else {
            return Evening {
                room_now: "--°".into(),
                room_comfort: -1,
                humidity: "No sensor".into(),
                air_label,
                air_level,
                air_detail,
                expected_low: "--".into(),
                expected_detail: "Needs a room temperature".into(),
                recommendation: missing_recommendation(
                    "Connect the SCD40 sensor to the Qwiic port to get a clothing recommendation.",
                ),
                rain_level,
                rain_title,
                rain_detail,
                ..Default::default()
            };
        };
        let plan = planner::plan(now, room, &self.history, &self.forecast, self.settings.sensitivity);
        Evening {
            room_now: format!("{room:.1}°").into(),
            room_comfort: Comfort::from_room(room).index(),
            humidity: match self.latest.humidity {
                Some(h) => format!("{h:.0}% humidity").into(),
                None => SharedString::new(),
            },
            air_label,
            air_level,
            air_detail,
            expected_low: format!("{:.1}°", plan.expected_low).into(),
            expected_detail: plan_detail(&plan),
            outdoor_note: outdoor_note(&plan),
            plan_bars: temperature_bars(plan.hourly.iter().map(|&t| Some(t))),
            dress_for: format!("Dress for {:.1}°", plan.expected_low).into(),
            recommendation: ui_recommendation(adapt(plan.recommendation, self.settings.no_bag)),
            rain_level,
            rain_title,
            rain_detail,
        }
    }

    fn rain(&self, from: Minutes, to: Minutes, when: &str, window: &str) -> (i32, SharedString, SharedString) {
        match self.forecast.rain_between(from, to) {
            None => (-1, "Rain forecast unavailable".into(), self.no_forecast_reason.into()),
            Some(o) => {
                let title = match (o.level(), o.likely_from) {
                    (2, Some(start)) => format!("Rain likely {when} from {:02}:00", start.hour()),
                    (1, _) => format!("Showers possible {when}"),
                    _ => format!("Dry {when}"),
                };
                let detail = match o.likely_until {
                    Some(end) if o.level() == 2 => {
                        format!("{}% chance until {:02}:00 · bring the rain cover", o.max_chance, end.hour())
                    }
                    _ => format!("Up to {}% chance, {window}", o.max_chance),
                };
                (o.level(), title.into(), detail.into())
            }
        }
    }
}

fn air_detail(night: &NightSummary) -> SharedString {
    match (night.pm25.mean(), night.pm25.max) {
        (Some(mean), Some((max, at))) => {
            let mut s = format!("PM2.5 avg {mean:.0} · max {max:.0}");
            if night.worst_air_quality() > night.air_quality() {
                s += &format!(" at {:02}:{:02}", at.hour(), at.minute());
            }
            s.into()
        }
        _ => SharedString::new(),
    }
}

fn co2_detail(night: &NightSummary) -> SharedString {
    match (night.co2.mean(), night.co2.max) {
        // No peak time: it is nearly always just before the room is aired in the morning.
        (Some(mean), Some((max, _))) => format!("CO₂ avg {mean:.0} · max {max:.0} ppm").into(),
        _ => SharedString::new(),
    }
}

fn plan_detail(plan: &Plan) -> SharedString {
    let at = plan.expected_low_at.hour();
    match plan.basis {
        Basis::LastNight { offset } if offset.abs() >= 0.05 => format!("around {at:02}:00 · last night {offset:+.1}°"),
        Basis::LastNight { .. } => format!("around {at:02}:00 · like last night"),
        Basis::Estimate => format!("around {at:02}:00 · estimate"),
    }
    .into()
}

/// How tonight compares outside, next to the expected low.
fn outdoor_note(plan: &Plan) -> SharedString {
    match (plan.outdoor_low, plan.outdoor_change) {
        (Some(_), Some(change)) if change.abs() >= 1.0 => {
            format!("{:.0}° {} outside", change.abs(), if change < 0.0 { "colder" } else { "warmer" }).into()
        }
        (Some(outside), _) => format!("{outside:.0}° outside").into(),
        _ => SharedString::new(),
    }
}

/// Apply the user's no-sleeping-bag choice.
fn adapt(r: Recommendation, no_bag: bool) -> Recommendation {
    if no_bag { r.without_bag() } else { r }
}

fn missing_recommendation(why: &str) -> crate::Recommendation {
    crate::Recommendation { valid: false, missing: why.into(), ..Default::default() }
}

fn ui_recommendation(r: Recommendation) -> crate::Recommendation {
    crate::Recommendation {
        valid: true,
        missing: SharedString::new(),
        outfit: r.outfit.index(),
        bag_used: r.bag_used,
        outfit_label: r.outfit.label().into(),
        bag_tog: tog(r.bag_tog).into(),
        bag_index: r.bag_index(),
        total: format!("≈ {} TOG in total", tog(r.total_tog())).into(),
        advice: r.advice().into(),
        // The advice is coloured by comfort; "too cool without a bag" is a cold warning.
        comfort: if r.short_by > 0.25 { Comfort::Cold.index() } else { r.comfort.index() },
    }
}

/// Replace `new` with `old` when they hold the same rows, so the struct compares equal.
fn keep_model<T: Clone + PartialEq + 'static>(new: &mut ModelRc<T>, old: &ModelRc<T>) {
    if new.row_count() == old.row_count() && new.iter().zip(old.iter()).all(|(a, b)| a == b) {
        *new = old.clone();
    }
}

fn tog(value: f32) -> String {
    let tenths = libm::roundf(value * 10.0) as i32;
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// Bars scaled to a fixed 14–28 °C range so nights can be compared at a glance.
fn temperature_bars(values: impl Iterator<Item = Option<f32>>) -> ModelRc<Bar> {
    let bars: Vec<Bar> = values
        .map(|v| match v {
            Some(t) => Bar {
                level: ((t - 14.0) / 14.0).clamp(0.05, 1.0),
                tone: Comfort::from_room(t).index(),
                empty: false,
            },
            None => Bar { level: 0.0, tone: 0, empty: true },
        })
        .collect();
    ModelRc::new(VecModel::from(bars))
}

/// Bars on a square-root scale up to 75 µg/m³, so low values are still visible.
fn air_bars(values: impl Iterator<Item = Option<f32>>) -> ModelRc<Bar> {
    let bars: Vec<Bar> = values
        .map(|v| match v {
            Some(pm) => Bar {
                level: libm::sqrtf(pm.max(0.0) / 75.0).clamp(0.05, 1.0),
                tone: AirQuality::from_pm25(pm).level(),
                empty: false,
            },
            None => Bar { level: 0.0, tone: 0, empty: true },
        })
        .collect();
    ModelRc::new(VecModel::from(bars))
}

/// Bars from outdoor air (400 ppm) to 2000 ppm, coloured by ventilation category.
fn co2_bars(values: impl Iterator<Item = Option<f32>>) -> ModelRc<Bar> {
    let bars: Vec<Bar> = values
        .map(|v| match v {
            Some(ppm) => Bar {
                level: ((ppm - 400.0) / 1600.0).clamp(0.05, 1.0),
                tone: Co2Level::from_ppm(ppm).level(),
                empty: false,
            },
            None => Bar { level: 0.0, tone: 0, empty: true },
        })
        .collect();
    ModelRc::new(VecModel::from(bars))
}

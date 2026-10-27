//! Desktop simulator for the Presto nursery display.
//!
//! ```text
//! cargo run                                   # screen + controls windows
//! cargo run -- cold                           # start from a preset
//! cargo run -- --screenshot out.png morning   # render a preset headlessly, like the device
//! cargo run --features mcp                    # with SLINT_MCP_PORT=9315: inspect over MCP
//! ```
//!
//! The screen is the same `nursery.slint` and the same `app.rs` logic as the firmware; only the
//! sensor readings, history, forecast and Wi-Fi are simulated. The forecast is only "fetched"
//! while the simulated Wi-Fi is connected, as on the device.

extern crate alloc;

/// Counts heap usage, to size the device's heap (set NURSERY_HEAP_STATS=1).
mod heap_stats {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};
    pub static CURRENT: AtomicUsize = AtomicUsize::new(0);
    pub static PEAK: AtomicUsize = AtomicUsize::new(0);
    pub struct Counting;
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let now = CURRENT.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
            unsafe { System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            CURRENT.fetch_sub(l.size(), Ordering::Relaxed);
            unsafe { System.dealloc(p, l) }
        }
    }
    #[global_allocator]
    static A: Counting = Counting;
}

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use nursery_core::clock::{DAY, Minutes};
use nursery_core::forecast::{Forecast, Hour};
use nursery_core::history::{History, Reading};
use nursery_core::settings::{InlineStr, Settings};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::{ComponentHandle, Timer, TimerMode};

slint::include_modules!();

// Some helpers are only used by the firmware (sensor recording, clock setting).
#[allow(dead_code)]
#[path = "../../nursery/app.rs"]
mod app;

use app::{App, ScannedNetwork, WifiLink};

/// Day number used for "today" in the simulation (any value > 1 works).
const TODAY: u32 = 10;

/// What a preset shows besides the scenario.
#[derive(Clone, Copy, Default)]
struct View {
    page: Option<i32>,
    panel: i32,
    no_bodysuit: bool,
    wifi_off: bool,
}

fn base_scenario() -> Scenario {
    Scenario {
        time: 20.0 * 60.0,
        room: 21.5,
        humidity: 48.0,
        pm25: 4.0,
        night_start: 21.0,
        night_drop: 2.5,
        night_pm: 5.0,
        night_spike: false,
        has_forecast: true,
        outdoor_low: 6.0,
        rain_chance: 70.0,
        rain_hour: 14.0,
        has_temp: true,
        has_pm: true,
        timeout: 180.0,
    }
}

const PRESETS: &[&str] = &[
    "evening", "morning", "cold", "hot", "smoky", "no-sensors", "no-wifi", "no-bodysuit", "settings", "wifi",
    "sensitivity", "brightness", "about",
];

fn preset(name: &str) -> Option<(Scenario, View)> {
    let base = base_scenario();
    let settings = |panel| View { page: Some(2), panel, ..Default::default() };
    Some(match name {
        "evening" => (base, View::default()),
        "morning" => (Scenario { time: 6.0 * 60.0 + 40.0, room: 19.0, ..base }, View::default()),
        "cold" => (
            Scenario { room: 16.5, outdoor_low: -6.0, night_start: 17.0, night_drop: 3.5, rain_chance: 10.0, ..base },
            View::default(),
        ),
        "hot" => (
            Scenario { room: 27.5, humidity: 62.0, outdoor_low: 22.0, night_start: 28.0, night_drop: 1.0, rain_chance: 30.0, ..base },
            View::default(),
        ),
        "smoky" => (Scenario { time: 7.0 * 60.0, pm25: 38.0, night_pm: 22.0, night_spike: true, ..base }, View::default()),
        "no-sensors" => (Scenario { has_temp: false, has_pm: false, has_forecast: false, ..base }, View::default()),
        "no-wifi" => (base, View { wifi_off: true, ..Default::default() }),
        "no-bodysuit" => (base, View { no_bodysuit: true, ..Default::default() }),
        "settings" => (base, settings(0)),
        "wifi" => (base, settings(1)),
        "sensitivity" => (base, settings(2)),
        "brightness" => (base, settings(3)),
        "about" => (base, settings(4)),
        _ => return None,
    })
}

/// Room temperature `minutes_after_19` minutes into a night that starts at `start` and cools by
/// `drop` until 05:00, then warms slightly.
fn night_curve(start: f32, drop: f32, minutes_after_19: f32) -> f32 {
    let coldest = 10.0 * 60.0;
    if minutes_after_19 <= coldest {
        start - drop * (minutes_after_19 / coldest).powf(0.8)
    } else {
        start - drop + 0.3 * (minutes_after_19 - coldest) / 60.0
    }
}

/// Outdoor temperature at `t`: a daily sine with its low (`low`) at 05:00.
fn outdoor_at(low: f32, t: Minutes) -> f32 {
    let phase = (t.of_day() as f32 / 60.0 - 5.0) / 24.0 * std::f32::consts::TAU;
    low + 5.0 * (1.0 - phase.cos())
}

/// Fill `app` with readings, history and forecast for a scenario.
fn apply(app: &mut App, s: &Scenario) {
    let now = Minutes(TODAY * DAY + s.time as u32);
    app.now = Some(now);
    let online = matches!(app.wifi.link, WifiLink::Connected { .. });

    // Synthetic history: the last 30 hours, sampled every 5 minutes. Last night's outdoor low
    // is the forecast's low plus 2 °C, so tonight is a little colder than last night.
    let mut history = History::default();
    let mut noise = 0x1234_5678u32;
    let mut rand = move || {
        noise ^= noise << 13;
        noise ^= noise >> 17;
        noise ^= noise << 5;
        (noise % 1000) as f32 / 1000.0 - 0.5
    };
    for m in (now.0.saturating_sub(30 * 60)..now.0).step_by(5) {
        let t = Minutes(m);
        let since_19 = (t.of_day() + DAY - 19 * 60) % DAY;
        let celsius = if since_19 < 12 * 60 {
            night_curve(s.night_start, s.night_drop, since_19 as f32)
        } else {
            // Daytime: back towards the evening temperature.
            s.night_start - s.night_drop * 0.3
        } + 0.1 * rand();
        let spike = s.night_spike && (23 * 60..23 * 60 + 40).contains(&t.of_day());
        let pm = (s.night_pm * (1.0 + 0.4 * rand()) + if spike { 60.0 } else { 0.0 }).max(0.0);
        history.record(
            t,
            Reading {
                celsius: s.has_temp.then_some(celsius),
                humidity: s.has_temp.then_some(s.humidity + 3.0 * rand()),
                pm25: s.has_pm.then_some(pm),
                outdoor: (s.has_forecast && online).then_some(outdoor_at(s.outdoor_low + 2.0, t)),
            },
        );
    }
    app.history = history;
    app.latest = Reading {
        celsius: s.has_temp.then_some(s.room),
        humidity: s.has_temp.then_some(s.humidity),
        pm25: s.has_pm.then_some(s.pm25),
        outdoor: None,
    };

    // Synthetic forecast: 36 hours from the start of the current hour; rain lasts three hours
    // from `rain_hour`.
    app.forecast = if s.has_forecast && online {
        let first = now.0 - now.0 % 60;
        Forecast::from_hours((0..36).map(|i| {
            let start = Minutes(first + 60 * i);
            let raining = (0..3).any(|k| (s.rain_hour as u32 + k) % 24 == start.hour());
            Hour {
                start,
                celsius: outdoor_at(s.outdoor_low, start),
                rain_chance: if raining { s.rain_chance as u8 } else { (s.rain_chance * 0.15) as u8 },
            }
        }))
    } else {
        Forecast::default()
    };
    app.no_forecast_reason = match (&app.wifi.link, s.has_forecast) {
        (WifiLink::Off, _) => "No forecast: Wi-Fi is off",
        (WifiLink::Connected { .. }, false) => "Forecast service unreachable",
        _ => "No forecast: Wi-Fi not connected",
    };
}

/// Networks the simulated radio "finds".
fn fake_scan() -> Vec<ScannedNetwork> {
    [("Nursery Home", -48, true), ("PrinterDirect-AB", -63, true), ("Neighbours 5G", -71, true), ("CoffeeShop Guest", -80, false)]
        .into_iter()
        .map(|(ssid, rssi, secure)| ScannedNetwork { ssid: ssid.into(), rssi, secure })
        .collect()
}

fn about_rows() -> Vec<(String, String)> {
    vec![
        ("Firmware".into(), version()),
        ("Running on".into(), "Desktop simulator".into()),
        ("Device".into(), "Pimoroni Presto · RP2350B".into()),
        ("Display".into(), "480×480 ST7701 · 63 Hz".into()),
        ("Sensors".into(), "PMSA003I · SHT4x (simulated)".into()),
        ("Wi-Fi MAC".into(), "28:cd:c1:00:00:00 (simulated)".into()),
        ("UI".into(), "Slint 1.18".into()),
    ]
}

fn version() -> String {
    format!("v{} ({})", env!("CARGO_PKG_VERSION"), env!("NURSERY_GIT"))
}

/// Render `scenario` with the software renderer into an RGB565 buffer (exactly what the Presto
/// shows) and save it as a PNG.
fn screenshot(scenario: &Scenario, view: View, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    struct Headless(Rc<MinimalSoftwareWindow>, Instant);
    impl slint::platform::Platform for Headless {
        fn create_window_adapter(&self) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
        fn duration_since_start(&self) -> Duration {
            self.1.elapsed()
        }
    }
    slint::platform::set_platform(Box::new(Headless(window.clone(), Instant::now())))
        .map_err(|e| format!("{e:?}"))?;
    let ui = NurseryScreen::new()?;
    App::install_text_ops(&ui);
    let mut app = App::new(0);
    app.version = version();
    app.about = about_rows();
    app.settings.no_bodysuit = view.no_bodysuit;
    if view.wifi_off {
        app.wifi.link = WifiLink::Off;
    } else {
        app.settings.wifi_enabled = true;
        app.wifi.link = WifiLink::Connected { ssid: "Nursery Home".into(), ip: "192.168.1.42".into() };
        app.wifi.networks = fake_scan();
    }
    app.page_override = view.page;
    apply(&mut app, scenario);
    app.update_ui(&ui);
    ui.set_settings_panel(view.panel);
    window.set_size(slint::PhysicalSize::new(480, 480));
    ui.show()?;
    // Let the page strip settle where it should be (it animates when the page changes).
    let mut pixels = vec![Rgb565Pixel(0); 480 * 480];
    std::thread::sleep(Duration::from_millis(300));
    slint::platform::update_timers_and_animations();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, 480);
    });
    save_png(path, 480, 480, pixels.iter().map(|p| {
        let v = p.0;
        let (r, g, b) = ((v >> 11) as u8, ((v >> 5) & 0x3f) as u8, (v & 0x1f) as u8);
        [r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2, 255]
    }))
}

fn save_png(path: &Path, width: u32, height: u32, pixels: impl Iterator<Item = [u8; 4]>) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let data: Vec<u8> = pixels.flatten().collect();
    encoder.write_header()?.write_image_data(&data)?;
    Ok(())
}

/// Run `f` once after `ms` milliseconds.
fn after(ms: u64, f: impl FnOnce() + 'static) {
    Timer::single_shot(Duration::from_millis(ms), f);
}

fn interactive(initial: Scenario, view: View) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(not(feature = "mcp"))]
    slint::BackendSelector::new().renderer_name("software".into()).select()?;

    let ui = NurseryScreen::new()?;
    App::install_text_ops(&ui);
    let controls = SimControls::new()?;
    controls.set_scenario(initial);
    ui.set_simulate_brightness(true);
    let start = Instant::now();
    let now_ms = move || start.elapsed().as_millis() as u64;
    let app = Rc::new(RefCell::new(App::new(now_ms())));
    {
        let mut app = app.borrow_mut();
        app.version = version();
        app.about = about_rows();
        app.page_override = view.page;
        app.settings.no_bodysuit = view.no_bodysuit;
        if !view.wifi_off {
            // Start as if the network had been set up before.
            app.settings.wifi_enabled = true;
            app.settings.ssid = InlineStr::from_str_truncated("Nursery Home");
            app.settings.password = InlineStr::from_str_truncated("lullaby123");
            app.wifi.link = WifiLink::Connected { ssid: "Nursery Home".into(), ip: "192.168.1.42".into() };
        }
    }

    let refresh = {
        let (ui, controls, app) = (ui.as_weak(), controls.as_weak(), app.clone());
        move || {
            let (Some(ui), Some(controls)) = (ui.upgrade(), controls.upgrade()) else { return };
            let mut app = app.borrow_mut();
            apply(&mut app, &controls.get_scenario());
            app.update_ui(&ui);
        }
    };
    refresh();
    ui.set_settings_panel(view.panel);

    controls.on_changed({
        let refresh = refresh.clone();
        move || refresh()
    });
    controls.on_touch_screen({
        let (app, refresh) = (app.clone(), refresh.clone());
        move || {
            app.borrow_mut().touch(now_ms());
            refresh();
        }
    });
    controls.on_screenshot({
        let ui = ui.as_weak();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            match ui.window().take_snapshot() {
                Ok(img) => {
                    let path = Path::new("nursery-screenshot.png");
                    let pixels = img.as_slice().iter().map(|p| [p.r, p.g, p.b, p.a]);
                    match save_png(path, img.width(), img.height(), pixels) {
                        Ok(()) => println!("saved {}", path.display()),
                        Err(e) => eprintln!("screenshot failed: {e}"),
                    }
                }
                Err(e) => eprintln!("screenshot failed: {e}"),
            }
        }
    });
    ui.on_activity({
        let app = app.clone();
        move || {
            app.borrow_mut().touch(now_ms());
        }
    });
    ui.on_wake({
        let (app, refresh) = (app.clone(), refresh.clone());
        move || {
            app.borrow_mut().touch(now_ms());
            refresh();
        }
    });
    ui.on_page_changed({
        let app = app.clone();
        move |page| {
            app.borrow_mut().page_override = Some(page);
        }
    });
    ui.on_adjust_clock({
        let (controls, refresh) = (controls.as_weak(), refresh.clone());
        move |minutes| {
            let Some(controls) = controls.upgrade() else { return };
            let mut s = controls.get_scenario();
            s.time = (s.time + minutes as f32).rem_euclid(DAY as f32);
            controls.set_scenario(s);
            refresh();
        }
    });
    ui.on_toggle_bodysuit({
        let (app, refresh) = (app.clone(), refresh.clone());
        move || {
            let settings = app.borrow_mut().toggle_bodysuit();
            println!("settings: no bodysuit = {}", settings.no_bodysuit);
            refresh();
        }
    });
    ui.on_set_sensitivity({
        let (app, refresh) = (app.clone(), refresh.clone());
        move |step| {
            app.borrow_mut().set_sensitivity(step);
            refresh();
        }
    });
    ui.on_set_brightness({
        let (app, refresh) = (app.clone(), refresh.clone());
        move |level| {
            app.borrow_mut().set_brightness(level);
            refresh();
        }
    });

    // Simulated radio: delays roughly like the real CYW43439.
    let scan = {
        let (app, refresh) = (app.clone(), refresh.clone());
        move || {
            {
                let mut app = app.borrow_mut();
                if !app.settings.wifi_enabled || app.wifi.scanning {
                    return;
                }
                app.wifi.scanning = true;
            }
            refresh();
            let (app, refresh) = (app.clone(), refresh.clone());
            after(1500, move || {
                {
                    let mut app = app.borrow_mut();
                    app.wifi.scanning = false;
                    if app.settings.wifi_enabled {
                        app.wifi.networks = fake_scan();
                    }
                }
                refresh();
            });
        }
    };
    let join = {
        let (app, refresh) = (app.clone(), refresh.clone());
        move |ssid: String, password: String| {
            app.borrow_mut().wifi.link = WifiLink::Connecting(ssid.clone());
            refresh();
            let (app, refresh) = (app.clone(), refresh.clone());
            after(1500, move || {
                {
                    let mut app = app.borrow_mut();
                    let secure = app.wifi.networks.iter().find(|n| n.ssid == ssid).is_none_or(|n| n.secure);
                    if secure && password.len() < 8 {
                        app.wifi.link = WifiLink::Failed(format!("Could not join {ssid}: wrong password?"));
                    } else {
                        app.wifi.link = WifiLink::Connected { ssid: ssid.clone(), ip: "192.168.1.42".into() };
                        app.settings.ssid = InlineStr::from_str_truncated(&ssid);
                        app.settings.password = InlineStr::from_str_truncated(&password);
                        println!("settings: joined {ssid}");
                    }
                }
                refresh();
            });
        }
    };
    ui.on_wifi_scan({
        let scan = scan.clone();
        move || scan()
    });
    ui.on_wifi_join({
        let join = join.clone();
        move |ssid, password| join(ssid.into(), password.into())
    });
    ui.on_set_wifi_enabled({
        let (app, refresh, scan, join) = (app.clone(), refresh.clone(), scan.clone(), join.clone());
        move |on| {
            {
                let mut app = app.borrow_mut();
                app.settings.wifi_enabled = on;
                app.wifi.link = if on { WifiLink::Starting } else { WifiLink::Off };
                app.wifi.networks.clear();
                app.wifi.scanning = false;
            }
            println!("settings: Wi-Fi {}", if on { "on" } else { "off (radio unpowered)" });
            refresh();
            if on {
                let (app, refresh, scan, join) = (app.clone(), refresh.clone(), scan.clone(), join.clone());
                after(800, move || {
                    let saved: Option<Settings> = {
                        let mut app = app.borrow_mut();
                        if !app.settings.wifi_enabled {
                            return;
                        }
                        app.wifi.link = WifiLink::Idle;
                        Some(app.settings)
                    };
                    refresh();
                    scan();
                    // Rejoin the saved network, as the device does at boot.
                    if let Some(saved) = saved.filter(|s| !s.ssid.is_empty()) {
                        join(saved.ssid.as_str().into(), saved.password.as_str().into());
                    }
                });
            }
        }
    });

    let sleep_timer = Timer::default();
    sleep_timer.start(TimerMode::Repeated, Duration::from_millis(250), {
        let (app, controls, refresh) = (app.clone(), controls.as_weak(), refresh.clone());
        move || {
            let Some(controls) = controls.upgrade() else { return };
            let timeout = (controls.get_scenario().timeout * 1000.0) as u64;
            if app.borrow_mut().poll_sleep(now_ms(), timeout).is_some() {
                refresh();
            }
        }
    });

    controls.show()?;
    ui.show()?;
    slint::run_event_loop()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let lookup = |name: &str| preset(name).ok_or_else(|| format!("unknown preset {name}; try one of {PRESETS:?}"));
    match args.as_slice() {
        [flag, path, rest @ ..] if flag == "--screenshot" => {
            let name = rest.first().map(String::as_str).unwrap_or("evening");
            let (scenario, view) = lookup(name)?;
            let before = heap_stats::CURRENT.load(std::sync::atomic::Ordering::Relaxed);
            screenshot(&scenario, view, Path::new(path))?;
            println!("wrote {path} ({name})");
            if std::env::var_os("NURSERY_HEAP_STATS").is_some() {
                use std::sync::atomic::Ordering::Relaxed;
                println!(
                    "heap: {} KiB in use, peak {} KiB (process baseline {} KiB)",
                    heap_stats::CURRENT.load(Relaxed) / 1024,
                    heap_stats::PEAK.load(Relaxed) / 1024,
                    before / 1024
                );
            }
            Ok(())
        }
        [name] if !name.starts_with('-') => {
            let (scenario, view) = lookup(name)?;
            interactive(scenario, view)
        }
        [] => {
            let (scenario, view) = lookup("evening")?;
            interactive(scenario, view)
        }
        _ => Err(format!("usage: nursery-desktop [PRESET] | --screenshot FILE [PRESET]; presets: {PRESETS:?}").into()),
    }
}

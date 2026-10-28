//! Nursery display: overnight room summary in the morning, sleepwear plan in the evening.
//!
//! Sensors on the Qwiic connector: a PMSA003I (PM2.5) and an SCD40 (CO₂, temperature and
//! humidity, at 0x62). The PMSA003I has no temperature sensor of its own. Either sensor may be
//! missing; the screen says what it cannot show.
//!
//! Swipe left/right between three pages: last night (morning), tonight's plan, and settings
//! (Wi-Fi, thermal sensitivity, brightness, about). Tap the sleeping bag to dress for a night without it.
//!
//! The screen turns off (backlight off) after a few minutes without a touch; touch it or press
//! the user button to wake it. With Wi-Fi on, the clock is set and the forecast fetched over the
//! network; otherwise tap the time to set it, and tonight's plan comes from last night's
//! temperatures. Settings are kept in the last flash sector.
//!
//! The UI is `examples/nursery/ui/nursery.slint`; try it on the desktop first with
//! `cargo run` in `examples/nursery-desktop`.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use core::cell::RefCell;

use defmt::{info, warn};
use embassy_executor::Spawner;
use embassy_futures::select::{Either4, select4};
use embassy_rp::gpio::Input;
use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::I2C0;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use embedded_hal_async::i2c::I2c as _;
use nursery_core::clock::Minutes;
use nursery_core::history::Reading;
use nursery_core::sensors;
use presto_bsp::lcd::{DISPLAY_WIDTH, FrameBuffer, Rgb565Pixel};
use presto_bsp::{BoardTouch, Presto};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use {defmt_rtt as _, panic_probe as _};

slint::include_modules!();

#[path = "../../../nursery/app.rs"]
mod app;
mod storage;
mod wifi;

use app::{App, SCREEN_TIMEOUT_MS, WifiLink};
use nursery_core::net;
use nursery_core::settings::{InlineStr, Settings};
use storage::Storage;

/// SRAM heap for Slint and the network stack.
const HEAP_SIZE: usize = 280 * 1024;
/// How often a reading goes into the history.
const RECORD_EVERY: Duration = Duration::from_secs(60);
/// The PMSA003I needs about 30 s after power-up before its readings settle.
const PM_WARMUP: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, defmt::Format)]
enum Touch {
    Down { x: u16, y: u16 },
    Up,
}

static TOUCHES: Channel<CriticalSectionRawMutex, Touch, 8> = Channel::new();
static READING: Signal<CriticalSectionRawMutex, Reading> = Signal::new();

/// Renders each line into SRAM, then hands it to the (PSRAM) frame buffer in paced bursts.
struct LineWriter<'a> {
    fb: &'a mut FrameBuffer,
    line: [Rgb565Pixel; DISPLAY_WIDTH],
}

impl slint::platform::software_renderer::LineBufferProvider for LineWriter<'_> {
    type TargetPixel = Rgb565Pixel;

    fn process_line(
        &mut self,
        line: usize,
        range: core::ops::Range<usize>,
        render_fn: impl FnOnce(&mut [Rgb565Pixel]),
    ) {
        let span = &mut self.line[range.clone()];
        render_fn(span);
        self.fb.write_span(line, range.start, span);
    }
}

struct PrestoPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl slint::platform::Platform for PrestoPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> core::time::Duration {
        core::time::Duration::from_micros(Instant::now().as_micros())
    }

    fn debug_log(&self, arguments: core::fmt::Arguments) {
        use alloc::string::ToString;
        info!("slint: {}", arguments.to_string().as_str());
    }
}

/// Forward touch-controller reports as press/release events.
#[embassy_executor::task]
async fn touch_task(mut touch: BoardTouch) {
    let mut pressed = false;
    loop {
        // While a finger is down, poll so a lost release interrupt cannot leave us "pressed".
        if pressed {
            let _ = embassy_futures::select::select(touch.wait_for_touch(), Timer::after_millis(40)).await;
        } else if touch.wait_for_touch().await.is_err() {
            continue;
        }
        let report = match touch.read_report().await {
            Ok(report) => report,
            Err(_) => {
                Timer::after_millis(20).await;
                continue;
            }
        };
        match report.points[0].filter(|p| p.active && report.touches > 0) {
            Some(p) => {
                pressed = true;
                let _ = TOUCHES.try_send(Touch::Down { x: p.x, y: p.y });
            }
            None if pressed => {
                pressed = false;
                let _ = TOUCHES.try_send(Touch::Up);
            }
            None => {}
        }
        Timer::after_millis(15).await;
    }
}

async fn read_pm(bus: &mut I2c<'static, I2C0, i2c::Async>) -> Option<f32> {
    let mut frame = [0u8; 32];
    bus.read(sensors::PMSA003I_ADDR, &mut frame).await.ok()?;
    match sensors::parse_pmsa003i(&frame) {
        Ok(r) => Some(r.pm25 as f32),
        Err(e) => {
            warn!("PMSA003I frame error: {}", defmt::Debug2Format(&e));
            None
        }
    }
}

/// Send an SCD4x command, wait for it, and read `B` bytes of response (`B` = 0: nothing).
async fn scd4x<const B: usize>(
    bus: &mut I2c<'static, I2C0, i2c::Async>,
    command: u16,
    wait_ms: u64,
) -> Result<[u8; B], ()> {
    let fail = |what: &str, e: &dyn core::fmt::Debug| warn!("SCD4x {:#06x} {}: {}", command, what, defmt::Debug2Format(e));
    if let Err(e) = bus.write(sensors::SCD4X_ADDR, &command.to_be_bytes()).await {
        fail("write", &e);
        return Err(());
    }
    Timer::after_millis(wait_ms).await;
    let mut frame = [0u8; B];
    if B > 0 {
        if let Err(e) = bus.read(sensors::SCD4X_ADDR, &mut frame).await {
            fail("read", &e);
            return Err(());
        }
    }
    Ok(frame)
}

/// Find an SCD4x and start periodic measurement (first result after 5 s).
async fn start_scd4x(bus: &mut I2c<'static, I2C0, i2c::Async>) -> bool {
    use sensors::scd4x;
    // It may still be measuring from before a reset of the Presto; it only reports its serial
    // number when idle.
    if scd4x::<0>(bus, scd4x::STOP_PERIODIC, scd4x::STOP_MS).await.is_err() {
        return false;
    }
    let Ok(frame) = scd4x::<9>(bus, scd4x::SERIAL, scd4x::READ_MS).await else { return false };
    let Ok(serial) = sensors::parse_words::<9, 3>(&frame) else {
        warn!("SCD4x serial frame {:02x}: bad CRC", frame);
        return false;
    };
    if scd4x::<0>(bus, scd4x::START_PERIODIC, 0).await.is_err() {
        return false;
    }
    info!("SCD4x serial {:04x}{:04x}{:04x}: measuring every 5 s", serial[0], serial[1], serial[2]);
    true
}

/// The latest SCD4x measurement, `Ok(None)` if none is ready yet, `Err` if the sensor is gone.
async fn read_scd4x(bus: &mut I2c<'static, I2C0, i2c::Async>) -> Result<Option<sensors::Co2Reading>, ()> {
    use sensors::scd4x;
    let frame = scd4x::<3>(bus, scd4x::DATA_READY, scd4x::READ_MS).await?;
    let Ok([status]) = sensors::parse_words::<3, 1>(&frame) else { return Ok(None) };
    if status & 0x07ff == 0 {
        return Ok(None);
    }
    let frame = scd4x::<9>(bus, scd4x::READ_MEASUREMENT, scd4x::READ_MS).await?;
    match sensors::parse_scd4x(&frame) {
        // 0 ppm is reported while the sensor is still settling.
        Ok(r) if r.co2 > 0 => Ok(Some(r)),
        Ok(_) => Ok(None),
        Err(_) => {
            warn!("SCD4x measurement frame {:02x}: bad CRC", frame);
            Ok(None)
        }
    }
}

/// Log every address that acknowledges a read on the Qwiic bus, then look for an SCD4x and start
/// it. Returns whether one was found.
async fn scan_bus(bus: &mut I2c<'static, I2C0, i2c::Async>) -> bool {
    info!("scanning Qwiic I2C");
    for address in 0x08u8..0x78 {
        let mut byte = [0u8; 1];
        let found = matches!(
            embassy_time::with_timeout(Duration::from_millis(20), bus.read(address, &mut byte)).await,
            Ok(Ok(()))
        );
        if found {
            info!("I2C device at {:#04x}", address);
        }
    }
    let scd = matches!(embassy_time::with_timeout(Duration::from_secs(1), start_scd4x(bus)).await, Ok(true));
    info!("scan done, SCD4x {}", if scd { "found" } else { "not found" });
    scd
}

/// Read both sensors every few seconds and publish a smoothed reading.
#[embassy_executor::task]
async fn sensor_task(mut bus: I2c<'static, I2C0, i2c::Async>) {
    let started = Instant::now();
    let mut pm_avg: Option<f32> = None;
    let mut scd = scan_bus(&mut bus).await;
    let mut rescan_at = Instant::now() + Duration::from_secs(30);
    // Last SCD4x measurement and when it arrived (it measures every 5 s, as often as this loop
    // runs, so a loop often finds nothing new).
    let mut climate: Option<(sensors::Co2Reading, Instant)> = None;
    let mut scd_errors = 0;
    loop {
        let pm = if started.elapsed() >= PM_WARMUP { read_pm(&mut bus).await } else { None };
        // Exponential smoothing over roughly the last 30 s.
        pm_avg = match (pm, pm_avg) {
            (Some(new), Some(old)) => Some(old + (new - old) * 0.15),
            (Some(new), None) => Some(new),
            (None, _) => None,
        };
        // A sensor plugged in later is found by the periodic rescan.
        if !scd && Instant::now() >= rescan_at {
            scd = scan_bus(&mut bus).await;
            rescan_at = Instant::now() + Duration::from_secs(30);
        }
        if scd {
            match read_scd4x(&mut bus).await {
                Ok(Some(r)) => {
                    scd_errors = 0;
                    if climate.is_none() {
                        info!("SCD4x: {} ppm, {} C, {} %RH", r.co2, r.celsius, r.humidity);
                    }
                    climate = Some((r, Instant::now()));
                }
                Ok(None) => scd_errors = 0,
                Err(()) => {
                    scd_errors += 1;
                    if scd_errors >= 3 {
                        warn!("SCD4x stopped answering; rescanning");
                        scd = false;
                        rescan_at = Instant::now();
                    }
                }
            }
        }
        let climate = climate.filter(|(_, at)| at.elapsed() < Duration::from_secs(30)).map(|(r, _)| r);
        READING.signal(Reading {
            celsius: climate.map(|r| r.celsius),
            humidity: climate.map(|r| r.humidity),
            pm25: pm_avg,
            co2: climate.map(|r| r.co2 as f32),
            outdoor: None,
        });
        Timer::after_secs(5).await;
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut board = Presto::default();

    // The heap must be in SRAM (Slint's atomic reference counts do not work in PSRAM), so the
    // frame buffer goes to PSRAM instead.
    {
        static mut HEAP_MEM: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
        // SAFETY: called once, before any allocation.
        unsafe { presto_examples::init_heap_at(core::ptr::addr_of_mut!(HEAP_MEM) as usize, HEAP_SIZE) };
    }
    info!("heap: {} KiB of SRAM", HEAP_SIZE / 1024);
    let Some(psram) = board.psram.as_ref() else {
        panic!("PSRAM not detected: the nursery UI keeps its frame buffer in PSRAM");
    };
    let Some(mut fb) = FrameBuffer::in_psram(psram) else { panic!("PSRAM too small") };
    let psram_kib = psram.size() / 1024;

    let mut storage = Storage::new(board.flash);
    let settings = storage.load();
    info!("settings: brightness {}, Wi-Fi {}", settings.brightness, settings.wifi_enabled);

    board.lcd.start(&fb).unwrap();
    info!("scan-out running");

    // Build with NURSERY_STRESS=1 to redraw the whole screen continuously for the first 30 s
    // (a display bandwidth test).
    let stress = option_env!("NURSERY_STRESS").is_some();
    let window = MinimalSoftwareWindow::new(if stress { RepaintBufferType::NewBuffer } else { RepaintBufferType::ReusedBuffer });
    slint::platform::set_platform(Box::new(PrestoPlatform { window: window.clone() })).unwrap();
    window.set_size(slint::PhysicalSize::new(DISPLAY_WIDTH as u32, DISPLAY_WIDTH as u32));
    let t = Instant::now();
    let ui = NurseryScreen::new().unwrap();
    App::install_text_ops(&ui);
    info!("UI created in {} ms", t.elapsed().as_millis());

    let now_ms = || Instant::now().as_millis();
    let app = Rc::new(RefCell::new(App::new(now_ms())));
    app.borrow_mut().settings = settings;
    app.borrow_mut().version = format!("v{} ({})", env!("CARGO_PKG_VERSION"), env!("NURSERY_GIT"));
    // Wall clock: a local time plus the time elapsed since it was set.
    let clock: Rc<RefCell<Option<(Minutes, Instant)>>> = Rc::new(RefCell::new(None));
    // Settings changed and not yet written to flash. Flash writes stall the PSRAM frame buffer,
    // so they happen when the screen turns off.
    let unsaved: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    // Brightness to apply (the backlight is owned by the main loop).
    let brightness: Rc<RefCell<Option<u8>>> = Rc::new(RefCell::new(None));
    let schedule_save = {
        let unsaved = unsaved.clone();
        move || *unsaved.borrow_mut() = true
    };

    ui.on_activity({
        let app = app.clone();
        move || {
            app.borrow_mut().touch(now_ms());
        }
    });
    ui.on_wake({
        let app = app.clone();
        move || {
            app.borrow_mut().touch(now_ms());
        }
    });
    ui.on_page_changed({
        let app = app.clone();
        move |page| app.borrow_mut().page_override = Some(page)
    });
    ui.on_adjust_clock({
        let (app, ui, clock) = (app.clone(), ui.as_weak(), clock.clone());
        move |minutes| {
            let mut app = app.borrow_mut();
            app.adjust_clock(minutes);
            *clock.borrow_mut() = app.now.map(|now| (now, Instant::now()));
            app.update_ui(&ui.unwrap());
        }
    });
    ui.on_toggle_bag({
        let (app, ui, schedule_save) = (app.clone(), ui.as_weak(), schedule_save.clone());
        move || {
            let mut app = app.borrow_mut();
            app.toggle_bag();
            app.update_ui(&ui.unwrap());
            schedule_save();
        }
    });
    ui.on_set_sensitivity({
        let (app, ui, schedule_save) = (app.clone(), ui.as_weak(), schedule_save.clone());
        move |step| {
            let mut app = app.borrow_mut();
            app.set_sensitivity(step);
            app.update_ui(&ui.unwrap());
            schedule_save();
        }
    });
    ui.on_set_brightness({
        let (app, ui, brightness, schedule_save) = (app.clone(), ui.as_weak(), brightness.clone(), schedule_save.clone());
        move |level| {
            let mut app = app.borrow_mut();
            let settings = app.set_brightness(level);
            *brightness.borrow_mut() = Some(settings.brightness);
            app.update_ui(&ui.unwrap());
            schedule_save();
        }
    });
    ui.on_set_wifi_enabled({
        let (app, ui, schedule_save) = (app.clone(), ui.as_weak(), schedule_save.clone());
        move |on| {
            let mut app = app.borrow_mut();
            app.settings.wifi_enabled = on;
            let command = if on {
                app.wifi.link = WifiLink::Starting;
                wifi::Command::Enable(saved_credentials(&app.settings))
            } else {
                app.wifi.networks.clear();
                app.wifi.scanning = false;
                wifi::Command::Disable
            };
            if wifi::COMMANDS.try_send(command).is_err() {
                warn!("wifi: command queue full");
            }
            app.update_ui(&ui.unwrap());
            schedule_save();
        }
    });
    ui.on_wifi_scan(|| {
        let _ = wifi::COMMANDS.try_send(wifi::Command::Scan);
    });
    ui.on_wifi_join(|ssid, password| {
        let credentials = wifi::Credentials { ssid: ssid.to_string(), password: password.to_string() };
        let _ = wifi::COMMANDS.try_send(wifi::Command::Join(credentials));
    });

    let t = Instant::now();
    app.borrow().update_ui(&ui);
    ui.show().unwrap();
    board.backlight.set_percent(settings.brightness);
    info!("UI populated in {} ms", t.elapsed().as_millis());

    spawner.spawn(touch_task(board.touch).unwrap());
    let qwiic = board.qwiic.into_async({
        let mut config = i2c::Config::default();
        config.frequency = 100_000; // PMSA003I maximum
        config
    });
    spawner.spawn(sensor_task(qwiic).unwrap());
    spawner.spawn(wifi::task(board.wifi, board.dma.0).unwrap());
    if settings.wifi_enabled {
        let _ = wifi::COMMANDS.try_send(wifi::Command::Enable(saved_credentials(&settings)));
    }
    let button: Input<'static> = board.button;

    let started = Instant::now();
    let mut last_record = Instant::now();
    let mut last_minute = None;
    let mut ignore_until_release = false;
    let mut finger_down = false;
    let mut last_position = LogicalPosition::new(0.0, 0.0);
    let mut mac = String::new();
    let mut utc_offset: Option<i32> = None;
    let mut frames = 0u32;
    let mut last_stats = Instant::now();

    loop {
        // Advance the wall clock.
        let now = clock.borrow().map(|(set, at)| Minutes(set.0 + at.elapsed().as_secs() as u32 / 60));
        let mut dirty = false;
        {
            let mut app = app.borrow_mut();
            app.now = now;
            if now.map(|n| n.of_day()) != last_minute {
                last_minute = now.map(|n| n.of_day());
                app.about = about_rows(&app, psram_kib, &mac, started);
                dirty = true;
            }
            if let Some(reading) = READING.try_take() {
                app.latest = reading;
                if last_record.elapsed() >= RECORD_EVERY {
                    last_record = Instant::now();
                    app.record(reading);
                }
                dirty = true;
            }
            while let Ok(event) = wifi::EVENTS.try_receive() {
                dirty = true;
                match event {
                    wifi::Event::Link(link) => {
                        if link == WifiLink::Off {
                            app.wifi.networks.clear();
                            app.wifi.scanning = false;
                        }
                        app.wifi.link = link;
                    }
                    wifi::Event::Scanning => app.wifi.scanning = true,
                    wifi::Event::Networks(networks) => {
                        app.wifi.scanning = false;
                        app.wifi.networks = networks;
                    }
                    wifi::Event::Joined(c) => {
                        app.settings.ssid = InlineStr::from_str_truncated(&c.ssid);
                        app.settings.password = InlineStr::from_str_truncated(&c.password);
                        schedule_save();
                    }
                    wifi::Event::Mac(m) => {
                        mac = format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5]);
                    }
                    wifi::Event::Time { unix, utc_offset: offset } => {
                        if offset.is_some() {
                            utc_offset = offset;
                        }
                        // Until the forecast reports the UTC offset, the clock waits.
                        if let Some(offset) = utc_offset {
                            let local = net::local_minutes(unix, offset);
                            let into_minute = Duration::from_secs(((unix as i64 + offset as i64).rem_euclid(60)) as u64);
                            app.set_time(local);
                            *clock.borrow_mut() = Some((local, Instant::now() - into_minute));
                            info!("clock set from network: {:02}:{:02}", local.hour(), local.minute());
                        }
                    }
                    wifi::Event::Forecast { forecast, utc_offset: offset } => {
                        utc_offset = Some(offset);
                        app.forecast = *forecast;
                    }
                }
            }
            app.no_forecast_reason = match app.wifi.link {
                WifiLink::Off => "No forecast: Wi-Fi is off",
                WifiLink::Connected { .. } => "Fetching the forecast…",
                _ => "No forecast: Wi-Fi not connected",
            };
            if app.poll_sleep(now_ms(), SCREEN_TIMEOUT_MS) == Some(true) {
                info!("screen off");
                board.backlight.set_percent(0);
                dirty = true;
                if core::mem::take(&mut *unsaved.borrow_mut()) {
                    match storage.save(&app.settings) {
                        Ok(()) => info!("settings saved"),
                        Err(e) => warn!("settings not saved: {}", defmt::Debug2Format(&e)),
                    }
                }
            }
            if dirty {
                app.update_ui(&ui);
            }
        }
        if let Some(level) = brightness.borrow_mut().take() {
            board.backlight.set_percent(level);
        }

        if last_stats.elapsed() >= Duration::from_secs(10) {
            last_stats = Instant::now();
            info!(
                "display: ring underruns {}, core 0 pauses {}, FIFO stalls {}, late frames {}",
                board.lcd.underruns(),
                board.lcd.throttles(),
                board.lcd.stalls(),
                board.lcd.late_frames()
            );
        }

        slint::platform::update_timers_and_animations();
        if stress && Instant::now().as_secs() < 30 {
            window.request_redraw();
        }
        if !app.borrow().asleep() {
            let t = Instant::now();
            let drawn = window.draw_if_needed(|renderer| {
                renderer.render_by_line(LineWriter { fb: &mut fb, line: [Rgb565Pixel(0); DISPLAY_WIDTH] });
            });
            if drawn {
                frames += 1;
                if frames <= 3 || frames % if stress { 20 } else { 100 } == 0 {
                    info!(
                        "frame {} rendered in {} ms, heap {} KiB, ring underruns {}",
                        frames,
                        t.elapsed().as_millis(),
                        presto_examples::heap_used() / 1024,
                        board.lcd.underruns()
                    );
                }
            }
        }

        let wait = if window.has_active_animations() {
            Duration::from_millis(16)
        } else {
            slint::platform::duration_until_next_timer_update()
                .map(|d| Duration::from_micros(d.as_micros() as u64))
                .unwrap_or(Duration::from_secs(1))
                .min(Duration::from_millis(500))
        };
        match select4(TOUCHES.receive(), Timer::after(wait), button_press(&button), wifi::EVENTS.ready_to_receive()).await {
            Either4::First(touch) => {
                let woke = app.borrow_mut().touch(now_ms());
                if woke {
                    // The touch that wakes the screen is not passed on to the UI.
                    info!("screen on");
                    board.backlight.set_percent(app.borrow().settings.brightness);
                    ignore_until_release = true;
                    app.borrow().update_ui(&ui);
                    continue;
                }
                match touch {
                    Touch::Down { .. } if ignore_until_release => {}
                    Touch::Up if ignore_until_release => ignore_until_release = false,
                    Touch::Down { x, y } => {
                        let position = LogicalPosition::new(x as f32, y as f32);
                        window.dispatch_event_with_result(WindowEvent::PointerMoved { position }).ok();
                        if !finger_down {
                            finger_down = true;
                            let event = WindowEvent::PointerPressed { position, button: PointerEventButton::Left };
                            window.dispatch_event_with_result(event).ok();
                        }
                        last_position = position;
                    }
                    Touch::Up => {
                        finger_down = false;
                        let event = WindowEvent::PointerReleased { position: last_position, button: PointerEventButton::Left };
                        window.dispatch_event_with_result(event).ok();
                        window.dispatch_event_with_result(WindowEvent::PointerExited).ok();
                    }
                }
            }
            Either4::Second(()) | Either4::Fourth(()) => {}
            Either4::Third(()) => {
                if app.borrow_mut().touch(now_ms()) {
                    info!("screen on (button)");
                    board.backlight.set_percent(app.borrow().settings.brightness);
                    app.borrow().update_ui(&ui);
                }
            }
        }
    }
}

fn saved_credentials(settings: &Settings) -> Option<wifi::Credentials> {
    (!settings.ssid.is_empty()).then(|| wifi::Credentials {
        ssid: settings.ssid.as_str().to_string(),
        password: settings.password.as_str().to_string(),
    })
}

/// Rows for Settings → About.
fn about_rows(app: &App, psram_kib: usize, mac: &str, started: Instant) -> alloc::vec::Vec<(String, String)> {
    let up = started.elapsed().as_secs();
    let sensors = format!(
        "PMSA003I {} · SCD40 {}",
        if app.latest.pm25.is_some() { "ok" } else { "—" },
        if app.latest.celsius.is_some() { "ok" } else { "—" }
    );
    let ip = match &app.wifi.link {
        WifiLink::Connected { ip, .. } => ip.clone(),
        _ => "—".into(),
    };
    vec![
        ("Firmware".into(), app.version.clone()),
        ("Device".into(), "Pimoroni Presto · RP2350B 200 MHz".into()),
        ("Display".into(), "480×480 ST7701 · 63 Hz".into()),
        ("Sensors".into(), sensors),
        ("Wi-Fi MAC".into(), if mac.is_empty() { "— (radio off)".into() } else { mac.into() }),
        ("IP address".into(), ip),
        ("Uptime".into(), format!("{}d {:02}h {:02}m", up / 86_400, up / 3600 % 24, up / 60 % 60)),
        ("Memory".into(), format!("heap {} / {} KiB · PSRAM {} KiB", presto_examples::heap_used() / 1024, HEAP_SIZE / 1024, psram_kib)),
    ]
}

/// Resolves when the (active-low) user button is pressed.
async fn button_press(button: &Input<'static>) {
    // `Input` only offers `&mut` waits; poll instead so the main loop can keep a shared borrow.
    loop {
        if button.is_low() {
            Timer::after_millis(30).await;
            if button.is_low() {
                while button.is_low() {
                    Timer::after_millis(20).await;
                }
                return;
            }
        }
        Timer::after_millis(50).await;
    }
}

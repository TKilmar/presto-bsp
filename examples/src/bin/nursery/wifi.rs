//! Wi-Fi: the RM2 (CYW43439) module, DHCP, and the HTTP requests for location, forecast and time.
//!
//! The task powers the module only while Wi-Fi is enabled. Turning Wi-Fi off drops the whole
//! driver (PIO program, DMA channel, network stack) and holds WL_ON low, so the radio is
//! unpowered and transmits nothing. Turning it on again rebuilds everything from scratch.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use cyw43::{JoinOptions, PowerManagementMode, ScanOptions};
use cyw43_pio::PioSpi;
use defmt::{info, warn};
use embassy_futures::select::{Either, select, select3};
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::{Config as NetConfig, DhcpConfig, Stack, StackResources};
use embassy_rp::Peri;
use embassy_rp::clocks::RoscRng;
use embassy_rp::dma;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::DMA_CH5;
use embassy_rp::pio::Pio;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use fixed::FixedU32;
use fixed::types::extra::U8;
use nursery_core::forecast::Forecast;
use nursery_core::net;
use presto_bsp::{Irqs, WifiPins};

use crate::app::{ScannedNetwork, WifiLink};

/// A network to join.
pub struct Credentials {
    pub ssid: String,
    pub password: String,
}

/// Requests from the UI.
pub enum Command {
    /// Power the radio up and join `Credentials` if given.
    Enable(Option<Credentials>),
    /// Leave, then cut power to the radio.
    Disable,
    Scan,
    Join(Credentials),
}

/// Updates for the UI.
pub enum Event {
    Link(WifiLink),
    Scanning,
    Networks(Vec<ScannedNetwork>),
    /// Joined successfully: worth saving.
    Joined(Credentials),
    Mac([u8; 6]),
    /// Current time from an HTTP `Date` header, and the local UTC offset if known.
    Time { unix: u64, utc_offset: Option<i32> },
    Forecast { forecast: Box<Forecast>, utc_offset: i32 },
}

pub static COMMANDS: Channel<CriticalSectionRawMutex, Command, 4> = Channel::new();
pub static EVENTS: Channel<CriticalSectionRawMutex, Event, 8> = Channel::new();

/// RM2 SPI clock: 200 MHz / 4 / 2 = 25 MHz, the module's rated speed.
const CLOCK_DIVIDER: FixedU32<U8> = FixedU32::from_bits(0x0400);
const FORECAST_EVERY: Duration = Duration::from_secs(60 * 60);
const RETRY_AFTER: Duration = Duration::from_secs(5 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

/// Optional fixed location at build time, e.g. `NURSERY_LAT=52.52 NURSERY_LON=13.41`. Without it,
/// the location is looked up from the public IP address.
fn fixed_location() -> Option<(f32, f32)> {
    Some((option_env!("NURSERY_LAT")?.parse().ok()?, option_env!("NURSERY_LON")?.parse().ok()?))
}

async fn send(event: Event) {
    EVENTS.send(event).await;
}

#[embassy_executor::task]
pub async fn task(mut pins: WifiPins, mut dma: Peri<'static, DMA_CH5>) -> ! {
    loop {
        // Radio off: hold WL_ON low until Wi-Fi is enabled.
        let credentials = {
            let _power_off = Output::new(pins.pwr.reborrow(), Level::Low);
            loop {
                if let Command::Enable(credentials) = COMMANDS.receive().await {
                    break credentials;
                }
            }
        };
        info!("wifi: powering up");
        send(Event::Link(WifiLink::Starting)).await;
        session(&mut pins, dma.reborrow(), credentials).await;
        info!("wifi: radio off");
        send(Event::Link(WifiLink::Off)).await;
    }
}

/// One power-on period of the radio. Returns when Wi-Fi is disabled; everything created here is
/// dropped on return.
async fn session(pins: &mut WifiPins, dma: Peri<'_, DMA_CH5>, credentials: Option<Credentials>) {
    static FIRMWARE: &cyw43::Aligned<cyw43::A4, [u8]> =
        cyw43::aligned_bytes!("../../../nursery/cyw43-firmware/43439A0.bin");
    static NVRAM: &cyw43::Aligned<cyw43::A4, [u8]> =
        cyw43::aligned_bytes!("../../../nursery/cyw43-firmware/nvram_rp2040.bin");

    let power = Output::new(pins.pwr.reborrow(), Level::Low);
    let cs = Output::new(pins.cs.reborrow(), Level::High);
    let Pio { mut common, sm0, irq0, .. } = Pio::new(pins.pio.reborrow(), Irqs);
    let spi = PioSpi::new(
        &mut common,
        sm0,
        CLOCK_DIVIDER,
        irq0,
        cs,
        pins.dio.reborrow(),
        pins.clk.reborrow(),
        dma::Channel::new(dma, Irqs),
    );
    // The driver state stays in SRAM: the SPI DMA uses its buffers, and DMA into PSRAM would
    // collide with flash writes (settings) that take the QSPI bus.
    let mut state = cyw43::State::new();
    let (device, mut control, runner) = cyw43::new(&mut state, power, spi, FIRMWARE, NVRAM).await;

    // The network stack is only touched by the CPU, so it can live in PSRAM.
    let mut resources = Box::new(StackResources::<3>::new());
    let mut dhcp = DhcpConfig::default();
    dhcp.hostname = "presto-nursery".try_into().ok();
    let (stack, mut net_runner) =
        embassy_net::new(device, NetConfig::dhcpv4(dhcp), &mut *resources, RoscRng.next_u64());

    select3(runner.run(), net_runner.run(), control_loop(&mut control, stack, credentials)).await;
    drop(common);
}

async fn control_loop(control: &mut cyw43::Control<'_>, stack: Stack<'_>, credentials: Option<Credentials>) {
    static CLM: &[u8] = include_bytes!("../../../nursery/cyw43-firmware/43439A0_clm.bin");
    control.init(CLM).await;
    control.set_power_management(PowerManagementMode::PowerSave).await;
    send(Event::Mac(control.address().await)).await;
    send(Event::Link(WifiLink::Idle)).await;
    scan(control).await;

    let mut connected = match credentials {
        Some(c) if !c.ssid.is_empty() => join(control, stack, c).await,
        _ => false,
    };
    let mut location = fixed_location();
    let mut next_fetch = Instant::now();

    loop {
        let wait = if connected { next_fetch.saturating_duration_since(Instant::now()) } else { FORECAST_EVERY };
        match select(COMMANDS.receive(), Timer::after(wait)).await {
            Either::First(Command::Disable) => {
                if connected {
                    control.leave().await;
                }
                return;
            }
            Either::First(Command::Scan) => scan(control).await,
            Either::First(Command::Join(c)) => {
                if connected {
                    control.leave().await;
                }
                connected = join(control, stack, c).await;
                next_fetch = Instant::now();
            }
            Either::First(Command::Enable(_)) => {}
            Either::Second(()) if connected => {
                if !stack.is_link_up() {
                    warn!("wifi: link lost");
                    send(Event::Link(WifiLink::Failed("Connection lost".into()))).await;
                    connected = false;
                    continue;
                }
                next_fetch = match with_timeout(HTTP_TIMEOUT * 2, fetch(stack, &mut location)).await {
                    Ok(Ok(())) => Instant::now() + FORECAST_EVERY,
                    _ => {
                        warn!("wifi: forecast fetch failed");
                        Instant::now() + RETRY_AFTER
                    }
                };
            }
            Either::Second(()) => {}
        }
    }
}

async fn scan(control: &mut cyw43::Control<'_>) {
    send(Event::Scanning).await;
    let mut found: Vec<ScannedNetwork> = Vec::new();
    {
        let mut scanner = control.scan(ScanOptions::default()).await;
        while let Some(bss) = scanner.next().await {
            let len = (bss.ssid_len as usize).min(bss.ssid.len());
            let Ok(ssid) = core::str::from_utf8(&bss.ssid[..len]) else { continue };
            if ssid.is_empty() || ssid.bytes().all(|b| b == 0) {
                continue; // hidden network
            }
            let rssi = bss.rssi;
            // Capability bit 4: privacy (WEP/WPA/WPA2/WPA3).
            let secure = bss.capability & 0x0010 != 0;
            match found.iter_mut().find(|n| n.ssid == ssid) {
                Some(n) => n.rssi = n.rssi.max(rssi),
                None => found.push(ScannedNetwork { ssid: ssid.to_string(), rssi, secure }),
            }
        }
    }
    found.sort_by(|a, b| b.rssi.cmp(&a.rssi));
    found.truncate(12);
    info!("wifi: {} networks", found.len());
    send(Event::Networks(found)).await;
}

/// Join a network and wait for DHCP. Reports progress; returns whether it worked.
async fn join(control: &mut cyw43::Control<'_>, stack: Stack<'_>, credentials: Credentials) -> bool {
    let ssid = credentials.ssid.clone();
    send(Event::Link(WifiLink::Connecting(ssid.clone()))).await;
    let mut joined = false;
    for _attempt in 0..2 {
        let options = if credentials.password.is_empty() {
            JoinOptions::new_open()
        } else {
            JoinOptions::new(credentials.password.as_bytes())
        };
        match control.join(&ssid, options).await {
            Ok(()) => {
                joined = true;
                break;
            }
            Err(e) => warn!("wifi: join failed: {}", defmt::Debug2Format(&e)),
        }
    }
    if !joined {
        send(Event::Link(WifiLink::Failed(format!("Could not join {ssid}: wrong password?")))).await;
        return false;
    }
    if with_timeout(Duration::from_secs(20), stack.wait_config_up()).await.is_err() {
        control.leave().await;
        send(Event::Link(WifiLink::Failed(format!("{ssid}: no IP address (DHCP)")))).await;
        return false;
    }
    let ip = stack.config_v4().map(|c| c.address.address().to_string()).unwrap_or_default();
    info!("wifi: connected, {}", ip.as_str());
    send(Event::Link(WifiLink::Connected { ssid, ip })).await;
    send(Event::Joined(credentials)).await;
    true
}

/// Look up the location (once), then fetch the forecast. Both responses set the clock.
async fn fetch(stack: Stack<'_>, location: &mut Option<(f32, f32)>) -> Result<(), ()> {
    if location.is_none() {
        let raw = http_get(stack, net::IP_API_HOST, net::IP_API_PATH).await?;
        let response = net::parse_response(&raw).ok_or(())?;
        if let Some(unix) = response.date {
            send(Event::Time { unix, utc_offset: None }).await;
        }
        *location = net::parse_location(response.body);
    }
    let (lat, lon) = location.ok_or(())?;
    let raw = http_get(stack, net::OPEN_METEO_HOST, &net::open_meteo_path(lat, lon)).await?;
    let response = net::parse_response(&raw).ok_or(())?;
    if response.status != 200 {
        return Err(());
    }
    let (forecast, utc_offset) = net::parse_forecast(response.body).ok_or(())?;
    if let Some(unix) = response.date {
        send(Event::Time { unix, utc_offset: Some(utc_offset) }).await;
    }
    info!("wifi: forecast with {} hours", forecast.hours().len());
    send(Event::Forecast { forecast: Box::new(forecast), utc_offset }).await;
    Ok(())
}

/// Plain HTTP/1.0 GET; returns the raw response (headers and body), up to 16 KiB.
async fn http_get(stack: Stack<'_>, host: &str, path: &str) -> Result<Vec<u8>, ()> {
    let address = *stack.dns_query(host, DnsQueryType::A).await.map_err(|_| ())?.first().ok_or(())?;
    let mut rx = alloc::vec![0u8; 4096];
    let mut tx = alloc::vec![0u8; 512];
    let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
    socket.set_timeout(Some(HTTP_TIMEOUT));
    socket.connect((address, 80)).await.map_err(|_| ())?;
    let request = net::http_get(host, path);
    let mut sent = 0;
    while sent < request.len() {
        sent += socket.write(&request.as_bytes()[sent..]).await.map_err(|_| ())?;
    }
    let mut response = Vec::with_capacity(2048);
    let mut chunk = [0u8; 512];
    loop {
        match socket.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) if response.len() + n <= 16 * 1024 => {
                response.extend_from_slice(&chunk[..n]);
                if net::response_complete(&response) {
                    break;
                }
            }
            Ok(_) | Err(_) => break,
        }
    }
    socket.close();
    Ok(response)
}

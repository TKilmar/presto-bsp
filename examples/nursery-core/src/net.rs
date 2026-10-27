//! Parsing for the two HTTP services the device uses (plain HTTP/1.0, no TLS):
//!
//! * [ip-api.com](http://ip-api.com) to find the approximate location from the public IP;
//! * [Open-Meteo](https://open-meteo.com) for the hourly forecast and the local UTC offset.
//!
//! Every response's `Date` header also sets the clock, so no NTP is needed.

use core::fmt::Write as _;

use serde::Deserialize;

use crate::clock::Minutes;
use crate::forecast::{Forecast, Hour};

/// Host of the location service.
pub const IP_API_HOST: &str = "ip-api.com";
/// Path of the location request.
pub const IP_API_PATH: &str = "/json/?fields=status,lat,lon";
/// Host of the forecast service.
pub const OPEN_METEO_HOST: &str = "api.open-meteo.com";

/// Hours of forecast requested.
pub const FORECAST_HOURS: usize = 36;

/// A request line plus headers for `GET path` on `host`.
pub fn http_get(host: &str, path: &str) -> heapless::String<320> {
    let mut s = heapless::String::new();
    let _ = write!(
        s,
        "GET {path} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: presto-nursery/1\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    s
}

/// The Open-Meteo request path for a location.
pub fn open_meteo_path(lat: f32, lon: f32) -> heapless::String<200> {
    let mut s = heapless::String::new();
    let _ = write!(
        s,
        "/v1/forecast?latitude={lat:.2}&longitude={lon:.2}&hourly=temperature_2m,precipitation_probability&forecast_hours={FORECAST_HOURS}&timezone=auto&timeformat=unixtime"
    );
    s
}

/// A parsed HTTP response.
#[derive(Debug, PartialEq)]
pub struct Response<'a> {
    /// Status code.
    pub status: u16,
    /// `Date` header as Unix seconds.
    pub date: Option<u64>,
    /// Body (honouring `Content-Length` when present).
    pub body: &'a [u8],
}

/// Split a complete HTTP/1.x response into status, date and body.
pub fn parse_response(raw: &[u8]) -> Option<Response<'_>> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = core::str::from_utf8(&raw[..split]).ok()?;
    let mut body = &raw[split + 4..];
    let mut lines = head.split("\r\n");
    let status = lines.next()?.split(' ').nth(1)?.parse().ok()?;
    let mut date = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        if name.eq_ignore_ascii_case("date") {
            date = parse_http_date(value);
        } else if name.eq_ignore_ascii_case("content-length") {
            if let Ok(len) = value.parse::<usize>() {
                body = &body[..len.min(body.len())];
            }
        }
    }
    Some(Response { status, date, body })
}

/// Whether `raw` holds a complete response: headers plus the whole `Content-Length` body.
/// (Without a length, only the server closing the connection marks the end.)
pub fn response_complete(raw: &[u8]) -> bool {
    let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") else { return false };
    let Ok(head) = core::str::from_utf8(&raw[..split]) else { return false };
    head.split("\r\n")
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .is_some_and(|len| raw.len() - split - 4 >= len)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse an IMF-fixdate (`Tue, 27 Oct 2026 20:12:19 GMT`) into Unix seconds.
pub fn parse_http_date(s: &str) -> Option<u64> {
    let mut parts = s.split_whitespace().skip(1);
    let day: u32 = parts.next()?.parse().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = parts.next()?.parse().ok()?;
    let mut hms = parts.next()?.split(':').map(|v| v.parse::<u64>());
    let (h, m, sec) = (hms.next()?.ok()?, hms.next()?.ok()?, hms.next()?.ok()?);
    let days = days_from_civil(year, month, day);
    u64::try_from(days).ok().map(|d| d * 86_400 + h * 3600 + m * 60 + sec)
}

/// Local wall-clock time as [`Minutes`] (minutes since 1970-01-01 local midnight).
pub fn local_minutes(unix_seconds: u64, utc_offset_seconds: i32) -> Minutes {
    Minutes(((unix_seconds as i64 + utc_offset_seconds as i64).max(0) / 60) as u32)
}

#[derive(Deserialize)]
struct IpApi {
    status: heapless::String<16>,
    lat: Option<f32>,
    lon: Option<f32>,
}

/// Latitude and longitude from an ip-api.com response body.
pub fn parse_location(body: &[u8]) -> Option<(f32, f32)> {
    let (r, _): (IpApi, _) = serde_json_core::from_slice(body).ok()?;
    (r.status.as_str() == "success").then_some((r.lat?, r.lon?))
}

#[derive(Deserialize)]
struct Hourly {
    time: heapless::Vec<u32, 48>,
    temperature_2m: heapless::Vec<Option<f32>, 48>,
    precipitation_probability: heapless::Vec<Option<f32>, 48>,
}

#[derive(Deserialize)]
struct OpenMeteo {
    utc_offset_seconds: i32,
    hourly: Hourly,
}

/// The forecast and the local UTC offset from an Open-Meteo response body.
pub fn parse_forecast(body: &[u8]) -> Option<(Forecast, i32)> {
    let (r, _): (OpenMeteo, _) = serde_json_core::from_slice(body).ok()?;
    let offset = r.utc_offset_seconds;
    let h = r.hourly;
    let hours = h.time.iter().zip(h.temperature_2m.iter()).zip(h.precipitation_probability.iter()).filter_map(
        |((&t, &temp), &rain)| {
            Some(Hour {
                start: local_minutes(t as u64, offset),
                celsius: temp?,
                rain_chance: rain.unwrap_or(0.0).clamp(0.0, 100.0) as u8,
            })
        },
    );
    let forecast = Forecast::from_hours(hours);
    (!forecast.is_empty()).then_some((forecast, offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPEN_METEO: &[u8] = include_bytes!("../testdata/open_meteo.http");
    const IP_API: &[u8] = include_bytes!("../testdata/ip_api.http");

    #[test]
    fn dates() {
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_http_date("Tue, 27 Oct 2026 20:12:19 GMT"), Some(1_793_131_939));
        assert_eq!(parse_http_date("Tue, 29 Feb 2028 12:00:00 GMT"), Some(1_835_438_400));
        assert_eq!(parse_http_date("garbage"), None);
        let m = local_minutes(1_793_131_939, -7 * 3600);
        assert_eq!((m.hour(), m.minute()), (13, 12));
    }

    #[test]
    fn location() {
        let r = parse_response(IP_API).unwrap();
        assert_eq!(r.status, 200);
        assert!(r.date.is_some());
        let (lat, lon) = parse_location(r.body).unwrap();
        assert!((lat - 52.52).abs() < 0.01 && (lon - 13.405).abs() < 0.01);
        assert_eq!(parse_location(br#"{"status":"fail"}"#), None);
    }

    #[test]
    fn forecast() {
        let r = parse_response(OPEN_METEO).unwrap();
        assert_eq!(r.status, 200);
        let (forecast, offset) = parse_forecast(r.body).unwrap();
        assert_eq!(offset, 3600);
        assert_eq!(forecast.hours().len(), FORECAST_HOURS);
        let hours = forecast.hours();
        assert!(hours.windows(2).all(|w| w[1].start.0 == w[0].start.0 + 60));
        assert!(hours.iter().all(|h| (-40.0..50.0).contains(&h.celsius) && h.rain_chance <= 100));
        // The first hour is the current local hour.
        let now = local_minutes(r.date.unwrap(), offset);
        assert!(now.0 - hours[0].start.0 < 60);
    }

    #[test]
    fn completeness() {
        assert!(response_complete(IP_API));
        assert!(!response_complete(&IP_API[..IP_API.len() - 1]));
        assert!(!response_complete(OPEN_METEO), "no Content-Length: wait for close");
    }

    #[test]
    fn requests() {
        let path = open_meteo_path(52.52, 13.41);
        assert!(path.starts_with("/v1/forecast?latitude=52.52&longitude=13.41&"));
        let req = http_get(OPEN_METEO_HOST, &path);
        assert!(req.starts_with("GET /v1/forecast?") && req.ends_with("\r\n\r\n"));
    }
}

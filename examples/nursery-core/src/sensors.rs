//! Frame parsers for the I2C sensors (the bus transactions live in the firmware).

/// PMSA003I particulate sensor: 7-bit I2C address.
pub const PMSA003I_ADDR: u8 = 0x12;
/// SCD4x (SCD40/41) CO₂, temperature and humidity sensor: 7-bit I2C address.
pub const SCD4X_ADDR: u8 = 0x62;

/// SCD4x commands (16-bit, sent big-endian) and how long to wait before the next transfer.
pub mod scd4x {
    /// Start measuring every 5 s. The first result is ready 5 s later.
    pub const START_PERIODIC: u16 = 0x21b1;
    /// Stop measuring. Accepted in any mode; the sensor ignores commands for 500 ms afterwards.
    pub const STOP_PERIODIC: u16 = 0x3f86;
    /// Milliseconds to wait after [`STOP_PERIODIC`].
    pub const STOP_MS: u64 = 500;
    /// Read the 48-bit serial number (3 words); only while idle.
    pub const SERIAL: u16 = 0x3682;
    /// Whether a new measurement is ready (1 word; ready if the low 11 bits are not all zero).
    pub const DATA_READY: u16 = 0xe4b8;
    /// Read the last measurement (3 words: CO₂, temperature, humidity). Clears data-ready.
    pub const READ_MEASUREMENT: u16 = 0xec05;
    /// Milliseconds between sending a read command and reading its response.
    pub const READ_MS: u64 = 1;
}

/// One PMSA003I report (concentrations in µg/m³, counts per 0.1 L of air).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PmReading {
    /// PM1.0, standard particle (CF=1).
    pub pm1_standard: u16,
    /// PM2.5, standard particle (CF=1).
    pub pm25_standard: u16,
    /// PM10, standard particle (CF=1).
    pub pm10_standard: u16,
    /// PM1.0 under atmospheric conditions.
    pub pm1: u16,
    /// PM2.5 under atmospheric conditions. Use this for air-quality categories.
    pub pm25: u16,
    /// PM10 under atmospheric conditions.
    pub pm10: u16,
    /// Particles > 0.3, 0.5, 1.0, 2.5, 5.0 and 10 µm.
    pub counts: [u16; 6],
}

/// Errors decoding a sensor frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The frame did not start with the expected header.
    Header,
    /// The checksum or CRC did not match.
    Checksum,
}

/// Decode the 32-byte frame read from the PMSA003I.
pub fn parse_pmsa003i(frame: &[u8; 32]) -> Result<PmReading, FrameError> {
    if frame[0] != 0x42 || frame[1] != 0x4d || u16::from_be_bytes([frame[2], frame[3]]) != 28 {
        return Err(FrameError::Header);
    }
    let sum = frame[..30].iter().map(|&b| b as u16).fold(0u16, u16::wrapping_add);
    if sum != u16::from_be_bytes([frame[30], frame[31]]) {
        return Err(FrameError::Checksum);
    }
    let word = |i: usize| u16::from_be_bytes([frame[4 + 2 * i], frame[5 + 2 * i]]);
    Ok(PmReading {
        pm1_standard: word(0),
        pm25_standard: word(1),
        pm10_standard: word(2),
        pm1: word(3),
        pm25: word(4),
        pm10: word(5),
        counts: core::array::from_fn(|i| word(6 + i)),
    })
}

/// Sensirion CRC-8 (polynomial 0x31, init 0xFF).
pub fn sensirion_crc(data: &[u8]) -> u8 {
    let mut crc = 0xffu8;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x31 } else { crc << 1 };
        }
    }
    crc
}

/// Decode a Sensirion response: 16-bit big-endian words, each followed by its CRC.
pub fn parse_words<const B: usize, const W: usize>(frame: &[u8; B]) -> Result<[u16; W], FrameError> {
    debug_assert_eq!(B, 3 * W);
    let mut words = [0u16; W];
    for (word, chunk) in words.iter_mut().zip(frame.chunks_exact(3)) {
        if sensirion_crc(&chunk[..2]) != chunk[2] {
            return Err(FrameError::Checksum);
        }
        *word = u16::from_be_bytes([chunk[0], chunk[1]]);
    }
    Ok(words)
}

/// One SCD4x measurement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Co2Reading {
    /// CO₂, ppm.
    pub co2: u16,
    /// Temperature, °C (with the sensor's default 4 °C self-heating offset already applied).
    pub celsius: f32,
    /// Relative humidity, %.
    pub humidity: f32,
}

/// Decode the 9-byte response to [`scd4x::READ_MEASUREMENT`].
pub fn parse_scd4x(frame: &[u8; 9]) -> Result<Co2Reading, FrameError> {
    let [co2, t, rh] = parse_words::<9, 3>(frame)?;
    Ok(Co2Reading {
        co2,
        celsius: -45.0 + 175.0 * t as f32 / 65535.0,
        humidity: (100.0 * rh as f32 / 65535.0).clamp(0.0, 100.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pmsa003i_frame() {
        let mut f = [0u8; 32];
        f[..4].copy_from_slice(&[0x42, 0x4d, 0x00, 0x1c]);
        for i in 0..13 {
            f[4 + 2 * i..6 + 2 * i].copy_from_slice(&(i as u16 + 1).to_be_bytes());
        }
        let sum = f[..30].iter().map(|&b| b as u16).sum::<u16>();
        f[30..].copy_from_slice(&sum.to_be_bytes());
        let r = parse_pmsa003i(&f).unwrap();
        assert_eq!((r.pm1_standard, r.pm25, r.pm10, r.counts[5]), (1, 5, 6, 12));
        f[10] ^= 1;
        assert_eq!(parse_pmsa003i(&f), Err(FrameError::Checksum));
        f[0] = 0;
        assert_eq!(parse_pmsa003i(&f), Err(FrameError::Header));
    }

    #[test]
    fn scd4x_frame() {
        // Datasheet CRC example: 0xBEEF -> 0x92.
        assert_eq!(sensirion_crc(&[0xbe, 0xef]), 0x92);
        // Datasheet read_measurement example: 500 ppm, 25 °C (0x6667), 37 %RH (0x5EB9).
        let f = [0x01, 0xf4, 0x33, 0x66, 0x67, 0xa2, 0x5e, 0xb9, 0x3c];
        let r = parse_scd4x(&f).unwrap();
        assert_eq!(r.co2, 500);
        assert!((r.celsius - 25.0).abs() < 0.01, "{}", r.celsius);
        assert!((r.humidity - 37.0).abs() < 0.01, "{}", r.humidity);
        let mut bad = f;
        bad[8] ^= 1;
        assert_eq!(parse_scd4x(&bad), Err(FrameError::Checksum));
    }
}

//! Frame parsers for the I2C sensors (the bus transactions live in the firmware).

/// PMSA003I particulate sensor: 7-bit I2C address.
pub const PMSA003I_ADDR: u8 = 0x12;
/// SHT4x temperature/humidity sensor: 7-bit I2C address (SHT40/41/45 "A" variants).
pub const SHT4X_ADDR: u8 = 0x44;
/// All SHT4x addresses ("A", "B" and "C" variants).
pub const SHT4X_ADDRS: [u8; 3] = [0x44, 0x45, 0x46];
/// SHT4x command: measure T and RH with high repeatability (result ready after 8.3 ms).
pub const SHT4X_MEASURE_HIGH: u8 = 0xFD;

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

/// Decode the 6-byte SHT4x measurement into (°C, %RH).
pub fn parse_sht4x(frame: &[u8; 6]) -> Result<(f32, f32), FrameError> {
    if sensirion_crc(&frame[0..2]) != frame[2] || sensirion_crc(&frame[3..5]) != frame[5] {
        return Err(FrameError::Checksum);
    }
    let t = u16::from_be_bytes([frame[0], frame[1]]) as f32;
    let rh = u16::from_be_bytes([frame[3], frame[4]]) as f32;
    let celsius = -45.0 + 175.0 * t / 65535.0;
    let humidity = (-6.0 + 125.0 * rh / 65535.0).clamp(0.0, 100.0);
    Ok((celsius, humidity))
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
    fn sht4x_frame() {
        // Datasheet CRC example: 0xBEEF -> 0x92.
        assert_eq!(sensirion_crc(&[0xbe, 0xef]), 0x92);
        let t = (((25.0 + 45.0) / 175.0) * 65535.0) as u16;
        let rh = (((50.0 + 6.0) / 125.0) * 65535.0) as u16;
        let mut f = [0u8; 6];
        f[..2].copy_from_slice(&t.to_be_bytes());
        f[2] = sensirion_crc(&f[..2]);
        f[3..5].copy_from_slice(&rh.to_be_bytes());
        f[5] = sensirion_crc(&f[3..5]);
        let (c, h) = parse_sht4x(&f).unwrap();
        assert!((c - 25.0).abs() < 0.01 && (h - 50.0).abs() < 0.01);
        f[5] ^= 1;
        assert!(parse_sht4x(&f).is_err());
    }
}

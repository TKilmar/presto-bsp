//! User settings and their storage format.
//!
//! The firmware keeps one copy in the last flash sector; the layout is fixed-size with a magic
//! number, version and CRC so a blank or corrupted sector simply yields the defaults.

/// Longest Wi-Fi network name (802.11 limit).
pub const SSID_MAX: usize = 32;
/// Longest WPA2 passphrase.
pub const PASSWORD_MAX: usize = 63;
/// Size of the serialised settings.
pub const STORED_SIZE: usize = 128;

const MAGIC: u32 = 0x4e55_5253; // "NURS"
const VERSION: u8 = 1;

/// A short string stored inline.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct InlineStr<const N: usize> {
    bytes: [u8; N],
    len: u8,
}

impl<const N: usize> InlineStr<N> {
    /// Empty string.
    pub const fn new() -> Self {
        Self { bytes: [0; N], len: 0 }
    }

    /// Copy `s`, truncated to `N` bytes on a character boundary.
    pub fn from_str_truncated(s: &str) -> Self {
        let mut end = s.len().min(N);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        let mut out = Self::new();
        out.bytes[..end].copy_from_slice(&s.as_bytes()[..end]);
        out.len = end as u8;
        out
    }

    /// The string.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }

    /// `true` if empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const N: usize> Default for InlineStr<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> core::fmt::Debug for InlineStr<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self.as_str(), f)
    }
}

/// Everything the user can change on the settings screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Backlight, 10–100 %.
    pub brightness: u8,
    /// Thermal sensitivity, −2 (runs cool) to +2 (runs warm).
    pub sensitivity: i8,
    /// Radio on. When off the Wi-Fi module is not powered at all.
    pub wifi_enabled: bool,
    /// Network to join.
    pub ssid: InlineStr<SSID_MAX>,
    /// Its passphrase (empty for an open network).
    pub password: InlineStr<PASSWORD_MAX>,
    /// The baby sleeps without a sleeping bag; recommendations compensate with warmer clothes.
    pub no_bag: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            brightness: 70,
            sensitivity: 0,
            wifi_enabled: false,
            ssid: InlineStr::new(),
            password: InlineStr::new(),
            no_bag: false,
        }
    }
}

/// CRC-32 (IEEE, as used by zlib).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

impl Settings {
    /// Brightness steps offered on the settings screen.
    pub const BRIGHTNESS_STEP: u8 = 10;

    /// Clamp every field into its valid range.
    pub fn sanitized(mut self) -> Self {
        self.brightness = self.brightness.clamp(10, 100) / Self::BRIGHTNESS_STEP * Self::BRIGHTNESS_STEP;
        self.sensitivity = self.sensitivity.clamp(-2, 2);
        self
    }

    /// Serialise for storage.
    pub fn to_bytes(&self) -> [u8; STORED_SIZE] {
        let mut b = [0xffu8; STORED_SIZE];
        b[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        b[4] = VERSION;
        b[5] = self.brightness;
        b[6] = self.sensitivity as u8;
        b[7] = self.wifi_enabled as u8;
        b[8] = self.ssid.len;
        b[9..9 + SSID_MAX].copy_from_slice(&self.ssid.bytes);
        let p = 9 + SSID_MAX;
        b[p] = self.password.len;
        b[p + 1..p + 1 + PASSWORD_MAX].copy_from_slice(&self.password.bytes);
        // 2, not 1: firmware before the sleeping-bag toggle stored "no bodysuit" as 1 here.
        b[p + 1 + PASSWORD_MAX] = if self.no_bag { 2 } else { 0 };
        let crc = crc32(&b[..STORED_SIZE - 4]);
        b[STORED_SIZE - 4..].copy_from_slice(&crc.to_le_bytes());
        b
    }

    /// Deserialise, or `None` if the data is blank, from another version or corrupted.
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < STORED_SIZE || u32::from_le_bytes(b[0..4].try_into().ok()?) != MAGIC || b[4] != VERSION {
            return None;
        }
        let crc = u32::from_le_bytes(b[STORED_SIZE - 4..STORED_SIZE].try_into().ok()?);
        if crc32(&b[..STORED_SIZE - 4]) != crc {
            return None;
        }
        let p = 9 + SSID_MAX;
        let ssid_len = (b[8] as usize).min(SSID_MAX);
        let pass_len = (b[p] as usize).min(PASSWORD_MAX);
        let ssid = core::str::from_utf8(&b[9..9 + ssid_len]).ok()?;
        let password = core::str::from_utf8(&b[p + 1..p + 1 + pass_len]).ok()?;
        Some(
            Self {
                brightness: b[5],
                sensitivity: b[6] as i8,
                wifi_enabled: b[7] != 0,
                ssid: InlineStr::from_str_truncated(ssid),
                password: InlineStr::from_str_truncated(password),
                no_bag: b[p + 1 + PASSWORD_MAX] == 2,
            }
            .sanitized(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Settings {
            brightness: 40,
            sensitivity: -1,
            wifi_enabled: true,
            ssid: InlineStr::from_str_truncated("Nursery Wi-Fi"),
            password: InlineStr::from_str_truncated("correct horse battery"),
            no_bag: true,
        };
        let bytes = s.to_bytes();
        assert_eq!(Settings::from_bytes(&bytes), Some(s));
    }

    #[test]
    fn rejects_blank_and_corrupt() {
        assert_eq!(Settings::from_bytes(&[0xff; STORED_SIZE]), None);
        let mut bytes = Settings::default().to_bytes();
        bytes[20] ^= 1;
        assert_eq!(Settings::from_bytes(&bytes), None);
    }

    #[test]
    fn truncates_on_char_boundary_and_sanitizes() {
        let s = InlineStr::<4>::from_str_truncated("aé€x");
        assert_eq!(s.as_str(), "aé");
        let s = Settings { brightness: 3, sensitivity: 9, ..Default::default() }.sanitized();
        assert_eq!((s.brightness, s.sensitivity), (10, 2));
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}

//! Simple piezo helper for the Presto speaker on GPIO43.
use embassy_rp::gpio::{Output};
use embassy_time::{Duration, Instant, Timer};

/// Represents any pitch
#[derive(Copy, Clone, PartialEq)]
#[allow(missing_docs)]
pub enum Pitch {
    Silent,
    Named(NamedPitch),
    /// Hz
    Frequency(u32),
}

/// Pitch for standard scale
#[allow(dead_code, missing_docs)]
#[derive(Copy, Clone, PartialEq)]
#[repr(u32)]
pub enum NamedPitch {
    C0 = 16,
    CS0 = 17,
    D0 = 18,
    DS0 = 19,
    E0 = 20,
    F0 = 21,
    FS0 = 23,
    G0 = 24,
    GS0 = 25,
    A0 = 27,
    AS0 = 29,
    B0 = 30,
    C1 = 32,
    CS1 = 34,
    D1 = 36,
    DS1 = 38,
    E1 = 41,
    F1 = 43,
    FS1 = 46,
    G1 = 49,
    GS1 = 51,
    A1 = 55,
    AS1 = 58,
    B1 = 61,
    C2 = 65,
    CS2 = 69,
    D2 = 73,
    DS2 = 77,
    E2 = 82,
    F2 = 87,
    FS2 = 92,
    G2 = 98,
    GS2 = 103,
    A2 = 110,
    AS2 = 116,
    B2 = 123,
    C3 = 130,
    CS3 = 138,
    D3 = 146,
    DS3 = 155,
    E3 = 164,
    F3 = 174,
    FS3 = 185,
    G3 = 196,
    GS3 = 207,
    A3 = 220,
    AS3 = 233,
    B3 = 246,
    C4 = 261,
    CS4 = 277,
    D4 = 293,
    DS4 = 311,
    E4 = 329,
    F4 = 349,
    FS4 = 369,
    G4 = 392,
    GS4 = 415,
    A4 = 440,
    AS4 = 466,
    B4 = 493,
    C5 = 523,
    CS5 = 554,
    D5 = 587,
    DS5 = 622,
    E5 = 659,
    F5 = 698,
    FS5 = 739,
    G5 = 783,
    GS5 = 830,
    A5 = 880,
    AS5 = 932,
    B5 = 987,
    C6 = 1046,
    CS6 = 1108,
    D6 = 1174,
    DS6 = 1244,
    E6 = 1318,
    F6 = 1396,
    FS6 = 1479,
    G6 = 1567,
    GS6 = 1661,
    A6 = 1760,
    AS6 = 1864,
    B6 = 1975,
    C7 = 2093,
    CS7 = 2217,
    D7 = 2349,
    DS7 = 2489,
    E7 = 2637,
    F7 = 2793,
    FS7 = 2959,
    G7 = 3135,
    GS7 = 3322,
    A7 = 3520,
    AS7 = 3729,
    B7 = 3951,
    C8 = 4186,
    CS8 = 4434,
    D8 = 4698,
    DS8 = 4978,
    E8 = 5274,
    F8 = 5587,
    FS8 = 5919,
    G8 = 6271,
    GS8 = 6644,
    A8 = 7040,
    AS8 = 7458,
    B8 = 7902,
}

impl NamedPitch {
    /// Turn into Hz
    #[must_use]
    pub fn into_frequency(self) -> u32 {
        self as u32
    }
}

impl From<NamedPitch> for Pitch {
    fn from(value: NamedPitch) -> Self {
        Self::Named(value)
    }
}

/// A note is a pitch + a duration (ms)
#[derive(Clone, Copy)]
pub struct Note(pub Pitch, pub u32);

/// Bit-banged speaker suitable for the Presto piezo on GPIO43.
pub struct SoftSpeaker<'a> {
    pin: Output<'a>,
}

impl <'a>SoftSpeaker<'a> {
    /// Create a new speaker instance from an output pin.
    pub fn new(pin: Output<'a>) -> Self {
        Self { pin }
    }

    /// Play a note asynchronously by toggling the pin.
    pub async fn play(&mut self, note: &Note) {
        let Note(pitch, duration_ms) = note;
        let frequency = match pitch {
            Pitch::Silent => {
                Timer::after_millis(u64::from(*duration_ms)).await;
                return;
            }
            Pitch::Named(n) => n.into_frequency(),
            Pitch::Frequency(f) => *f,
        };

        let period_us = 1_000_000u32 / frequency;
        if period_us == 0 {
            return;
        }
        let half_period = u64::from(period_us / 2);
        let end = Instant::now() + Duration::from_millis(u64::from(*duration_ms));

        while Instant::now() < end {
            self.pin.set_high();
            Timer::after_micros(half_period).await;
            self.pin.set_low();
            Timer::after_micros(half_period).await;
        }
    }

    /// Play a note in a blocking fashion (spins).
    pub fn play_blocking(&mut self, note: &Note) {
        let Note(pitch, duration_ms) = note;
        let frequency = match pitch {
            Pitch::Silent => {
                busy_wait_ms(*duration_ms);
                return;
            }
            Pitch::Named(n) => n.into_frequency(),
            Pitch::Frequency(f) => *f,
        };

        let period_us = 1_000_000u32 / frequency;
        if period_us == 0 {
            return;
        }
        let half_period = period_us / 2;
        let start = embassy_time::Instant::now();
        let total = Duration::from_millis(u64::from(*duration_ms));

        while embassy_time::Instant::now() - start < total {
            self.pin.set_high();
            spin_delay_us(half_period);
            self.pin.set_low();
            spin_delay_us(half_period);
        }
    }
}

fn busy_wait_ms(ms: u32) {
    spin_delay_us(ms.saturating_mul(1000));
}

fn spin_delay_us(mut us: u32) {
    // Simple calibrated spin. This is crude but avoids extra dependencies.
    // Each iteration should cost a few cycles; scale empirically if needed.
    while us > 0 {
        cortex_m::asm::nop();
        us -= 1;
    }
}

use embassy_hal_internal::Peri;
use embassy_rp::{
    clocks::clk_sys_freq, dma::{AnyChannel, Channel}, gpio::{Level, Output}, peripherals::SPI1, pio::{
        self, Common, Config, FifoJoin, Instance, LoadedProgram, Pin, ShiftConfig, ShiftDirection, StateMachine, program::pio_asm
    }, spi::{Blocking, Spi}
};
use fixed::types::U24F8;
use embassy_time::{Duration, block_for};
use slint::platform::software_renderer as sr;

/// Default display width in pixels.
pub const DISPLAY_WIDTH: usize = 480;
/// Default display height in pixels.
pub const DISPLAY_HEIGHT: usize = 480;

/// Errors that can be raised by the LCD driver.
#[derive(Debug)]
pub enum Error {
    /// The provided frame buffer does not match the configured display size.
    IncorrectBufferLength,
}

/// Placeholder used by the board definition until an LCD driver is instantiated.
pub struct LcdPlaceholder;

impl LcdPlaceholder {
    /// Construct a new placeholder.
    pub const fn new() -> Self {
        Self
    }
}

#[allow(dead_code)]
#[derive(Copy, Clone)]
#[repr(u8)]
pub enum DataCommand {
    Cmd = 0x00,
    Data = 0x01
}

/// ST7701 register addresses.
#[allow(dead_code)]
#[derive(Copy, Clone)]
#[repr(u8)]
pub enum Register {
    SwReset = 0x01,
    SleepOut = 0x11,
    InvOn = 0x21,
    DisplayOn = 0x29,
    MadCtl = 0x36,
    ColMod = 0x3A,
    Cnd2BkxSel = 0xFF,
    RegB0 = 0xB0,
    RegB1 = 0xB1,
    RegB2 = 0xB2,
    RegB3 = 0xB3,
    RegB5 = 0xB5,
    RegB6 = 0xB6,
    RegB7 = 0xB7,
    RegB8 = 0xB8,
    RegBA = 0xBA,
    RegBC = 0xBC,
    RegC0 = 0xC0,
    RegC1 = 0xC1,
    RegC2 = 0xC2,
    RegC3 = 0xC3,
    RegC5 = 0xC5,
    RegC7 = 0xC7,
    RegC8 = 0xC8,
    RegCA = 0xCA,
    RegCC = 0xCC,
    RegCD = 0xCD,
    RegE0 = 0xE0,
    RegE1 = 0xE1,
    RegE2 = 0xE2,
    RegE3 = 0xE3,
    RegE4 = 0xE4,
    RegE5 = 0xE5,
    RegE6 = 0xE6,
    RegE7 = 0xE7,
    RegE8 = 0xE8,
    RegEB = 0xEB,
    RegEC = 0xEC,
    RegED = 0xED,
}

/// ST7701 driver that streams full frames over a 16-bit parallel bus using PIO + DMA.
///
/// The driver is intentionally minimal to support Slint: supply a rendered RGB565 frame buffer
/// and call [`write_buffer`](Self::write_buffer) to push it to the panel. Synchronisation signals
/// are kept high and only a dot clock is toggled; the panel must already be configured for
/// data-enable mode by application code.
pub struct St7701<'d, PIO: Instance, const SM: usize> {
    sm: StateMachine<'d, PIO, SM>,
    _program: LoadedProgram<'d, PIO>,
    dma: Peri<'d, AnyChannel>,
    // Keep ownership of the pins for the lifetime of the driver.
    _data_pins: [Pin<'d, PIO>; 16],
    _clock_pin: Pin<'d, PIO>,
    _de_pin: Option<Pin<'d, PIO>>,
    cs: Output<'d>,
    spi: Spi<'d, SPI1, Blocking>
}

impl<'d, PIO: Instance, const SM: usize> St7701<'d, PIO, SM> {
    /// Create a new ST7701 stream.
    ///
    /// * `common` / `sm` – the PIO block and state machine to dedicate to the LCD data stream.
    /// * `dma` – DMA channel used to feed the TX FIFO.
    /// * `data_pins` – sixteen consecutive pins for the RGB565 bus (D0..D15).
    /// * `dotclk` – dot clock pin, toggled via side-set.
    /// * `de` – optional data enable pin; if provided it is driven high.
    /// * `width`/`height` – resolution of the frame buffers supplied to [`write_buffer`](Self::write_buffer).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        common: &mut Common<'d, PIO>,
        mut sm: StateMachine<'d, PIO, SM>,
        dma: Peri<'d, impl Channel>,
        data_pins: [Pin<'d, PIO>; 16],
        dotclk: Pin<'d, PIO>,
        de: Option<Pin<'d, PIO>>,
        cs: Output<'d>,
        spi: Spi<'d, SPI1, Blocking>
    ) -> Self {
        let prg = pio_asm!(
            ".side_set 1",
            ".wrap_target",
            // Present 16 bits of RGB565 data with dot clock low.
            "out pins, 16 side 0",
            // Strobe dot clock high for one cycle to latch the pixel.
            "nop side 1",
            ".wrap",
        );
        let program = common.load_program(&prg.program);

        let data_pins = data_pins;
        let clock_pin = dotclk;
        let de_pin = de;

        let data_refs: [&Pin<'d, PIO>; 16] = core::array::from_fn(|idx| &data_pins[idx]);
        let side_refs = [&clock_pin];

        let mut cfg = Config::default();
        cfg.use_program(&program, &side_refs);
        cfg.set_out_pins(&data_refs);
        cfg.fifo_join = FifoJoin::TxOnly;
        cfg.shift_out = ShiftConfig {
            auto_fill: true,
            threshold: 16,
            direction: ShiftDirection::Right,
        };

        // Target ~12 MHz dot clock by default; adjust as needed for your panel timing.
        let sys_khz = U24F8::from_num(clk_sys_freq() / 1000);
        let dotclk_khz = U24F8::from_num(12_000);
        cfg.clock_divider = sys_khz / dotclk_khz;

        sm.set_config(&cfg);
        sm.set_pin_dirs(pio::Direction::Out, &data_refs);
        sm.set_pin_dirs(pio::Direction::Out, &side_refs);

        if let Some(ref de_pin) = de_pin {
            sm.set_pin_dirs(pio::Direction::Out, core::slice::from_ref(&de_pin));
            sm.set_pins(Level::High, core::slice::from_ref(&de_pin));
        }

        sm.set_enable(true);

        Self {
            sm,
            _program: program,
            dma: dma.into(),
            _data_pins: data_pins,
            _clock_pin: clock_pin,
            _de_pin: de_pin,
            cs,
            spi
        }
    }

    // blocking write adapted for 9bit spi
    fn write_spi(&mut self, reg: Register, data: &[u8]) -> Result<(), ()> {
        let p = embassy_rp::pac::SPI1;
        self.cs.set_low();

        // cmd section
        while !p.sr().read().tnf() {}
        p.dr().write(|w| w.set_data(reg as _));
        while !p.sr().read().rne() {}
        let _ = p.dr().read();

        // data section
        for &b in data {
            while !p.sr().read().tnf() {}
            p.dr().write(|w| w.set_data((b as u16) + 0x100));
            while !p.sr().read().rne() {}
            let _ = p.dr().read();
        }

        self.spi.flush().unwrap();

        self.cs.set_high();
        Ok(())
    }

    /// Run the ST7701 power-on register sequence. The provided closure is responsible for
    /// driving the 3-line SPI interface (9th bit = D/C) or an equivalent command transport.
    ///
    pub fn init(&mut self) -> Result<(), ()> {
        use Register::*;

        self.write_spi(SwReset, &[])?;
        block_for(Duration::from_millis(150));

        self.write_spi(Cnd2BkxSel, &[0x77, 0x01, 0x00, 0x00, 0x10])?;

        self.write_spi(MadCtl, &[0x00])?;
        self.write_spi(RegC0, &[0x3b, 0x00])?;
        self.write_spi(RegC1, &[0x0d, 0x02])?;
        self.write_spi(RegC2, &[0x31, 0x01])?; // this is set to 0x05
        self.write_spi(RegCD, &[0x08])?;
        self.write_spi(
            RegB0,
            &[0x00, 0x11, 0x18, 0x0e, 0x11, 0x06, 0x07, 0x08, 0x07, 0x22, 0x04, 0x12, 0x0f, 0xaa, 0x31, 0x18],
        )?;
        self.write_spi(
            RegB1,
            &[0x00, 0x11, 0x19, 0x0e, 0x12, 0x07, 0x08, 0x08, 0x08, 0x22, 0x04, 0x11, 0x11, 0xa9, 0x32, 0x18],
        )?;
        self.write_spi(RegC3, &[0x80, 0x2e, 0x0e])?; // this does not exist

        self.write_spi(Cnd2BkxSel, &[0x77, 0x01, 0x00, 0x00, 0x11])?;
        self.write_spi(RegB0, &[0x60])?;
        self.write_spi(RegB1, &[0x32])?;
        self.write_spi(RegB2, &[0x07])?;
        self.write_spi(RegB3, &[0x80])?;
        self.write_spi(RegB5, &[0x49])?;
        self.write_spi(RegB7, &[0x85])?;
        self.write_spi(RegB8, &[0x21])?;
        self.write_spi(RegC1, &[0x78])?;
        self.write_spi(RegC2, &[0x78])?;

        // Forbidden sequence specific to TL040WVS03CT15-H1263A.
        self.write_spi(RegE0, &[0x00, 0x1b, 0x02])?;
        self.write_spi(RegE1, &[0x08, 0xa0, 0x00, 0x00, 0x07, 0xa0, 0x00, 0x00, 0x00, 0x44, 0x44])?;
        self.write_spi(RegE2, &[0x11, 0x11, 0x44, 0x44, 0xed, 0xa0, 0x00, 0x00, 0xec, 0xa0, 0x00, 0x00])?;
        self.write_spi(RegE3, &[0x00, 0x00, 0x11, 0x11])?;
        self.write_spi(RegE4, &[0x44, 0x44])?;
        self.write_spi(RegE5, &[0x0a, 0xe9, 0xd8, 0xa0, 0x0c, 0xeb, 0xd8, 0xa0, 0x0e, 0xed, 0xd8, 0xa0, 0x10, 0xef, 0xd8, 0xa0])?;
        self.write_spi(RegE6, &[0x00, 0x00, 0x11, 0x11])?;
        self.write_spi(RegE7, &[0x44, 0x44])?;
        self.write_spi(RegE8, &[0x09, 0xe8, 0xd8, 0xa0, 0x0b, 0xea, 0xd8, 0xa0, 0x0d, 0xec, 0xd8, 0xa0, 0x0f, 0xee, 0xd8, 0xa0])?;
        self.write_spi(RegEB, &[0x02, 0x00, 0xe4, 0xe4, 0x88, 0x00, 0x40])?;
        self.write_spi(RegEC, &[0x3c, 0x00])?;
        self.write_spi(RegED, &[0xab, 0x89, 0x76, 0x54, 0x02, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x20, 0x45, 0x67, 0x98, 0xba])?;
        self.write_spi(MadCtl, &[0x00])?;

        self.write_spi(Cnd2BkxSel, &[0x77, 0x01, 0x00, 0x00, 0x13])?;
        self.write_spi(RegE5, &[0xe4])?;

        self.write_spi(Cnd2BkxSel, &[0x77, 0x01, 0x00, 0x00, 0x00])?;
        self.write_spi(ColMod, &[0x66])?;

        self.write_spi(InvOn, &[])?;
        block_for(Duration::from_millis(1));
        self.write_spi(SleepOut, &[])?;
        block_for(Duration::from_millis(120));
        self.write_spi(DisplayOn, &[])?;
        block_for(Duration::from_millis(100));
        Ok(())
    }

    /// Stream a full RGB565 frame to the panel. The slice must contain exactly `width * height`
    /// pixels in little-endian RGB565 format (matches Slint's `Rgb565Pixel` layout).
    pub async fn write_buffer(&mut self, pixels: &[sr::Rgb565Pixel]) -> Result<(), Error> {
        let expected = DISPLAY_HEIGHT * DISPLAY_WIDTH;
        if pixels.len() < expected {
            return Err(Error::IncorrectBufferLength);
        }

        let words: &[u16] = unsafe { core::slice::from_raw_parts(pixels.as_ptr() as *const u16, expected) };
        self.sm.tx().dma_push(self.dma.reborrow(), words, false).await;
        Ok(())
    }
}

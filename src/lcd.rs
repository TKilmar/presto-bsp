use defmt::{debug, info};
use embassy_hal_internal::Peri;
use embassy_rp::{
    clocks::clk_sys_freq, dma::{AnyChannel, Channel}, gpio::Output, peripherals::SPI1, pio::{self, Common, Config, FifoJoin, Instance, Irq, Pin, StateMachine, program::pio_asm}, spi::{Blocking, Spi}
};
use fixed::types::U24F8;
use embassy_time::{Duration, block_for};
use slint::platform::software_renderer as sr;

/// Default display width in pixels.
pub const DISPLAY_WIDTH: usize = 480;
/// Default display height in pixels.
pub const DISPLAY_HEIGHT: usize = 480;
const DOTCLK_PERIODS: u32 = 261_360;
const HSYNC_LINES: u32 = 495;
const HSYNC_HIGH_PERIODS: u32 = 526;

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
pub struct St7701<'d, PIO: Instance, const SM0: usize, const SM1: usize, const SM2: usize, const SM3: usize> {
    sm_dotclk: StateMachine<'d, PIO, SM0>,
    sm_hsync: StateMachine<'d, PIO, SM1>,
    sm_vsync: StateMachine<'d, PIO, SM2>,
    sm_data: StateMachine<'d, PIO, SM3>,
    dma: Peri<'d, AnyChannel>,
    dma2: Peri<'d, AnyChannel>,
    irq_flags: pio::IrqFlags<'d, PIO>,
    cs: Output<'d>,
    spi: Spi<'d, SPI1, Blocking>,
}

impl<'d, PIO: Instance, const SM0: usize, const SM1: usize, const SM2: usize, const SM3: usize>
    St7701<'d, PIO, SM0, SM1, SM2, SM3>
{
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
        irq_flags: pio::IrqFlags<'d, PIO>,
        mut sm_dotclk: StateMachine<'d, PIO, SM0>,
        mut sm_hsync: StateMachine<'d, PIO, SM1>,
        mut sm_vsync: StateMachine<'d, PIO, SM2>,
        mut sm_data: StateMachine<'d, PIO, SM3>,
        dma: Peri<'d, impl Channel>,
        dma2: Peri<'d, impl Channel>,
        data_pins: [Pin<'d, PIO>; 16],
        dotclk: Pin<'d, PIO>,
        hsync: Pin<'d, PIO>,
        vsync: Pin<'d, PIO>,
        de: Pin<'d, PIO>,
        cs: Output<'d>,
        spi: Spi<'d, SPI1, Blocking>
    ) -> Self {

        let prg_dotclk = pio_asm!(
            ".side_set 1",
            "pull block side 0",
            ".wrap_target",
            "mov x, osr side 0",
            "wait 0 irq 1 side 0",
            "loop:",
            "nop side 1",
            "jmp x--, loop side 0",
            ".wrap",
        );

        let prg_vsync = pio_asm!(
            ".side_set 1",
            ".wrap_target",
            "set x, 2 side 1",
            "wait 0 irq 1 side 1",
            "loop:",
            "jmp x--, loop side 0",
            "irq set 1 side 0",
            ".wrap",
        );

        let prg_hsync = pio_asm!(
            ".side_set 1",
            "pull block side 1",
            "mov isr, osr side 1",
            "pull block side 1",
            ".wrap_target",
            "mov y, osr side 1",
            "wait 0 irq 1 side 1",
            "line:",
            "set x, 2 side 0",
            "sync:",
            "jmp x--, sync side 0",
            "mov x, isr side 1",
            "rest:",
            "jmp x--, rest side 1",
            "jmp y--, line side 1",
            ".wrap",
        );

        let prog_data = pio_asm!(
            ".side_set 1",
            "set pins, 0 side 0",
            ".wrap_target",
            "wait 1 irq 0 side 0",
            "irq clear 0 side 0",
            "pull side 1",
            ".wrap",
        );

        let program_dotclk = common.load_program(&prg_dotclk.program);
        let program_hsync = common.load_program(&prg_hsync.program);
        let program_vsync = common.load_program(&prg_vsync.program);
        let program_data = common.load_program(&prog_data.program);

        let data_refs: [&Pin<'d, PIO>; 16] = core::array::from_fn(|idx| &data_pins[idx]);
        let clk_refs = [&dotclk];
        let hsync_refs = [&hsync];
        let vsync_refs = [&vsync];
        let de_refs = [&de];

        let mut cfg_dotclk = Config::default();
        cfg_dotclk.use_program(&program_dotclk, &clk_refs);
        cfg_dotclk.fifo_join = FifoJoin::TxOnly;

        let mut cfg_hsync = Config::default();
        cfg_hsync.use_program(&program_hsync, &hsync_refs);
        cfg_hsync.fifo_join = FifoJoin::TxOnly;

        let mut cfg_vsync = Config::default();
        cfg_vsync.use_program(&program_vsync, &vsync_refs);

        let mut cfg_data = Config::default();
        cfg_data.use_program(&program_data, &de_refs);
        cfg_data.set_out_pins(&data_refs);

        // Target ~12 MHz dot clock by default; adjust as needed
        let sys_khz = U24F8::from_num(clk_sys_freq() / 1000);
        let dotclk_khz = U24F8::from_num(100);
        cfg_dotclk.clock_divider = sys_khz / dotclk_khz;
        cfg_hsync.clock_divider = sys_khz / dotclk_khz;
        cfg_vsync.clock_divider = sys_khz / dotclk_khz;
        cfg_data.clock_divider = sys_khz / dotclk_khz;

        irq_flags.set(1);

        sm_dotclk.set_config(&cfg_dotclk);
        sm_dotclk.set_pin_dirs(pio::Direction::Out, &clk_refs);
        sm_dotclk.clear_fifos();
        sm_dotclk.tx().push(DOTCLK_PERIODS - 1);
        sm_dotclk.set_enable(true);
        
        sm_hsync.set_config(&cfg_hsync);
        sm_hsync.set_pin_dirs(pio::Direction::Out, &hsync_refs);
        sm_hsync.clear_fifos();
        sm_hsync.tx().push(HSYNC_LINES - 1);
        sm_hsync.tx().push(HSYNC_HIGH_PERIODS - 1);
        sm_hsync.set_enable(true);

        sm_vsync.set_config(&cfg_vsync);
        sm_vsync.set_pin_dirs(pio::Direction::Out, &vsync_refs);
        sm_vsync.set_enable(true);

        sm_data.set_config(&cfg_data);
        sm_data.set_pin_dirs(pio::Direction::Out, &data_refs);
        sm_data.set_pin_dirs(pio::Direction::Out, &de_refs);
        sm_data.clear_fifos();
        sm_data.set_enable(false);

        block_for(Duration::from_millis(1));

        Self {
            sm_dotclk,
            sm_hsync,
            sm_vsync,
            sm_data,
            dma: dma.into(),
            dma2: dma2.into(),
            irq_flags,
            cs,
            spi,
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

        self.write_spi(Cnd2BkxSel, &[0x77, 0x01, 0x00, 0x00, 0x10])?; // bank 0

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

        self.write_spi(Cnd2BkxSel, &[0x77, 0x01, 0x00, 0x00, 0x11])?; // bank 1
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

        self.write_spi(Cnd2BkxSel, &[0x77, 0x01, 0x00, 0x00, 0x13])?; // bank 3
        self.write_spi(RegE5, &[0xe4])?;

        self.write_spi(Cnd2BkxSel, &[0x77, 0x01, 0x00, 0x00, 0x00])?; // bank 0
        self.write_spi(ColMod, &[0x55])?;

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
        info!("Pushing data");
        self.irq_flags.clear(1);
        Ok(())
    }
}

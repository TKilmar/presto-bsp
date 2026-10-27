use defmt::info;
use embassy_hal_internal::Peri;
use embassy_rp::pio::{
        self,
        Common,
        Config,
        FifoJoin,
        Instance,
        Pin,
        ShiftConfig,
        StateMachine,
        program::{InstructionOperands, MovDestination, MovOperation, MovSource, pio_asm},
};
use embassy_rp::{
    clocks::clk_sys_freq,
    dma::{AnyChannel, Channel},
    gpio::Output,
    peripherals::SPI1,
    spi::{Blocking, Spi},
};
use fixed::{traits::ToFixed, types::U24F8};
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

#[allow(dead_code, missing_docs)]
#[derive(Copy, Clone)]
#[repr(u8)]
pub enum DataCommand {
    Cmd = 0x00,
    Data = 0x01
}

/// ST7701 register addresses.
#[allow(dead_code, missing_docs)]
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
pub struct St7701<'d, PIO: Instance, const SM0: usize, const SM1: usize> {
    sm_sync: StateMachine<'d, PIO, SM0>,
    sm_data: StateMachine<'d, PIO, SM1>,
    dma: Peri<'d, AnyChannel>,
    irq_flags: pio::IrqFlags<'d, PIO>,
    cs: Output<'d>,
    spi: Spi<'d, SPI1, Blocking>,
}

impl<'d, PIO: Instance, const SM0: usize, const SM1: usize>
    St7701<'d, PIO, SM0, SM1>
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
        mut sm_sync: StateMachine<'d, PIO, SM0>,
        mut sm_data: StateMachine<'d, PIO, SM1>,
        dma: Peri<'d, impl Channel>,
        data_pins: [Pin<'d, PIO>; 16],
        dotclk: Pin<'d, PIO>,
        hsync: Pin<'d, PIO>,
        vsync: Pin<'d, PIO>,
        de: Pin<'d, PIO>,
        cs: Output<'d>,
        spi: Spi<'d, SPI1, Blocking>,
    ) -> Self {

        // 10 instructions, 2 periods per instruction
        let prg_sync = pio_asm!(
            ".side_set 2",
            ".wrap_target",

            "set y, 12          side 0b11", // 12+1 for vbp
            "wait 0 irq 0       side 0b11", // idle
            "irq set 4          side 0b11", // sync with faster sm from slower sm

            "set x, 21          side 0b00", // vsync, 4 clks
            "line:"
            "jmp x--, line [5]  side 0b11"  // line 0, (21+1)*6*4=528 + 8 = 536, clks actual is 536
            
            "set x, 20          side 0b10", // hsync, 4 clks
            "jmp y--, line [5]  side 0b11", // repeat 12 times for vbp

            "mov y, osr         side 0b11", // load 482 lines
            "data:",
            "set x, 21          side 0b11", //
            "line2:"
            "jmp x--, line2 [5] side 0b11"  // line x, 22*5*4=528 clks
            "jmp y--, data      side 0b10", // hsync, 4 clks
            ".wrap"
        );
        
        // 20 instructions, 2 instructions per period
        let prg_data = pio_asm!(
            ".side_set 2",              // 0b CLK DE 
            
            ".wrap_target",
            "mov osr, rxfifo[0] side 0b00", // rx0 has vbp clks 6864
            "mov x, isr         side 0b00"  // isr has W&H 480
            "wait 1 irq 4   [2] side 0b00", // wait for frame start signal
            "out y, 32      [4] side 0b00",

            "vbp:",                        
            "nop                side 0b10",
            "jmp y--, vbp       side 0b00",
            
            "set y, 24          side 0b01", // hbp + hsync, 24 clocks
            "hbp:",                         
            "nop                side 0b10", 
            "jmp y--, hbp       side 0b00", 
            
            "mov y, isr         side 0b00", // outputs 480 data pixels
            "data:",                        
            "out pins, 16       side 0b01",
            "jmp y--, data      side 0b11",
            
            "set y, 24          side 0b00", // hfp, 24 clocks
            "hfp:",
            "nop                side 0b10", 
            "jmp y--, hfp       side 0b00",

            "jmp x--, hbp       side 0b00", // repeats for each data line

            "mov osr, rxfifo[1] side 0b00", // rx1 holds vfp clks, 1028
            "out y, 32          side 0b00",
            "vfp:",                        
            "nop                side 0b10",
            "jmp y--, vfp       side 0b00",
            "irq set 0          side 0b00",
            ".wrap"
        );

        let program_sync = common.load_program(&prg_sync.program);
        let program_data = common.load_program(&prg_data.program);

        let data_refs: [&Pin<'d, PIO>; 16] = core::array::from_fn(|idx| &data_pins[idx]);
        let clk_refs = [&de, &dotclk];
        let sync_refs = [&hsync, &vsync];

        let mut cfg_sync = Config::default();
        cfg_sync.use_program(&program_sync, &sync_refs);
        cfg_sync.fifo_join = FifoJoin::Duplex;

        let mut cfg_data = Config::default();
        cfg_data.use_program(&program_data, &clk_refs);
        cfg_data.set_out_pins(&data_refs);
        cfg_data.fifo_join = FifoJoin::RxAsControl;
        cfg_data.shift_out = ShiftConfig { 
            threshold: 32, 
            direction: pio::ShiftDirection::Right, 
            auto_fill: true 
        };
        
        // Target ~12 MHz dot clock by default; adjust as needed
        let sys_khz = U24F8::from_num(clk_sys_freq() / 1000);
        let dotclk_khz = U24F8::from_num(8_000);
        cfg_sync.clock_divider = (sys_khz / dotclk_khz * 4).to_fixed();
        cfg_data.clock_divider = (sys_khz / dotclk_khz / 2).to_fixed();

        irq_flags.set(0);

        let pull = InstructionOperands::PULL { if_empty: false, block: false }.encode();
        let mov_osr_to_isr = InstructionOperands::MOV {
            destination: MovDestination::ISR,
            op: MovOperation::None,
            source: MovSource::OSR,
        }.encode();

        sm_sync.set_config(&cfg_sync);
        sm_sync.set_pin_dirs(pio::Direction::Out, &sync_refs);
        sm_sync.tx().push(DISPLAY_WIDTH as u32);
        unsafe {
            sm_sync.exec_instr(pull);
        }
        
        sm_data.set_config(&cfg_data);
        sm_data.set_pin_dirs(pio::Direction::Out, &data_refs);
        sm_data.set_pin_dirs(pio::Direction::Out, &clk_refs);
        sm_data.clear_fifos();
        sm_data.set_rxf_entry(0, 6864);
        sm_data.set_rxf_entry(1, 1028);
        sm_data.tx().push((DISPLAY_WIDTH-1) as u32);
        unsafe {
            sm_data.exec_instr(pull);
            sm_data.exec_instr(mov_osr_to_isr);
        }

        common.apply_sm_batch(|batch| {
            batch.restart(&mut sm_data);
            batch.restart(&mut sm_sync);
            batch.set_enable(&mut sm_data, true);
            batch.set_enable(&mut sm_sync, true);
        });

        block_for(Duration::from_millis(1));

        Self {
            sm_sync,
            sm_data,
            dma: dma.into(),
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
        let words: &[u16] = unsafe { core::slice::from_raw_parts(pixels.as_ptr() as *const u16, expected) };
        let dma_tx = self.sm_data.tx().dma_push(self.dma.reborrow(), words, false);
        self.irq_flags.clear(0);
        dma_tx.await;
        Ok(())
    }
}

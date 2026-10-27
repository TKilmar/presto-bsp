//! ST7701 480x480 RGB panel driver with hardware-only scan-out.
//!
//! The Presto's panel has no frame memory of its own: it must be streamed a complete frame,
//! continuously, with correct HSYNC/VSYNC timing, forever. If the stream stops or the sync
//! signals drift relative to the pixel data the panel shows partial or torn images.
//!
//! This driver produces the stream without any CPU involvement once [`St7701::start`] has been
//! called, so neither core has to be dedicated to the display:
//!
//! * **PIO1 SM0 (timing)** generates the dot clock (side-set), HSYNC and VSYNC. It consumes one
//!   32-bit descriptor per scan line which says what the sync pins do during that line and which
//!   instruction to run at the start of the active region (`irq 4` to start a data line,
//!   `irq 0` to signal vertical blanking to the CPU, or a `nop`).
//! * **PIO1 SM1 (data)** waits for `irq 4` and then shifts out one line of RGB565 pixels on
//!   GPIO1..=16, two PIO cycles before every rising clock edge. Both state machines share a clock
//!   divider and are started in the same cycle, so the pixel phase is locked to the dot clock.
//! * **Four DMA channels** feed the two state machines. Channel *A* streams the frame buffer into
//!   the data SM and, when it finishes, chains to channel *B* which re-arms *A* with the frame
//!   buffer address. Channels *C*/*D* do the same with the per-line descriptor table for the
//!   timing SM. Both loops run forever without interrupts.
//!
//! The panel bus is wired MSB-first (GPIO1 carries bit 15 of the RGB565 value), so the data SM
//! bit-reverses each pixel with `mov pins, ::osr`; the frame buffer holds ordinary little-endian
//! [`Rgb565Pixel`] values that Slint can render into directly.
//!
//! The frame buffer (450 KiB) can live in SRAM, scanned out as above, or in PSRAM. PSRAM reads
//! over QSPI can take microseconds while the CPU uses the bus, which would stall the DMA engine
//! and the data state machine with it, so for a PSRAM frame buffer core 1 takes over the pixel
//! stream (see [`ring`]) and channels *A*/*B* are not used.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering, compiler_fence};

use embassy_rp::Peri;
use embassy_rp::clocks::clk_sys_freq;
use embassy_rp::dma::ChannelInstance;
use embassy_rp::gpio::Output;
use embassy_rp::pac;
use embassy_rp::multicore::{Stack, spawn_core1};
use embassy_rp::peripherals::{CORE1, DMA_CH1, DMA_CH2, DMA_CH3, DMA_CH4, DMA_CH8, PIO1, SPI1};
use embassy_rp::pio::program::{InstructionOperands, OutDestination, pio_asm};
use embassy_rp::pio::{
    self, Common, Config, FifoJoin, Irq, Pin, ShiftConfig, ShiftDirection, StateMachine,
};
use embassy_rp::psram::Psram;
use embassy_rp::spi::{Blocking, Spi};
use embassy_time::{Duration, block_for};
use fixed::types::U24F8;
pub use slint::platform::software_renderer::Rgb565Pixel;

/// Display width in pixels.
pub const DISPLAY_WIDTH: usize = 480;
/// Display height in pixels.
pub const DISPLAY_HEIGHT: usize = 480;
/// Number of pixels in one frame.
pub const FRAME_PIXELS: usize = DISPLAY_WIDTH * DISPLAY_HEIGHT;

/// Panel timing in dot clocks (horizontal) and lines (vertical).
///
/// These match the values Pimoroni's reference driver uses with `RGBCTRL` = `0x80 0x2e 0x0e`
/// (HV mode, HBP 46 = sync + back porch). They are baked into the timing PIO program, so they are
/// constants rather than runtime configuration.
pub mod timing {
    /// Horizontal front porch.
    pub const H_FRONT: usize = 4;
    /// HSYNC pulse width.
    pub const H_SYNC: usize = 16;
    /// Horizontal back porch.
    pub const H_BACK: usize = 30;
    /// Visible pixels per line.
    pub const H_ACTIVE: usize = super::DISPLAY_WIDTH;
    /// VSYNC pulse width in lines.
    pub const V_SYNC: usize = 8;
    /// Vertical back porch in lines.
    pub const V_BACK: usize = 5;
    /// Visible lines.
    pub const V_ACTIVE: usize = super::DISPLAY_HEIGHT;
    /// Vertical front porch in lines.
    pub const V_FRONT: usize = 5;
    /// Dot clocks per line.
    pub const H_TOTAL: usize = H_FRONT + H_SYNC + H_BACK + H_ACTIVE;
    /// Lines per frame.
    pub const V_TOTAL: usize = V_SYNC + V_BACK + V_ACTIVE + V_FRONT;
    /// Target dot clock for a frame buffer in SRAM. The PIO runs at four cycles per dot, so this
    /// is rounded down to `clk_sys / (4 * n)`: 16.67 MHz (63 Hz refresh) at 200 MHz.
    pub const MAX_DOT_CLOCK_HZ: u32 = 16_700_000;
    /// Target dot clock for a frame buffer in PSRAM: 12.5 MHz (47 Hz) at 200 MHz, i.e. 25 MB/s
    /// of the ~40 MB/s PSRAM can deliver.
    pub const MAX_DOT_CLOCK_HZ_PSRAM: u32 = 12_500_000;
}

const TIMING_SM: usize = 0;
const DATA_SM: usize = 1;
/// PIO IRQ flag the timing SM raises at the start of each active line (internal to PIO1).
const LINE_IRQ: u8 = 4;
/// PIO IRQ flag raised once per frame at the start of the vertical front porch.
const VBLANK_IRQ: u8 = 0;
/// PIO IRQ flag raised once per frame, one line after [`VBLANK_IRQ`]. Core 1 restarts the
/// pixel stream on it (PSRAM frame buffers only).
const FRAME_IRQ: u8 = 1;
/// DREQ numbers for PIO1 TX FIFOs (RP2350 datasheet, DMA DREQ table).
const DREQ_PIO1_TX0: u8 = 8;
/// "Permanent request" DREQ: transfer as fast as possible.
const DREQ_FORCE: u8 = 0x3f;

/// Instructions executed by the timing SM at the start of a line's active region. They are run
/// via `out exec`, so they must carry the program's side-set bit (bit 12, PCLK low) themselves.
mod exec {
    /// `irq set 4 side 0`: start shifting a line of pixels.
    pub const LINE: u16 = 0xC000 | super::LINE_IRQ as u16;
    /// `irq set 0 side 0`: vertical blanking has started.
    pub const VBLANK: u16 = 0xC000 | super::VBLANK_IRQ as u16;
    /// `irq set 1 side 0`: time to restart the pixel stream for the next frame.
    pub const FRAME: u16 = 0xC000 | super::FRAME_IRQ as u16;
    /// `mov y, y side 0`: no-op.
    pub const NOP: u16 = 0xA042;
}

/// Build the per-line descriptor table consumed by the timing SM.
///
/// Bit layout (shifted out LSB first, autopull threshold 22):
/// `[1:0]` HSYNC/VSYNC during the front porch, `[3:2]` during the sync pulse,
/// `[5:4]` during back porch + active, `[21:6]` instruction to `exec` at the start of the active
/// region. Both sync signals are active low.
const fn build_line_table() -> [u32; timing::V_TOTAL] {
    let mut table = [0u32; timing::V_TOTAL];
    let mut line = 0;
    while line < timing::V_TOTAL {
        let vs: u32 = if line < timing::V_SYNC { 0 } else { 1 };
        let first_active = timing::V_SYNC + timing::V_BACK;
        let instr = if line >= first_active && line < first_active + timing::V_ACTIVE {
            exec::LINE
        } else if line == first_active + timing::V_ACTIVE {
            exec::VBLANK
        } else if line == first_active + timing::V_ACTIVE + 1 {
            exec::FRAME
        } else {
            exec::NOP
        };
        let front = 0b01 | (vs << 1); // HSYNC high
        let sync = vs << 1; // HSYNC low
        let back = 0b01 | (vs << 1);
        table[line] = front | (sync << 2) | (back << 4) | ((instr as u32) << 6);
        line += 1;
    }
    table
}

/// 4-byte aligned so DMA can move whole words.
#[repr(C, align(4))]
struct LineTable([u32; timing::V_TOTAL]);

static LINE_TABLE: LineTable = LineTable(build_line_table());
/// Read by DMA channel *D* to re-arm channel *C* at the end of every frame.
static LINE_TABLE_ADDR: AtomicU32 = AtomicU32::new(0);
/// Read by DMA channel *B* to re-arm channel *A* at the end of every frame. Writing it switches
/// the scanned-out buffer at the next frame boundary.
static FRAME_ADDR: AtomicU32 = AtomicU32::new(0);

/// PSRAM frame buffers are not scanned out by DMA. PSRAM shares the QSPI bus with code running
/// from flash and with the CPU drawing, so a PSRAM read can take microseconds, and while the
/// DMA engine waits for one it cannot feed the panel either: the data state machine's FIFO
/// lasts only 1.3 µs.
///
/// Instead core 1 is dedicated to the panel, like in Pimoroni's driver:
///
/// * DMA channel 8 copies the frame from PSRAM into this SRAM ring in [`ring::CHUNK`]-word
///   pieces, keeping up to [`ring::LEAD`] words ahead. A slow copy only delays the copy.
/// * Core 1 itself moves words from the ring into the data state machine's FIFO, running from
///   SRAM with interrupts off, so nothing it needs is ever behind the QSPI bus. If the copy
///   falls behind it sends the stale ring contents rather than stalling (a few wrong pixels for
///   one frame, counted by [`St7701::underruns`]).
/// * At the end of every frame it resets the data state machine and starts again from the first
///   pixel, so a glitch can never shift the picture.
pub mod ring {
    /// Ring size in 32-bit words (64 KiB, ~68 lines).
    pub const WORDS: u32 = 16384;
    /// Words copied per DMA transfer (divides both the ring and the frame).
    pub const CHUNK: u32 = 256;
    /// How far the copy runs ahead of the scan-out, in words (~2.4 ms at 47 Hz).
    pub const LEAD: u32 = 15360;
}

#[repr(C, align(4))]
struct Ring([u32; ring::WORDS as usize]);

/// Address of the ring; set by [`FrameBuffer::in_psram`], which owns the ring's storage so that
/// firmware with an SRAM frame buffer does not link it.
static RING_ADDR: AtomicU32 = AtomicU32::new(0);
/// Times the copy fell behind the scan-out (diagnostics).
static UNDERRUNS: AtomicU32 = AtomicU32::new(0);
/// Times the data state machine ran out of pixels mid-frame (diagnostics).
static STALLS: AtomicU32 = AtomicU32::new(0);
/// Frames that had not been fully sent when the next one was due (diagnostics).
static LATE_FRAMES: AtomicU32 = AtomicU32::new(0);
/// How many words the ring is ahead of the scan-out; `u32::MAX` while no ring is in use.
/// Updated continuously by core 1. CPU writes to a PSRAM frame buffer wait while it is low.
static RING_LEAD: AtomicU32 = AtomicU32::new(u32::MAX);
static mut CORE1_STACK: Stack<2048> = Stack::new();
/// DMA channel core 1 uses to copy PSRAM into the ring.
const COPY_DMA: u8 = 8;
const FRAME_WORDS: u32 = (FRAME_PIXELS / 2) as u32;

/// Start copying `words` words from `src` to `dst` with the copy channel (does not wait).
#[inline(always)]
fn start_copy(src: u32, dst: u32, words: u32) {
    use pac::dma::vals::{DataSize, TreqSel};
    let ch = pac::DMA.ch(COPY_DMA as usize);
    ch.read_addr().write_value(src);
    ch.write_addr().write_value(dst);
    ch.trans_count().write(|w| w.set_count(words));
    compiler_fence(Ordering::SeqCst);
    ch.ctrl_trig().write(|w| {
        w.set_treq_sel(TreqSel::from(DREQ_FORCE));
        w.set_data_size(DataSize::SIZE_WORD);
        w.set_incr_read(true);
        w.set_incr_write(true);
        w.set_chain_to(COPY_DMA);
        w.set_en(true);
    });
}

/// Put the data state machine back at the start of its program with empty FIFOs. The clock
/// divider keeps running, so the pixel phase relative to the dot clock is unchanged.
#[inline(always)]
fn reset_data_sm(data_origin: u8) {
    let pio = pac::PIO1;
    pio.ctrl().modify(|w| w.set_sm_enable(w.sm_enable() & !(1 << DATA_SM)));
    // Toggling FJOIN_RX twice empties both FIFOs.
    let shift = pio.sm(DATA_SM).shiftctrl();
    shift.modify(|w| w.set_fjoin_rx(!w.fjoin_rx()));
    shift.modify(|w| w.set_fjoin_rx(!w.fjoin_rx()));
    // Restart empties the OSR/ISR and keeps Y (the line length).
    pio.ctrl().modify(|w| w.set_sm_restart(1 << DATA_SM));
    pio.sm(DATA_SM).instr().write(|w| w.set_instr(data_origin as u16)); // jmp origin
    pio.irq().write(|w| w.set_irq(1 << LINE_IRQ));
    pio.fdebug().write(|w| w.set_txstall(1 << DATA_SM));
    pio.ctrl().modify(|w| w.set_sm_enable(w.sm_enable() | (1 << DATA_SM)));
}

/// Core 1: copy the frame into the ring and feed the ring to the data state machine, forever.
///
/// Runs from SRAM with everything inlined and interrupts off: it has 32 CPU cycles per word,
/// and core 0's code evicts this loop from the XIP cache while rendering.
#[inline(never)]
#[unsafe(link_section = ".data.lcd_pump")]
fn pump(data_origin: u8) -> ! {
    cortex_m::interrupt::disable();
    let pio = pac::PIO1;
    let copy = pac::DMA.ch(COPY_DMA as usize);
    let txf = pio.txf(DATA_SM).as_ptr();
    let ring = RING_ADDR.load(Ordering::Acquire) as *mut u32;
    let full_bit = 1 << DATA_SM;

    let mut frame = FRAME_ADDR.load(Ordering::Acquire);
    // Words sent to the state machine, copy issued up to, copy completed up to (this frame).
    let mut sent = 0u32;
    let mut issued = 0u32;
    let mut copied = 0u32;
    let mut copying = false;
    // All of this frame sent; waiting for the next frame's start (copy for it may already run).
    let mut waiting = true;
    loop {
        if copying && !copy.ctrl_trig().read().busy() {
            copying = false;
            copied = issued;
        }
        if !copying && issued < FRAME_WORDS && issued < sent + ring::LEAD {
            if issued < sent {
                // The copy fell behind; skip to where the scan-out is.
                UNDERRUNS.fetch_add(1, Ordering::Relaxed);
                issued = sent.next_multiple_of(ring::CHUNK);
                copied = issued;
            } else {
                let dst = ring as u32 + (issued % ring::WORDS) * 4;
                start_copy(frame + issued * 4, dst, ring::CHUNK);
                issued += ring::CHUNK;
                copying = true;
            }
        }
        RING_LEAD.store(if copied >= FRAME_WORDS { u32::MAX } else { copied.saturating_sub(sent) }, Ordering::Relaxed);

        if pio.irq().read().irq() & (1 << FRAME_IRQ) != 0 {
            pio.irq().write(|w| w.set_irq(1 << FRAME_IRQ));
            if !waiting {
                // Still sending the last frame: drop the rest and start over.
                LATE_FRAMES.fetch_add(1, Ordering::Relaxed);
                while copy.ctrl_trig().read().busy() {}
                copying = false;
                frame = FRAME_ADDR.load(Ordering::Acquire);
                (sent, issued, copied) = (0, 0, 0);
            }
            reset_data_sm(data_origin);
            waiting = false;
            continue;
        }
        if waiting {
            continue;
        }
        // Top up the state machine's FIFO.
        while sent < FRAME_WORDS && pio.fstat().read().txfull() & full_bit == 0 {
            if sent == 8 {
                // The state machine stalls by design at the start of the frame.
                pio.fdebug().write(|w| w.set_txstall(full_bit));
            }
            if sent > 8 && pio.fdebug().read().txstall() & full_bit != 0 {
                STALLS.fetch_add(1, Ordering::Relaxed);
                pio.fdebug().write(|w| w.set_txstall(full_bit));
            }
            // SAFETY: in bounds; the copy DMA may be writing other words of the ring.
            let word = unsafe { core::ptr::read_volatile(ring.add((sent % ring::WORDS) as usize)) };
            unsafe { core::ptr::write_volatile(txf, word) };
            sent += 1;
        }
        if sent == FRAME_WORDS {
            // Frame sent. Start copying the next one during vertical blanking.
            waiting = true;
            while copy.ctrl_trig().read().busy() {}
            copying = false;
            frame = FRAME_ADDR.load(Ordering::Acquire);
            (sent, issued, copied) = (0, 0, 0);
        }
    }
}

/// A full-screen RGB565 frame buffer, scanned out continuously by the display hardware.
///
/// It can live in one of two places:
///
/// * **SRAM** ([`FrameBuffer::take`]): 450 KiB of the 520 KiB, scanned out at 63 Hz. Little SRAM
///   is left for anything else.
/// * **PSRAM** ([`FrameBuffer::in_psram`]): frees the SRAM, but uses core 1 and DMA channel 8 to
///   feed the panel (see [`ring`]). PSRAM delivers about 40 MB/s, so scan-out runs at a
///   12.5 MHz dot clock (47 Hz, 25 MB/s) to leave headroom for the CPU drawing into it. Draw
///   with [`FrameBuffer::write_span`]. Flash erase/program stalls PSRAM too, so expect a brief
///   glitch while writing flash (do it with the backlight off).
///
/// Note that the RP2350's exclusive monitor only covers SRAM: atomic read-modify-write
/// operations (e.g. `Arc`/`Rc`-style reference counts used by Slint) never complete on PSRAM, so
/// heaps belong in SRAM and the frame buffer is what to move out.
///
/// The driver reads the buffer with DMA while the application draws into it through
/// [`FrameBuffer::pixels_mut`]; drawing mid-frame can briefly show a partially drawn region
/// (tearing), which [`St7701::wait_vblank`] can be used to avoid.
pub struct FrameBuffer {
    pixels: *mut Rgb565Pixel,
}

#[repr(C, align(4))]
struct SramFrameBuffer([Rgb565Pixel; FRAME_PIXELS]);

static mut SRAM_FRAME_BUFFER: SramFrameBuffer = SramFrameBuffer([Rgb565Pixel(0); FRAME_PIXELS]);
static SRAM_TAKEN: AtomicBool = AtomicBool::new(false);
static PSRAM_TAKEN: AtomicBool = AtomicBool::new(false);

impl FrameBuffer {
    /// Take the SRAM frame buffer. Returns `None` if it was already taken.
    ///
    /// The 450 KiB buffer is only linked into firmware that calls this.
    pub fn take() -> Option<FrameBuffer> {
        if SRAM_TAKEN.swap(true, Ordering::AcqRel) {
            return None;
        }
        let pixels = unsafe { core::ptr::addr_of_mut!(SRAM_FRAME_BUFFER.0) } as *mut Rgb565Pixel;
        Some(FrameBuffer { pixels })
    }

    /// Use the first 450 KiB of PSRAM as the frame buffer (cleared to black). Returns `None` if
    /// it was already taken. The PSRAM driver must stay alive while the display runs; the rest
    /// of the PSRAM (from [`FrameBuffer::PSRAM_BYTES`]) remains free.
    pub fn in_psram(psram: &Psram<'_>) -> Option<FrameBuffer> {
        if psram.size() < Self::PSRAM_BYTES || PSRAM_TAKEN.swap(true, Ordering::AcqRel) {
            return None;
        }
        // Use the uncached alias (0x15xx_xxxx) for both DMA and CPU: the XIP cache is shared with
        // code running from flash, and streaming 450 KiB per frame through it would evict the
        // program and slow the CPU down several times over.
        let uncached = psram.base_address() as usize - 0x1100_0000 + 0x1500_0000;
        let pixels = uncached as *mut Rgb565Pixel;
        static mut RING: Ring = Ring([0; ring::WORDS as usize]);
        RING_ADDR.store(unsafe { core::ptr::addr_of_mut!(RING.0) } as u32, Ordering::Release);
        let mut fb = FrameBuffer { pixels };
        fb.pixels_mut().fill(Rgb565Pixel(0));
        Some(fb)
    }

    /// Bytes of PSRAM [`FrameBuffer::in_psram`] uses.
    pub const PSRAM_BYTES: usize = FRAME_PIXELS * 2;

    /// Mutable access to all pixels, row-major, `DISPLAY_WIDTH` pixels per row.
    pub fn pixels_mut(&mut self) -> &mut [Rgb565Pixel] {
        // SAFETY: `pixels` points at FRAME_PIXELS pixels owned by this handle.
        unsafe { core::slice::from_raw_parts_mut(self.pixels, FRAME_PIXELS) }
    }

    /// Copy `pixels` into row `y` starting at column `x`.
    ///
    /// For a PSRAM frame buffer this is the way to draw: it writes in short bursts and pauses
    /// whenever the scan-out ring runs low, so drawing never starves the display (writing pixel
    /// by pixel straight into PSRAM saturates the QSPI bus). Render into SRAM first, e.g. with
    /// Slint's `render_by_line`.
    pub fn write_span(&mut self, y: usize, x: usize, pixels: &[Rgb565Pixel]) {
        let start = y * DISPLAY_WIDTH + x;
        let in_psram = self.is_in_psram();
        let dst = &mut self.pixels_mut()[start..start + pixels.len()];
        if !in_psram {
            dst.copy_from_slice(pixels);
            return;
        }
        const BURST: usize = 32;
        for (dst, src) in dst.chunks_mut(BURST).zip(pixels.chunks(BURST)) {
            while RING_LEAD.load(Ordering::Relaxed) < ring::LEAD / 4 * 3 {
                core::hint::spin_loop();
            }
            dst.copy_from_slice(src);
        }
    }

    /// Whether the buffer is in PSRAM (scanned out at the lower dot clock).
    pub fn is_in_psram(&self) -> bool {
        (0x1000_0000..0x2000_0000).contains(&(self.pixels as u32))
    }

    fn address(&self) -> u32 {
        self.pixels as u32
    }
}

/// Errors raised by the LCD driver.
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    /// [`St7701::start`] was called while scan-out is already running.
    AlreadyRunning,
}

/// ST7701 register addresses used during initialisation.
#[allow(missing_docs)]
#[derive(Copy, Clone)]
#[repr(u8)]
pub enum Register {
    SwReset = 0x01,
    SleepIn = 0x10,
    SleepOut = 0x11,
    InvOn = 0x21,
    DisplayOff = 0x28,
    DisplayOn = 0x29,
    MadCtl = 0x36,
    ColMod = 0x3A,
    Cnd2BkxSel = 0xFF,
    RegB0 = 0xB0,
    RegB1 = 0xB1,
    RegB2 = 0xB2,
    RegB3 = 0xB3,
    RegB5 = 0xB5,
    RegB7 = 0xB7,
    RegB8 = 0xB8,
    RegC0 = 0xC0,
    RegC1 = 0xC1,
    RegC2 = 0xC2,
    RegC3 = 0xC3,
    RegC7 = 0xC7,
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

/// Pins and peripherals the driver needs.
pub struct St7701Pins<'d> {
    /// RGB565 data bus, GPIO1..=GPIO16, as PIO1 pins.
    pub data: [Pin<'d, PIO1>; 16],
    /// GPIO19 as a PIO1 pin.
    pub hsync: Pin<'d, PIO1>,
    /// GPIO20 as a PIO1 pin.
    pub vsync: Pin<'d, PIO1>,
    /// GPIO22 as a PIO1 pin.
    pub dotclk: Pin<'d, PIO1>,
    /// GPIO21 (data enable). Held high: the panel runs in HV mode.
    pub de: Output<'d>,
    /// GPIO17/18, the two least significant bits of the panel's 18-bit bus. Held low.
    pub unused_lsbs: [Output<'d>; 2],
    /// GPIO44, panel reset (active low).
    pub reset: Output<'d>,
    /// GPIO28, SPI chip select for the panel's command interface.
    pub spi_cs: Output<'d>,
    /// SPI1 on GPIO26 (CLK) / GPIO27 (MOSI), configured for 9-bit frames.
    pub spi: Spi<'d, SPI1, Blocking>,
}

/// DMA channels used for scan-out: pixels, pixel re-arm, line table, line table re-arm.
/// They run without interrupts.
pub type ScanoutDma<'d> = (
    Peri<'d, DMA_CH1>,
    Peri<'d, DMA_CH2>,
    Peri<'d, DMA_CH3>,
    Peri<'d, DMA_CH4>,
);

const DMA_CHANNELS: [u8; 4] = [1, 2, 3, 4];

fn check_dma_channels() {
    debug_assert_eq!(
        DMA_CHANNELS,
        [
            <DMA_CH1 as ChannelInstance>::number(),
            <DMA_CH2 as ChannelInstance>::number(),
            <DMA_CH3 as ChannelInstance>::number(),
            <DMA_CH4 as ChannelInstance>::number(),
        ]
    );
}

/// ST7701 driver: panel initialisation over 9-bit SPI plus continuous PIO/DMA scan-out.
pub struct St7701<'d> {
    sm_timing: StateMachine<'d, PIO1, TIMING_SM>,
    sm_data: StateMachine<'d, PIO1, DATA_SM>,
    /// Program start addresses in PIO instruction memory.
    origins: [u8; 2],
    vblank: Irq<'d, PIO1, { VBLANK_IRQ as usize }>,
    _dma: ScanoutDma<'d>,
    pins: St7701Pins<'d>,
    running: bool,
    /// Core 1 and its copy channel, used when the frame buffer is in PSRAM.
    core1: Option<Peri<'static, CORE1>>,
    _copy_dma: Peri<'static, DMA_CH8>,
}

impl<'d> St7701<'d> {
    /// Load the PIO programs and configure both state machines. Nothing is driven onto the panel
    /// bus until [`start`](Self::start).
    ///
    pub fn new(
        common: &mut Common<'d, PIO1>,
        mut sm_timing: StateMachine<'d, PIO1, TIMING_SM>,
        mut sm_data: StateMachine<'d, PIO1, DATA_SM>,
        vblank: Irq<'d, PIO1, { VBLANK_IRQ as usize }>,
        dma: ScanoutDma<'d>,
        pins: St7701Pins<'d>,
        core1: Peri<'static, CORE1>,
        copy_dma: Peri<'static, DMA_CH8>,
    ) -> Self {
        // Four PIO cycles per dot: two with PCLK low, two with PCLK high. Every phase is
        // `out pins` + `set x` (one dot) followed by `x + 1` dots of loop, so the immediates are
        // phase length - 2. The timing SM raises `irq 4` one dot before the end of the panel's
        // back porch (the "back" phase is one dot short and "active" one dot long); the data
        // SM's `wait` then puts pixel 0 on the bus at the falling clock edge just before the
        // panel latches it, which is the 47th rising edge after HSYNC falls (HBP = 46).
        const _: () = assert!(timing::H_FRONT - 2 == 2);
        const _: () = assert!(timing::H_SYNC - 2 == 14);
        const _: () = assert!(timing::H_BACK - 1 - 2 == 27);
        let prg_timing = pio_asm!(
            ".side_set 1",
            ".wrap_target",
            "    out pins, 2     side 0 [1]", // front porch
            "    set x, 2        side 1 [1]",
            "front:",
            "    nop             side 0 [1]",
            "    jmp x-- front   side 1 [1]",
            "    out pins, 2     side 0 [1]", // sync pulse
            "    set x, 14       side 1 [1]",
            "sync:",
            "    nop             side 0 [1]",
            "    jmp x-- sync    side 1 [1]",
            "    out pins, 2     side 0 [1]", // back porch
            "    set x, 27       side 1 [1]",
            "back:",
            "    nop             side 0 [1]",
            "    jmp x-- back    side 1 [1]",
            "    out exec, 16    side 0",     // exec'd instruction takes the second low cycle
            "    mov x, y        side 1 [1]", // y = H_ACTIVE - 1
            "active:",
            "    nop             side 0 [1]",
            "    jmp x-- active  side 1 [1]",
            ".wrap",
        );

        // Pixels arrive two per word (p1:p0). OSR shifts left so `out isr, 16` moves p1 into the
        // ISR and leaves p0 in the OSR's top half; `mov pins, ::osr` bit-reverses, which puts the
        // top half, reversed, onto the 16 data pins. The next word is fetched during the last
        // pixel of the previous one, which carries across lines and frames because the DMA
        // stream never pauses.
        let prg_data = pio_asm!(
            "    out isr, 16",
            ".wrap_target",
            "    mov x, y",            // y = DISPLAY_WIDTH / 2 - 1
            "    wait 1 irq 4 [2]",
            "pixels:",
            "    mov pins, ::osr [1]", // even pixel, PCLK falling edge
            "    out null, 16",
            "    in null, 16",
            "    mov pins, ::isr [1]", // odd pixel
            "    out isr, 16",
            "    jmp x-- pixels",
            ".wrap",
        );

        let program_timing = common.load_program(&prg_timing.program);
        let program_data = common.load_program(&prg_data.program);

        // Replaced in `start`, depending on where the frame buffer lives.
        let clock_divider = Self::clock_divider(timing::MAX_DOT_CLOCK_HZ);

        let mut cfg = Config::default();
        cfg.use_program(&program_timing, &[&pins.dotclk]);
        cfg.set_out_pins(&[&pins.hsync, &pins.vsync]);
        cfg.fifo_join = FifoJoin::TxOnly;
        cfg.shift_out = ShiftConfig {
            threshold: 22,
            direction: ShiftDirection::Right,
            auto_fill: true,
        };
        cfg.clock_divider = clock_divider;
        sm_timing.set_config(&cfg);
        sm_timing.set_pin_dirs(pio::Direction::Out, &[&pins.hsync, &pins.vsync, &pins.dotclk]);

        let data_refs: [&Pin<'d, PIO1>; 16] = core::array::from_fn(|i| &pins.data[i]);
        let mut cfg = Config::default();
        cfg.use_program(&program_data, &[]);
        cfg.set_out_pins(&data_refs);
        cfg.fifo_join = FifoJoin::TxOnly;
        cfg.shift_out = ShiftConfig {
            threshold: 32,
            direction: ShiftDirection::Left,
            auto_fill: true,
        };
        cfg.shift_in = ShiftConfig {
            threshold: 32,
            direction: ShiftDirection::Left,
            auto_fill: false,
        };
        cfg.clock_divider = clock_divider;
        sm_data.set_config(&cfg);
        sm_data.set_pin_dirs(pio::Direction::Out, &data_refs);

        Self {
            sm_timing,
            sm_data,
            origins: [program_timing.origin, program_data.origin],
            vblank,
            _dma: dma,
            pins,
            running: false,
            core1: Some(core1),
            _copy_dma: copy_dma,
        }
    }

    /// PIO clock divider for a dot clock of at most `dot_clock_hz` (four PIO cycles per dot).
    fn clock_divider(dot_clock_hz: u32) -> U24F8 {
        U24F8::from_num(clk_sys_freq().div_ceil(4 * dot_clock_hz))
    }

    /// Write a command and its parameters over the 9-bit SPI interface (bit 8 = data/command).
    fn command(&mut self, reg: Register, data: &[u8]) {
        let p = pac::SPI1;
        self.pins.spi_cs.set_low();
        for word in core::iter::once(reg as u16).chain(data.iter().map(|&b| 0x100 | b as u16)) {
            while !p.sr().read().tnf() {}
            p.dr().write(|w| w.set_data(word));
            while !p.sr().read().rne() {}
            let _ = p.dr().read();
        }
        while p.sr().read().bsy() {}
        self.pins.spi_cs.set_high();
    }

    /// Reset the panel and run its power-on register sequence (blocking, about 0.5 s).
    ///
    /// The sequence is the one Pimoroni ships for the Presto's TL040WVS03 panel.
    pub fn init(&mut self) {
        use Register::*;

        self.pins.reset.set_low();
        block_for(Duration::from_millis(10));
        self.pins.reset.set_high();
        block_for(Duration::from_millis(120));

        self.command(SwReset, &[]);
        block_for(Duration::from_millis(150));

        self.command(Cnd2BkxSel, &[0x77, 0x01, 0x00, 0x00, 0x10]); // command2 bank 0
        self.command(RegC0, &[0x3b, 0x00]); // 480 lines
        self.command(RegC1, &[0x0d, 0x02]); // porch control
        self.command(RegC2, &[0x31, 0x01]); // inversion / frame rate
        self.command(RegCD, &[0x08]);
        self.command(
            RegB0,
            &[0x00, 0x11, 0x18, 0x0e, 0x11, 0x06, 0x07, 0x08, 0x07, 0x22, 0x04, 0x12, 0x0f, 0xaa, 0x31, 0x18],
        );
        self.command(
            RegB1,
            &[0x00, 0x11, 0x19, 0x0e, 0x12, 0x07, 0x08, 0x08, 0x08, 0x22, 0x04, 0x11, 0x11, 0xa9, 0x32, 0x18],
        );
        // RGBCTRL: HV mode, sync active low, latch on rising PCLK; HBP 46 dots, VBP 14 lines.
        self.command(RegC3, &[0x80, 0x2e, 0x0e]);

        self.command(Cnd2BkxSel, &[0x77, 0x01, 0x00, 0x00, 0x11]); // command2 bank 1
        self.command(RegB0, &[0x60]);
        self.command(RegB1, &[0x32]);
        self.command(RegB2, &[0x07]);
        self.command(RegB3, &[0x80]);
        self.command(RegB5, &[0x49]);
        self.command(RegB7, &[0x85]);
        self.command(RegB8, &[0x21]);
        self.command(RegC1, &[0x78]);
        self.command(RegC2, &[0x78]);

        // Gate timing sequence specific to the TL040WVS03 glass.
        self.command(RegE0, &[0x00, 0x1b, 0x02]);
        self.command(RegE1, &[0x08, 0xa0, 0x00, 0x00, 0x07, 0xa0, 0x00, 0x00, 0x00, 0x44, 0x44]);
        self.command(RegE2, &[0x11, 0x11, 0x44, 0x44, 0xed, 0xa0, 0x00, 0x00, 0xec, 0xa0, 0x00, 0x00]);
        self.command(RegE3, &[0x00, 0x00, 0x11, 0x11]);
        self.command(RegE4, &[0x44, 0x44]);
        self.command(
            RegE5,
            &[0x0a, 0xe9, 0xd8, 0xa0, 0x0c, 0xeb, 0xd8, 0xa0, 0x0e, 0xed, 0xd8, 0xa0, 0x10, 0xef, 0xd8, 0xa0],
        );
        self.command(RegE6, &[0x00, 0x00, 0x11, 0x11]);
        self.command(RegE7, &[0x44, 0x44]);
        self.command(
            RegE8,
            &[0x09, 0xe8, 0xd8, 0xa0, 0x0b, 0xea, 0xd8, 0xa0, 0x0d, 0xec, 0xd8, 0xa0, 0x0f, 0xee, 0xd8, 0xa0],
        );
        self.command(RegEB, &[0x02, 0x00, 0xe4, 0xe4, 0x88, 0x00, 0x40]);
        self.command(RegEC, &[0x3c, 0x00]);
        self.command(
            RegED,
            &[0xab, 0x89, 0x76, 0x54, 0x02, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x20, 0x45, 0x67, 0x98, 0xba],
        );
        self.command(MadCtl, &[0x00]);

        self.command(Cnd2BkxSel, &[0x77, 0x01, 0x00, 0x00, 0x13]); // command2 bank 3
        self.command(RegE5, &[0xe4]);

        self.command(Cnd2BkxSel, &[0x77, 0x01, 0x00, 0x00, 0x00]); // command2 off
        // 18 bits per pixel. The two LSB lines are held low, so the panel sees RGB565 data.
        self.command(ColMod, &[0x66]);
        self.command(MadCtl, &[0x00]);
        self.command(Cnd2BkxSel, &[0x77, 0x01, 0x00, 0x00, 0x10]);
        self.command(RegC7, &[0x00]); // SDIR: no X mirror
        self.command(Cnd2BkxSel, &[0x77, 0x01, 0x00, 0x00, 0x00]);

        self.command(InvOn, &[]);
        block_for(Duration::from_millis(1));
        self.command(SleepOut, &[]);
        block_for(Duration::from_millis(120));
        self.command(DisplayOn, &[]);
        block_for(Duration::from_millis(50));
    }

    /// Start continuous scan-out of `frame_buffer`. Runs until the driver is dropped.
    pub fn start(&mut self, frame_buffer: &FrameBuffer) -> Result<(), Error> {
        if self.running {
            return Err(Error::AlreadyRunning);
        }
        check_dma_channels();
        let dot_clock = if frame_buffer.is_in_psram() {
            timing::MAX_DOT_CLOCK_HZ_PSRAM
        } else {
            timing::MAX_DOT_CLOCK_HZ
        };
        let divider = Self::clock_divider(dot_clock);
        self.sm_timing.set_clock_divider(divider);
        self.sm_data.set_clock_divider(divider);
        FRAME_ADDR.store(frame_buffer.address(), Ordering::Release);
        LINE_TABLE_ADDR.store(LINE_TABLE.0.as_ptr() as u32, Ordering::Release);

        // Preload the per-line loop counters into Y through the TX FIFO.
        let pull = InstructionOperands::PULL { if_empty: false, block: true }.encode();
        let out_y = InstructionOperands::OUT { destination: OutDestination::Y, bit_count: 32 }.encode();
        self.sm_timing.restart();
        self.sm_data.restart();
        self.sm_timing.clear_fifos();
        self.sm_data.clear_fifos();
        self.sm_timing.tx().push(timing::H_ACTIVE as u32 - 1);
        self.sm_data.tx().push(DISPLAY_WIDTH as u32 / 2 - 1);
        unsafe {
            self.sm_timing.exec_jmp(self.origins[0]);
            self.sm_data.exec_jmp(self.origins[1]);
            self.sm_timing.exec_instr(pull);
            self.sm_timing.exec_instr(out_y);
            self.sm_data.exec_instr(pull);
            self.sm_data.exec_instr(out_y);
        }

        // Prioritise DMA on the bus fabric so CPU traffic cannot starve the panel.
        pac::BUSCTRL.bus_priority().write(|w| {
            w.set_dma_r(true);
            w.set_dma_w(true);
        });

        let [px, px_rearm, lines, lines_rearm] = DMA_CHANNELS;
        let txf = |sm: usize| pac::PIO1.txf(sm).as_ptr() as u32;
        if frame_buffer.is_in_psram() {
            // Core 1 feeds the data state machine from the SRAM ring; it starts with the first
            // full frame (the first frame stays black).
            pac::PIO1.irq().write(|w| w.set_irq(1 << FRAME_IRQ));
            let data_origin = self.origins[1];
            if let Some(core1) = self.core1.take() {
                // SAFETY: the stack is only ever handed to core 1 once (`core1` is taken).
                let stack = unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) };
                spawn_core1(core1, stack, move || pump(data_origin));
            }
        } else {
            arm_loop(px, px_rearm, &FRAME_ADDR, txf(DATA_SM), FRAME_WORDS, DREQ_PIO1_TX0 + DATA_SM as u8, 0);
        }
        arm_loop(
            lines,
            lines_rearm,
            &LINE_TABLE_ADDR,
            txf(TIMING_SM),
            timing::V_TOTAL as u32,
            DREQ_PIO1_TX0 + TIMING_SM as u8,
            0,
        );

        // Start both state machines on the same PIO clock edge so they stay phase locked.
        pac::PIO1.ctrl().modify(|w| {
            w.set_clkdiv_restart((1 << TIMING_SM) | (1 << DATA_SM));
            w.set_sm_enable(w.sm_enable() | (1 << TIMING_SM) | (1 << DATA_SM));
        });
        self.running = true;
        Ok(())
    }

    /// Times the core-1 ring copy fell behind the scan-out since start (PSRAM frame buffers
    /// only). Each shows as a few stale pixels, e.g. while flash is being written.
    pub fn underruns(&self) -> u32 {
        UNDERRUNS.load(Ordering::Relaxed)
    }

    /// Times the data state machine ran out of pixels mid-line since start (PSRAM frame buffers
    /// only). Each shows as a line whose remaining pixels are shifted right for one frame.
    pub fn stalls(&self) -> u32 {
        STALLS.load(Ordering::Relaxed)
    }

    /// Frames that were still being sent when the next one was due (PSRAM frame buffers only).
    /// The scan-out restarts every frame, so each is a one-frame glitch.
    pub fn late_frames(&self) -> u32 {
        LATE_FRAMES.load(Ordering::Relaxed)
    }

    /// Show `frame_buffer` from the next frame on. It must stay alive while it is shown, and be
    /// in the same kind of memory (SRAM or PSRAM) as the one given to [`start`](Self::start).
    pub fn set_frame_buffer(&mut self, frame_buffer: &FrameBuffer) {
        FRAME_ADDR.store(frame_buffer.address(), Ordering::Release);
    }

    /// Wait for the start of vertical blanking. Rendering right after this avoids tearing for
    /// updates that finish within the blanking interval (about 10 lines, 0.3 ms).
    pub async fn wait_vblank(&mut self) {
        pac::PIO1.irq().write(|w| w.set_irq(1 << VBLANK_IRQ));
        self.vblank.wait().await;
    }

    /// Turn the panel output off (black, low power) or back on. Scan-out keeps running.
    pub fn set_display_on(&mut self, on: bool) {
        if on {
            self.command(Register::SleepOut, &[]);
            block_for(Duration::from_millis(120));
            self.command(Register::DisplayOn, &[]);
        } else {
            self.command(Register::DisplayOff, &[]);
            self.command(Register::SleepIn, &[]);
        }
    }
}

impl Drop for St7701<'_> {
    fn drop(&mut self) {
        pac::PIO1
            .ctrl()
            .modify(|w| w.set_sm_enable(w.sm_enable() & !((1 << TIMING_SM) | (1 << DATA_SM))));
        let mask: u16 = DMA_CHANNELS.iter().fold(1 << COPY_DMA, |a, &ch| a | 1 << ch);
        // Stop the re-arm channels first so they cannot restart the streaming channels.
        for i in [1, 3, 0, 2] {
            let ch = pac::DMA.ch(DMA_CHANNELS[i] as usize);
            let mut ctrl = ch.ctrl_trig().read();
            ctrl.set_en(false);
            ch.al1_ctrl().write_value(ctrl.0);
        }
        pac::DMA.chan_abort().write(|w| w.set_chan_abort(mask));
        while pac::DMA.chan_abort().read().chan_abort() & mask != 0 {}
    }
}

/// Set up a self-perpetuating DMA loop: `stream` copies `count` words from `*addr_source` to
/// `fifo` paced by `dreq`, then chains to `rearm`, which writes `*addr_source` into `stream`'s
/// read-address trigger register, restarting it with its transfer count reloaded.
/// `read_ring_bits`: if non-zero, the stream's read address wraps within a buffer of
/// `1 << read_ring_bits` bytes (aligned to its size).
fn arm_loop(stream: u8, rearm: u8, addr_source: &'static AtomicU32, fifo: u32, count: u32, dreq: u8, read_ring_bits: u8) {
    use pac::dma::vals::{DataSize, TreqSel};
    let s = pac::DMA.ch(stream as usize);
    let r = pac::DMA.ch(rearm as usize);

    r.read_addr().write_value(addr_source.as_ptr() as u32);
    r.write_addr().write_value(s.al3_read_addr_trig().as_ptr() as u32);
    r.trans_count().write(|w| w.set_count(1));
    let mut ctrl = pac::dma::regs::CtrlTrig(0);
    ctrl.set_treq_sel(TreqSel::from(DREQ_FORCE));
    ctrl.set_data_size(DataSize::SIZE_WORD);
    ctrl.set_incr_read(false);
    ctrl.set_incr_write(false);
    ctrl.set_chain_to(rearm); // chaining to itself disables chaining
    ctrl.set_high_priority(true);
    ctrl.set_en(true);
    // AL1_CTRL does not trigger: the channel waits for `stream` to chain to it.
    r.al1_ctrl().write_value(ctrl.0);

    s.read_addr().write_value(addr_source.load(Ordering::Acquire));
    s.write_addr().write_value(fifo);
    s.trans_count().write(|w| w.set_count(count));
    compiler_fence(Ordering::SeqCst);
    s.ctrl_trig().write(|w| {
        w.set_treq_sel(TreqSel::from(dreq));
        w.set_data_size(DataSize::SIZE_WORD);
        w.set_incr_read(true);
        w.set_incr_write(false);
        w.set_ring_sel(false);
        w.set_ring_size(read_ring_bits);
        w.set_chain_to(rearm);
        w.set_high_priority(true);
        w.set_en(true);
    });
}

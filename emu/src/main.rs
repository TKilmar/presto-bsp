//! Run Presto firmware in the `rp2350-emu` emulator and check the LCD bus.
//!
//! The harness acts as a logic analyser on the panel pins: it samples every GPIO once per system
//! clock cycle, decodes HSYNC/VSYNC/PCLK and the 16 data lines exactly like the ST7701 does in HV
//! mode, rebuilds each frame, and compares it with the pattern drawn by the `lcd_test_pattern`
//! example. Captured frames are written to `emu/out/frame<N>.ppm`.
//!
//! ```text
//! ./setup.sh                                      # once: patched emulator crates in vendor/
//! cargo build --release --bin lcd_test_pattern   # in examples/
//! cargo run --release                             # in emu/
//! ```
//!
//! Diagnostics: `EMU_TRACE=1` (trace the last PCs before a HardFault), `EMU_PEEK=addr,...`
//! (dump registers if the dot clock never starts), `EMU_EDGES=<cycles>` (pin activity, PIO and
//! DMA state).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use object::{Object, ObjectSegment, ObjectSymbol};
use rp2350_emu::{Config, EmulatorBuilder};

const WIDTH: usize = 480;
const HEIGHT: usize = 480;

// Panel pins (see the schematic).
const PIN_D0: u32 = 1;
const PIN_HSYNC: u32 = 19;
const PIN_VSYNC: u32 = 20;
const PIN_DE: u32 = 21;
const PIN_PCLK: u32 = 22;

// What the ST7701 is configured to expect (RGBCTRL 0x80 0x2e 0x0e and Pimoroni's timing).
const H_TOTAL: usize = 530;
const H_SYNC: usize = 16;
/// Pixel 0 is latched on this rising PCLK edge, counting from the HSYNC falling edge (HBP 46).
const FIRST_PIXEL_EDGE: usize = 47;
const V_TOTAL: usize = 498;
const V_SYNC: usize = 8;
const FIRST_ACTIVE_LINE: usize = 13;
const FRAMES_TO_CHECK: usize = 3;
const SYS_CLK_HZ: f64 = 200e6;

/// Copy of `pattern()` in `examples/src/bin/lcd_test_pattern.rs`.
fn pattern(x: usize, y: usize) -> u16 {
    if x == 0 || y == 0 || x == WIDTH - 1 || y == HEIGHT - 1 {
        return 0xffff;
    }
    let r = (x * 32 / WIDTH) as u16;
    let g = (y * 64 / HEIGHT) as u16;
    let b = if ((x / 30) ^ (y / 30)) & 1 == 1 { 0x1f } else { (x ^ y) as u16 & 0x1f };
    (r << 11) | (g << 5) | b
}

/// Minimal Cortex-M boot ROM: core 0 jumps through the vector table at the start of flash,
/// core 1 parks in a WFE loop. Stands in for the RP2350 mask ROM, which is not redistributed
/// with the emulator crate.
fn boot_stub() -> Vec<u8> {
    let halfwords: [u16; 14] = [
        0x4806, // 0x08 ldr r0, [pc, #24]  ; =SIO_CPUID
        0x6800, // 0x0a ldr r0, [r0]
        0x2800, // 0x0c cmp r0, #0
        0xd107, // 0x0e bne park
        0x4805, // 0x10 ldr r0, [pc, #20]  ; =0x10000000 (flash vector table)
        0x4906, // 0x12 ldr r1, [pc, #24]  ; =VTOR
        0x6008, // 0x14 str r0, [r1]
        0x6801, // 0x16 ldr r1, [r0]       ; initial SP
        0xf381, 0x8808, // 0x18 msr msp, r1
        0x6841, // 0x1c ldr r1, [r0, #4]   ; reset handler
        0x4708, // 0x1e bx r1
        0xbf20, // 0x20 park: wfe
        0xe7fd, // 0x22 b park
    ];
    let mut rom = Vec::new();
    rom.extend_from_slice(&0x2008_2000u32.to_le_bytes()); // initial SP
    rom.extend_from_slice(&0x0000_0009u32.to_le_bytes()); // reset handler (Thumb)
    for h in halfwords {
        rom.extend_from_slice(&h.to_le_bytes());
    }
    for word in [0xd000_0000u32, 0x1000_0000, 0xe000_ed08] {
        rom.extend_from_slice(&word.to_le_bytes());
    }
    rom.resize(32 * 1024, 0);
    rom
}

struct Firmware {
    flash: Vec<u8>,
    symbols: Vec<(u64, u64, String)>,
}

fn load_elf(path: &PathBuf) -> Result<Firmware, String> {
    let data = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let elf = object::File::parse(&*data).map_err(|e| format!("parsing ELF: {e}"))?;
    let mut flash = Vec::new();
    for segment in elf.segments() {
        // Program headers carry the load address; `.data` is stored in flash after `.rodata`.
        let bytes = segment.data().map_err(|e| e.to_string())?;
        let (offset, _) = segment.file_range();
        let paddr = elf_paddr(&data, offset).unwrap_or(segment.address());
        if bytes.is_empty() || !(0x1000_0000..0x1100_0000).contains(&paddr) {
            continue;
        }
        let start = (paddr - 0x1000_0000) as usize;
        if flash.len() < start + bytes.len() {
            flash.resize(start + bytes.len(), 0xff);
        }
        flash[start..start + bytes.len()].copy_from_slice(bytes);
    }
    let mut symbols: Vec<_> = elf
        .symbols()
        .filter(|s| s.size() > 0)
        .filter_map(|s| Some((s.address() & !1, s.size(), s.name().ok()?.to_string())))
        .collect();
    symbols.sort();
    Ok(Firmware { flash, symbols })
}

/// The `object` crate reports a segment's virtual address; find the physical (load) address by
/// matching the program header with the same file offset.
fn elf_paddr(data: &[u8], file_offset: u64) -> Option<u64> {
    let u32_at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap()) as u64;
    let u16_at = |o: usize| u16::from_le_bytes(data[o..o + 2].try_into().unwrap()) as usize;
    let phoff = u32_at(0x1c) as usize;
    let phentsize = u16_at(0x2a);
    let phnum = u16_at(0x2c);
    (0..phnum).find_map(|i| {
        let ph = phoff + i * phentsize;
        (u32_at(ph) == 1 && u32_at(ph + 4) == file_offset).then(|| u32_at(ph + 12))
    })
}

impl Firmware {
    fn symbol_for(&self, addr: u32) -> String {
        let addr = addr as u64 & !1;
        self.symbols
            .iter()
            .rev()
            .find(|(start, size, _)| (*start..start + size).contains(&addr))
            .map(|(start, _, name)| format!("{name}+{:#x}", addr - start))
            .unwrap_or_else(|| format!("{addr:#010x}"))
    }
}

fn bit(pins: u64, pin: u32) -> bool {
    (pins >> pin) & 1 != 0
}

/// Decode the 16 data lines: GPIO1 carries RGB565 bit 15, GPIO16 bit 0.
fn data(pins: u64) -> u16 {
    (((pins >> PIN_D0) & 0xffff) as u16).reverse_bits()
}

#[derive(Default)]
struct LineStats {
    edges: usize,
    hsync_low_edges: usize,
}

struct Analyzer {
    history: VecDeque<u64>,
    /// Rising-edge samples awaiting a hold-time check: (cycles remaining, sampled data).
    pending_hold: VecDeque<(u32, u16)>,
    in_frame: bool,
    line: usize,
    line_stats: LineStats,
    line_pixels: Vec<u16>,
    frame: Vec<u16>,
    frames: Vec<Vec<u16>>,
    vsync_low_lines: usize,
    errors: Vec<String>,
    last_rise_cycle: Option<u64>,
    pclk_periods: (u64, u64, u64), // (min, max, count)
    setup_violations: usize,
    hold_violations: usize,
    de_low_seen: bool,
}

impl Analyzer {
    fn new() -> Self {
        Self {
            history: VecDeque::with_capacity(8),
            pending_hold: VecDeque::new(),
            in_frame: false,
            line: 0,
            line_stats: LineStats::default(),
            line_pixels: Vec::with_capacity(H_TOTAL),
            frame: vec![0; WIDTH * HEIGHT],
            frames: Vec::new(),
            vsync_low_lines: 0,
            errors: Vec::new(),
            last_rise_cycle: None,
            pclk_periods: (u64::MAX, 0, 0),
            setup_violations: 0,
            hold_violations: 0,
            de_low_seen: false,
        }
    }

    fn error(&mut self, msg: String) {
        if self.errors.len() < 20 {
            eprintln!("  ! {msg}");
        }
        self.errors.push(msg);
    }

    fn sample(&mut self, cycle: u64, pins: u64) {
        let prev = self.history.back().copied().unwrap_or(pins);
        if self.history.len() == 8 {
            self.history.pop_front();
        }
        self.history.push_back(pins);

        if !bit(pins, PIN_DE) {
            self.de_low_seen = true;
        }

        for entry in self.pending_hold.iter_mut() {
            entry.0 -= 1;
        }
        while let Some(&(0, sampled)) = self.pending_hold.front() {
            self.pending_hold.pop_front();
            if data(pins) != sampled {
                self.hold_violations += 1;
            }
        }

        // VSYNC falling edge: a new frame starts with this line.
        if bit(prev, PIN_VSYNC) && !bit(pins, PIN_VSYNC) {
            if self.in_frame {
                self.finish_frame();
            }
            self.in_frame = true;
            self.line = usize::MAX; // the HSYNC edge of this line makes it line 0
            self.vsync_low_lines = 0;
        }
        // HSYNC falling edge: a new line.
        if bit(prev, PIN_HSYNC) && !bit(pins, PIN_HSYNC) {
            self.finish_line();
            self.line = self.line.wrapping_add(1);
            if !bit(pins, PIN_VSYNC) {
                self.vsync_low_lines += 1;
            }
        }
        // PCLK rising edge: the panel latches HSYNC, VSYNC and data here.
        if !bit(prev, PIN_PCLK) && bit(pins, PIN_PCLK) {
            if let Some(last) = self.last_rise_cycle {
                let period = cycle - last;
                let p = &mut self.pclk_periods;
                *p = (p.0.min(period), p.1.max(period), p.2 + 1);
            }
            self.last_rise_cycle = Some(cycle);
            self.line_stats.edges += 1;
            if !bit(pins, PIN_HSYNC) {
                self.line_stats.hsync_low_edges += 1;
            }
            // Setup: data must have been stable for one PIO cycle (3 system cycles) before the edge.
            let earlier = self.history[self.history.len().saturating_sub(4)];
            if data(earlier) != data(pins) {
                self.setup_violations += 1;
            }
            self.pending_hold.push_back((3, data(pins)));
            self.line_pixels.push(data(pins));
        }
    }

    fn finish_line(&mut self) {
        let stats = std::mem::take(&mut self.line_stats);
        let pixels = std::mem::take(&mut self.line_pixels);
        if !self.in_frame || self.line == usize::MAX {
            return;
        }
        if stats.edges != H_TOTAL {
            let msg = format!("frame {} line {}: {} PCLK edges, expected {H_TOTAL}", self.frames.len(), self.line, stats.edges);
            self.error(msg);
        }
        if stats.hsync_low_edges != H_SYNC {
            let msg = format!("line {}: HSYNC low for {} edges, expected {H_SYNC}", self.line, stats.hsync_low_edges);
            self.error(msg);
        }
        if let Some(row) = self.line.checked_sub(FIRST_ACTIVE_LINE).filter(|&r| r < HEIGHT) {
            let start = FIRST_PIXEL_EDGE - 1;
            for x in 0..WIDTH {
                self.frame[row * WIDTH + x] = pixels.get(start + x).copied().unwrap_or(0);
            }
        }
    }

    fn finish_frame(&mut self) {
        let lines = self.line.wrapping_add(1);
        if lines != V_TOTAL {
            let msg = format!("frame {}: {lines} lines, expected {V_TOTAL}", self.frames.len());
            self.error(msg);
        }
        if self.vsync_low_lines != V_SYNC {
            let msg = format!("frame {}: VSYNC low for {} lines, expected {V_SYNC}", self.frames.len(), self.vsync_low_lines);
            self.error(msg);
        }
        self.frames.push(std::mem::replace(&mut self.frame, vec![0; WIDTH * HEIGHT]));
    }
}

fn write_ppm(path: &PathBuf, frame: &[u16]) -> std::io::Result<()> {
    let mut out = format!("P6\n{WIDTH} {HEIGHT}\n255\n").into_bytes();
    for &p in frame {
        let r = ((p >> 11) & 0x1f) as u8;
        let g = ((p >> 5) & 0x3f) as u8;
        let b = (p & 0x1f) as u8;
        out.extend_from_slice(&[r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2]);
    }
    std::fs::write(path, out)
}

/// Compare a captured frame with the reference pattern. The top-left block may be black, white
/// or the original pattern depending on when the firmware last toggled it.
fn check_frame(index: usize, frame: &[u16]) -> Vec<String> {
    let mut errors = Vec::new();
    let mut mismatches = 0usize;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let got = frame[y * WIDTH + x];
            let in_block = (1..=64).contains(&x) && (1..=64).contains(&y);
            let ok = got == pattern(x, y) || (in_block && (got == 0 || got == 0xffff));
            if !ok {
                mismatches += 1;
                if errors.len() < 5 {
                    errors.push(format!(
                        "frame {index}: pixel ({x}, {y}) = {got:#06x}, expected {:#06x}",
                        pattern(x, y)
                    ));
                }
            }
        }
    }
    if mismatches > 0 {
        errors.push(format!("frame {index}: {mismatches} mismatched pixels"));
    }
    errors
}

fn main() -> ExitCode {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let elf_path = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        root.join("../examples/target/thumbv8m.main-none-eabihf/release/lcd_test_pattern")
    });
    let firmware = match load_elf(&elf_path) {
        Ok(fw) => fw,
        Err(e) => {
            eprintln!("{e}\nBuild it first: (cd examples && cargo build --release --bin lcd_test_pattern)");
            return ExitCode::FAILURE;
        }
    };
    println!("firmware: {} ({} KiB flash)", elf_path.display(), firmware.flash.len() / 1024);

    let trace = std::env::var_os("EMU_TRACE").is_some();
    let quantum = if trace { 1 } else { 64 };
    let mut emu = EmulatorBuilder::new(Config::default()).step_quantum(quantum).build().expect("emulator");
    let mut recent_pcs: VecDeque<u32> = VecDeque::new();
    emu.load_bootrom(&boot_stub());
    emu.load_flash(&firmware.flash);

    // Phase 1: boot, clock setup and panel init (~0.5 s of delays) in coarse quanta, until the
    // dot clock starts toggling.
    let wall = Instant::now();
    let mut last_pclk = emu.gpio_read(PIN_PCLK as u8);
    let mut toggles = 0;
    let mut next_report = 0u64;
    loop {
        if let Err(e) = emu.step() {
            eprintln!("emulator error: {e:?}");
            return ExitCode::FAILURE;
        }
        let pclk = emu.gpio_read(PIN_PCLK as u8);
        if pclk != last_pclk {
            toggles += 1;
            if toggles > 4 {
                break;
            }
        }
        last_pclk = pclk;
        let cycles = emu.cycles();
        let pc = emu.core(0).reg(15);
        if trace && recent_pcs.back() != Some(&pc) {
            if recent_pcs.len() == 40 {
                recent_pcs.pop_front();
            }
            recent_pcs.push_back(pc);
        }
        if firmware.symbol_for(pc).starts_with("HardFault") {
            for pc in &recent_pcs {
                eprintln!("  trace {pc:08x} {}", firmware.symbol_for(*pc));
            }
            let sp = emu.core(0).reg(13);
            let frame: Vec<u32> = (0..8).map(|i| emu.peek(sp + 4 * i)).collect();
            eprintln!("HardFault: stacked PC {} LR {}", firmware.symbol_for(frame[6]), firmware.symbol_for(frame[5]));
            eprintln!("  r0-r3 {:08x} {:08x} {:08x} {:08x} xPSR {:08x}", frame[0], frame[1], frame[2], frame[3], frame[7]);
            eprintln!(
                "  CFSR {:08x} HFSR {:08x} MMFAR {:08x} BFAR {:08x}",
                emu.peek(0xe000_ed28),
                emu.peek(0xe000_ed2c),
                emu.peek(0xe000_ed34),
                emu.peek(0xe000_ed38)
            );
            return ExitCode::FAILURE;
        }
        if cycles >= next_report {
            let pc = emu.core(0).reg(15);
            println!("  boot: {:>5.0} ms emulated, core0 in {}", cycles as f64 / SYS_CLK_HZ * 1e3, firmware.symbol_for(pc));
            next_report += 40_000_000;
        }
        if cycles > 1_000_000_000 {
            eprintln!("dot clock never started (core0 at {})", firmware.symbol_for(emu.core(0).reg(15)));
            // EMU_PEEK=0x40088000,0x4008800c dumps registers to help find what firmware waits for.
            for addr in std::env::var("EMU_PEEK").unwrap_or_default().split(',').filter(|a| !a.is_empty()) {
                let addr = u32::from_str_radix(addr.trim_start_matches("0x"), 16).expect("hex address");
                eprintln!("  [{addr:#010x}] = {:#010x}", emu.mmio_read32(addr));
            }
            return ExitCode::FAILURE;
        }
    }
    println!("scan-out started after {:.0} ms emulated ({:.1} s wall)", emu.cycles() as f64 / SYS_CLK_HZ * 1e3, wall.elapsed().as_secs_f64());

    // Phase 2: sample every system cycle and decode the panel bus.
    emu.step_quantum = 1;
    let mut analyzer = Analyzer::new();
    let wall = Instant::now();
    if std::env::var_os("EMU_EDGES").is_some() {
        // Diagnostic (EMU_EDGES=<cycles>): count edges on every pin, then dump PIO1 and the
        // scan-out DMA channels.
        let mut edges = [0u32; 48];
        let mut prev = emu.gpio_read_all();
        for _ in 0..std::env::var("EMU_EDGES").unwrap().parse::<u64>().unwrap_or(4_000_000) {
            emu.step().unwrap();
            let pins = emu.gpio_read_all();
            for (pin, count) in edges.iter_mut().enumerate() {
                if (pins ^ prev) >> pin & 1 != 0 {
                    *count += 1;
                }
            }
            prev = pins;
        }
        for (pin, count) in edges.iter().enumerate().filter(|(_, c)| **c > 0) {
            println!("  GPIO{pin}: {count} edges");
        }
        println!("  final pins {prev:#014x}");
        let pio = 0x5030_0000u32;
        println!("  PIO1 CTRL {:#x} FLEVEL {:#010x} IRQ {:#x}", emu.mmio_read32(pio), emu.mmio_read32(pio + 0x0c), emu.mmio_read32(pio + 0x30));
        for sm in 0..2u32 {
            let b = pio + 0xc8 + sm * 0x18;
            let r: Vec<u32> = (0..6).map(|i| emu.mmio_read32(b + 4 * i)).collect();
            println!(
                "  SM{sm}: clkdiv {:#010x} execctrl {:#010x} shiftctrl {:#010x} addr {} instr {:#06x} pinctrl {:#010x}",
                r[0], r[1], r[2], r[3], r[4], r[5]
            );
        }
        for ch in 1..=4u32 {
            let base = 0x5000_0000 + ch * 0x40;
            let r: Vec<u32> = (0..4).map(|i| emu.mmio_read32(base + 4 * i)).collect();
            println!("  DMA{ch}: read {:#010x} write {:#010x} count {:#010x} ctrl {:#010x}", r[0], r[1], r[2], r[3]);
        }
        return ExitCode::SUCCESS;
    }
    while analyzer.frames.len() < FRAMES_TO_CHECK {
        if let Err(e) = emu.step() {
            eprintln!("emulator error: {e:?}");
            return ExitCode::FAILURE;
        }
        analyzer.sample(emu.cycles(), emu.gpio_read_all());
        if emu.cycles() % 50_000_000 == 0 {
            println!("  capture: {} frames, core0 in {}", analyzer.frames.len(), firmware.symbol_for(emu.core(0).reg(15)));
        }
    }
    println!("captured {FRAMES_TO_CHECK} frames in {:.1} s wall", wall.elapsed().as_secs_f64());

    let (min, max, _) = analyzer.pclk_periods;
    println!(
        "PCLK period {min}..{max} system cycles -> {:.2} MHz dot clock, {:.1} Hz refresh",
        SYS_CLK_HZ / max as f64 / 1e6,
        SYS_CLK_HZ / max as f64 / (H_TOTAL * V_TOTAL) as f64
    );
    println!("setup violations: {}, hold violations: {}", analyzer.setup_violations, analyzer.hold_violations);
    let mut errors = std::mem::take(&mut analyzer.errors);
    if min != max {
        errors.push(format!("PCLK jitter: period varies {min}..{max} cycles"));
    }
    if analyzer.setup_violations + analyzer.hold_violations > 0 {
        errors.push("data changed within one PIO cycle of a PCLK rising edge".into());
    }
    if analyzer.de_low_seen {
        errors.push("DE went low (panel runs in HV mode with DE held high)".into());
    }

    let out = root.join("out");
    let _ = std::fs::create_dir_all(&out);
    for (i, frame) in analyzer.frames.iter().enumerate() {
        let path = out.join(format!("frame{i}.ppm"));
        if let Err(e) = write_ppm(&path, frame) {
            eprintln!("writing {}: {e}", path.display());
        }
        errors.extend(check_frame(i, frame));
    }
    println!("frames written to {}", out.display());

    if errors.is_empty() {
        println!("PASS: {FRAMES_TO_CHECK} frames match the reference pattern with correct sync timing");
        ExitCode::SUCCESS
    } else {
        for e in errors.iter().take(30) {
            eprintln!("FAIL: {e}");
        }
        ExitCode::FAILURE
    }
}

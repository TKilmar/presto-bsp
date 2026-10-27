//! Measures PSRAM read bandwidth (DMA and CPU), to size display and heap placement.
#![no_std]
#![no_main]

use defmt::info;
use embassy_executor::Spawner;
use embassy_rp::pac;
use embassy_time::Instant;
use presto_bsp::Presto;
use {defmt_rtt as _, panic_probe as _};

const BYTES: usize = 64 * 1024;
static mut SINK: [u32; BYTES / 4] = [0; BYTES / 4];

/// Copy `BYTES` from `src` into SINK with DMA channel 6 (32-bit, unpaced); returns MB/s.
fn dma_copy(src: u32, incr_write: bool) -> u32 {
    use pac::dma::vals::{DataSize, TreqSel};
    let ch = pac::DMA.ch(6);
    ch.read_addr().write_value(src);
    ch.write_addr().write_value(core::ptr::addr_of_mut!(SINK) as u32);
    ch.trans_count().write(|w| w.set_count((BYTES / 4) as u32));
    let t = Instant::now();
    ch.ctrl_trig().write(|w| {
        w.set_treq_sel(TreqSel::from(0x3f));
        w.set_data_size(DataSize::SIZE_WORD);
        w.set_incr_read(true);
        w.set_incr_write(incr_write);
        w.set_chain_to(6);
        w.set_en(true);
    });
    while ch.ctrl_trig().read().busy() {}
    let us = t.elapsed().as_micros().max(1) as u32;
    (BYTES as u32) / us
}

fn cpu_read(src: u32) -> u32 {
    let p = src as *const u32;
    let t = Instant::now();
    let mut acc = 0u32;
    for i in 0..BYTES / 4 {
        acc = acc.wrapping_add(unsafe { core::ptr::read_volatile(p.add(i)) });
    }
    let us = t.elapsed().as_micros().max(1) as u32;
    defmt::trace!("{}", acc);
    (BYTES as u32) / us
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    presto_examples::init_heap();
    let board = Presto::default();
    let Some(psram) = board.psram.as_ref() else { panic!("no PSRAM") };
    let base = psram.base_address() as u32;
    let uncached = base - 0x1100_0000 + 0x1500_0000;
    // Fill a region so reads are real data.
    for i in 0..(4 * BYTES / 4) {
        unsafe { core::ptr::write_volatile((uncached as *mut u32).add(i), i as u32) };
    }
    info!("M1_TIMING {:#010x}", pac::QMI.mem(1).timing().read().0);
    let sram = core::ptr::addr_of!(SINK) as u32;
    info!("DMA SRAM->SRAM        {} MB/s", dma_copy(sram, true));
    info!("DMA PSRAM uncached    {} MB/s", dma_copy(uncached + BYTES as u32, true));
    info!("DMA PSRAM cached      {} MB/s", dma_copy(base + 2 * BYTES as u32, true));
    info!("DMA PSRAM cached, fixed dst {} MB/s", dma_copy(base + 3 * BYTES as u32, false));
    info!("CPU PSRAM uncached    {} MB/s", cpu_read(uncached + BYTES as u32));
    info!("CPU PSRAM cached      {} MB/s", cpu_read(base + 2 * BYTES as u32));
    info!("done");
    loop {
        embassy_time::Timer::after_secs(1).await;
    }
}

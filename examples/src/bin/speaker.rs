#![no_std]
#![no_main]

extern crate alloc;

use embassy_executor::Spawner;
use embassy_time::Timer;
use presto_bsp::speaker::{NamedPitch, Note, SoftSpeaker};
use presto_bsp::{embassy_rp::gpio::{Level, Output}, Presto};
use {defmt_rtt as _, panic_probe as _};


const TUNE: [(NamedPitch, u32); 18] = {
    #[allow(clippy::enum_glob_use)]
    use NamedPitch::*;
    [
        (D4, 1),
        (DS4, 1),
        (E4, 1),
        (C5, 2),
        (E4, 1),
        (C5, 2),
        (E4, 1),
        (C5, 3),
        (C4, 1),
        (D4, 1),
        (DS4, 1),
        (E4, 1),
        (C4, 1),
        (D4, 1),
        (E4, 2),
        (B4, 1),
        (D5, 2),
        (C4, 4),
    ]
};

#[embassy_executor::main]
async fn main(_s: Spawner) {
    // Simple bump allocator for Slint/alloc users.
    const HEAP_SIZE: usize = 200 * 1024;
    static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
    #[global_allocator]
    static ALLOCATOR: embedded_alloc::Heap = embedded_alloc::Heap::empty();
    unsafe { ALLOCATOR.init(core::ptr::addr_of_mut!(HEAP) as usize, HEAP_SIZE) }

    let board = Presto::default();
    defmt::info!("Application started!");
    let mut speaker = SoftSpeaker::new(Output::new(board.speaker, Level::Low));
    loop {
        defmt::info!("Playing tune!");
        for (pitch, ticks) in TUNE {
            speaker.play(&Note(pitch.into(), 200 * ticks)).await;
        }
        Timer::after_secs(5).await;
    }
}

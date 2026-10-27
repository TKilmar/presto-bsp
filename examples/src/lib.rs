//! Shared helpers for the Presto examples.
#![no_std]

use embedded_alloc::LlffHeap;

#[global_allocator]
static HEAP: LlffHeap = LlffHeap::empty();

/// Give the global allocator a small SRAM heap.
///
/// Slint (a BSP dependency) links `alloc`, so every binary needs an allocator. The nursery UI
/// needs a much larger one; see `init_heap_at`. Heaps must be in SRAM: atomic reference counts
/// never complete in PSRAM on the RP2350.
pub fn init_heap() {
    const HEAP_SIZE: usize = 8 * 1024;
    static mut HEAP_MEM: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
    // SAFETY: called once at start-up, before any allocation.
    unsafe { HEAP.init(core::ptr::addr_of_mut!(HEAP_MEM) as usize, HEAP_SIZE) }
}

/// Give the global allocator `size` bytes at `start` (for example PSRAM).
///
/// # Safety
/// The memory must be unused, valid for reads and writes, and this must be called once, before
/// any allocation.
pub unsafe fn init_heap_at(start: usize, size: usize) {
    unsafe { HEAP.init(start, size) }
}

/// Bytes currently allocated from the global heap.
pub fn heap_used() -> usize {
    HEAP.used()
}

//! The audio callback's path, `AudioInput::push`, must not allocate. A counting global allocator watches one thread.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Duration;

use common::{FakeRecognizer, fake_host};
use omuzaki_speech::HostConfig;
use omuzaki_speech::synth::tone;

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY-free: the allocator only counts and forwards to `System`, so it adds no unsafe obligations beyond the trait's.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if WATCHING.with(Cell::get) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if WATCHING.with(Cell::get) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn pushing_audio_does_not_allocate() {
    let (mut host, mut input) = fake_host(HostConfig::default(), Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    let block = tone(0.01, 220.0, 0.3);
    input.push(&block); // warm up anything lazy (thread-locals, the first unpark)

    WATCHING.with(|w| w.set(true));
    for _ in 0..2_000 {
        input.push(&block);
    }
    WATCHING.with(|w| w.set(false));

    assert_eq!(ALLOCATIONS.with(Cell::get), 0, "AudioInput::push allocated");
    host.stop();
}

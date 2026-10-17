//! Real-time safety checks for the cubed-synth crates.
//!
//! An audio callback must not allocate or free memory: either can take a lock
//! or fault in pages and miss the block's deadline. [`CountingAllocator`]
//! counts the allocations, reallocations and frees made on threads that armed
//! it, and `tests/no_alloc.rs` uses it to show that every source the engine's
//! mixer will host renders its blocks without any.

use std::alloc::{GlobalAlloc, Layout, System};
use std::backtrace::Backtrace;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static EVENTS: Cell<usize> = const { Cell::new(0) };
}

static TRACE: AtomicBool = AtomicBool::new(false);

/// Prints a backtrace for every counted call, to find where a render path
/// allocates. The tests turn it on when `CUBED_RT_TRACE` is set.
pub fn trace_allocations(on: bool) {
    TRACE.store(on, Ordering::Relaxed);
}

/// The system allocator, counting calls made on armed threads. A test binary
/// installs it with `#[global_allocator]`.
pub struct CountingAllocator;

impl CountingAllocator {
    fn count() {
        // `try_with`: a thread's locals may already be gone while it exits.
        let _ = ARMED.try_with(|armed| {
            if armed.get() {
                let _ = EVENTS.try_with(|events| events.set(events.get() + 1));
                if TRACE.load(Ordering::Relaxed) {
                    // Disarmed while tracing, since the backtrace allocates too.
                    armed.set(false);
                    eprintln!("counted allocation:\n{}", Backtrace::force_capture());
                    armed.set(true);
                }
            }
        });
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::count();
        System.alloc(layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::count();
        System.alloc_zeroed(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        Self::count();
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::count();
        System.realloc(ptr, layout, new_size)
    }
}

/// Runs `f` and returns how many allocations, reallocations and frees it made
/// on this thread. It only counts when [`CountingAllocator`] is the global
/// allocator.
pub fn allocations_during(f: impl FnOnce()) -> usize {
    EVENTS.with(|events| events.set(0));
    ARMED.with(|armed| armed.set(true));
    f();
    ARMED.with(|armed| armed.set(false));
    EVENTS.with(|events| events.get())
}

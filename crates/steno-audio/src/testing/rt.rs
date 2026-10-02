//! Real-time safety evidence: a counting global allocator.
//! From `spikes/capture-rs/src/rt.rs`; no Swift equivalent (the Swift
//! tests hook libmalloc's `malloc_logger`, Darwin only).
//!
//! While counting is on for the current thread, every allocation,
//! reallocation and deallocation that thread makes is counted. An
//! integration test installs it as `#[global_allocator]` (each test binary
//! has its own), runs the IOProc body, the processing loop with the real
//! Speex canceller and the relay hand-off on its own thread, and asserts
//! the count is zero. The report prints the count; anything above zero
//! means the callback path allocated.
//!
//! The current thread is identified by the OS (`pthread_self` on Unix,
//! `GetCurrentThreadId` on Windows), the two calls that neither allocate
//! nor touch Rust's thread-local machinery, so the allocator may make them
//! re-entrantly. (A `thread_local!` address worked on Linux and macOS but
//! counted nothing on the Windows runner.)

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

pub struct CountingAllocator;

static COUNTING: AtomicBool = AtomicBool::new(false);
static COUNTED_THREAD: AtomicUsize = AtomicUsize::new(0);
static COUNTED: AtomicU64 = AtomicU64::new(0);
static TOTAL: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
unsafe extern "C" {
    fn pthread_self() -> usize;
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentThreadId() -> u32;
}

#[inline(always)]
fn thread_id() -> usize {
    // SAFETY: both are plain, always-available OS calls without
    // preconditions.
    #[cfg(unix)]
    unsafe {
        pthread_self()
    }
    #[cfg(windows)]
    unsafe {
        GetCurrentThreadId() as usize
    }
}

#[inline(always)]
fn note() {
    TOTAL.fetch_add(1, Ordering::Relaxed);
    if COUNTING.load(Ordering::Relaxed) && thread_id() == COUNTED_THREAD.load(Ordering::Relaxed) {
        COUNTED.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: every method forwards to `System` after a counter update; the
// counters are atomics and the thread id is a TLS address, neither of
// which allocates, so the allocator never re-enters itself.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note();
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        // SAFETY: forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

impl CountingAllocator {
    /// Starts counting the calling thread's allocations; resets the count.
    pub fn start_counting() {
        COUNTED.store(0, Ordering::Relaxed);
        COUNTED_THREAD.store(thread_id(), Ordering::Relaxed);
        COUNTING.store(true, Ordering::Release);
    }

    /// Stops counting and returns the allocations the counted thread made
    /// since `start_counting`.
    pub fn stop_counting() -> u64 {
        COUNTING.store(false, Ordering::Release);
        COUNTED.load(Ordering::Relaxed)
    }

    /// Allocations the calling thread made while running `body`. Only
    /// meaningful when this type is the process's `#[global_allocator]`;
    /// otherwise the count is always zero and proves nothing, which the
    /// callers' "the hook sees a deliberate allocation" test guards.
    pub fn allocations_during(body: impl FnOnce()) -> u64 {
        Self::start_counting();
        body();
        Self::stop_counting()
    }

    /// Every allocator call in the process so far.
    pub fn total() -> u64 {
        TOTAL.load(Ordering::Relaxed)
    }
}

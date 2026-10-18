//! Real-time safety evidence: a counting global allocator. While counting
//! is on, every allocator call the counted thread makes is counted. The
//! thread is identified by an OS call (`gettid`, `pthread_self`,
//! `GetCurrentThreadId`) that neither allocates nor touches Rust's
//! thread-local machinery, so the allocator may make it re-entrantly.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// The `#[global_allocator]` a test binary installs to count one thread's
/// allocations.
pub struct CountingAllocator;

/// One measurement at a time: the counters are process-wide, and a second
/// measurement would reset the first's.
static MEASURING: Mutex<()> = Mutex::new(());

static COUNTING: AtomicBool = AtomicBool::new(false);
static COUNTED_THREAD: AtomicUsize = AtomicUsize::new(0);
static COUNTED: AtomicU64 = AtomicU64::new(0);
static TOTAL: AtomicU64 = AtomicU64::new(0);

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn gettid() -> i32;
}

#[cfg(all(unix, not(target_os = "linux")))]
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
    // SAFETY: a plain, always-available OS call without preconditions
    // (`gettid` since glibc 2.30 and musl 1.2.2).
    #[cfg(target_os = "linux")]
    unsafe {
        gettid() as usize
    }
    // SAFETY: as above.
    #[cfg(all(unix, not(target_os = "linux")))]
    unsafe {
        pthread_self()
    }
    // SAFETY: as above.
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
// counters are atomics and the thread id is an OS call, neither of which
// allocates, so the allocator never re-enters itself.
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
        Self::start_counting_on(thread_id());
    }

    fn start_counting_on(thread: usize) {
        COUNTED.store(0, Ordering::Relaxed);
        COUNTED_THREAD.store(thread, Ordering::Relaxed);
        COUNTING.store(true, Ordering::Release);
    }

    /// Stops counting and returns the allocations the counted thread made
    /// since `start_counting`.
    pub fn stop_counting() -> u64 {
        COUNTING.store(false, Ordering::Release);
        COUNTED.load(Ordering::Relaxed)
    }

    /// Allocations the calling thread made while running `body`. Only
    /// meaningful when this type is the `#[global_allocator]`; otherwise
    /// the count is always zero, which the callers' deliberate-allocation
    /// test guards.
    pub fn allocations_during(body: impl FnOnce()) -> u64 {
        Self::allocations_on(thread_id(), body)
    }

    /// Allocations thread `thread` made while the calling thread ran
    /// `body`: how a test counts a thread it cannot run code on (on Linux
    /// by kernel thread id).
    pub fn allocations_on(thread: usize, body: impl FnOnce()) -> u64 {
        let _guard = MEASURING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::start_counting_on(thread);
        body();
        Self::stop_counting()
    }

    /// The calling thread's id as the counter compares it.
    #[must_use]
    pub fn current_thread() -> usize {
        thread_id()
    }

    /// Every allocator call in the process so far.
    pub fn total() -> u64 {
        TOTAL.load(Ordering::Relaxed)
    }
}

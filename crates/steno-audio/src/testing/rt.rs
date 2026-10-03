//! Real-time safety evidence: a counting global allocator. No Swift
//! equivalent (the Swift tests hook libmalloc's `malloc_logger`, Darwin
//! only).
//!
//! While counting is on for the current thread, every allocation,
//! reallocation and deallocation that thread makes is counted. An
//! integration test installs it as `#[global_allocator]` (each test binary
//! has its own), runs the IOProc body, the processing loop with the real
//! Speex canceller and the relay hand-off on its own thread, and asserts
//! the count is zero. The report prints the count; anything above zero
//! means the callback path allocated.
//!
//! The current thread is identified by the OS (`gettid` on Linux,
//! `pthread_self` on other Unixes, `GetCurrentThreadId` on Windows), calls
//! that neither allocate nor touch Rust's thread-local machinery, so the
//! allocator may make them re-entrantly. (A `thread_local!` address worked
//! on Linux and macOS but counted nothing on the Windows runner.) On Linux
//! the id is the kernel's thread id, the one `/proc/self/task` lists, so a
//! test can count a thread it did not start, PipeWire's data loop
//! ([`CountingAllocator::allocations_on`]).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// The `#[global_allocator]` a test binary installs to count one thread's
/// allocations; see the module doc.
pub struct CountingAllocator;

/// One measurement at a time: the counters are process-wide, and the test
/// harness runs tests on several threads, so a second `allocations_during`
/// would reset the count and move the counted thread under the first.
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
    // SAFETY: all three are plain, always-available OS calls without
    // preconditions (`gettid` since glibc 2.30 and musl 1.2.2).
    #[cfg(target_os = "linux")]
    unsafe {
        gettid() as usize
    }
    #[cfg(all(unix, not(target_os = "linux")))]
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
    /// Measurements are serialised process-wide (see `MEASURING`).
    pub fn allocations_during(body: impl FnOnce()) -> u64 {
        let _guard = MEASURING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::start_counting();
        body();
        Self::stop_counting()
    }

    /// Allocations thread `thread` (an id as [`Self::current_thread`]
    /// gives it, on Linux the kernel's thread id) made while the calling
    /// thread ran `body`: how a test counts a thread it cannot run code on.
    /// Serialised with [`Self::allocations_during`].
    pub fn allocations_on(thread: usize, body: impl FnOnce()) -> u64 {
        let _guard = MEASURING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        COUNTED.store(0, Ordering::Relaxed);
        COUNTED_THREAD.store(thread, Ordering::Relaxed);
        COUNTING.store(true, Ordering::Release);
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

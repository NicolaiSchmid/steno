//! Real-time safety evidence. A counting global allocator: while the IOProc
//! is inside its callback (flag set, thread recorded) every allocation or
//! deallocation on that thread is counted. The report prints the count;
//! anything above zero means the callback path allocated.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

pub struct CountingAllocator;

pub static IN_CALLBACK: AtomicBool = AtomicBool::new(false);
pub static CALLBACK_THREAD: AtomicUsize = AtomicUsize::new(0);
pub static CALLBACK_ALLOCS: AtomicU64 = AtomicU64::new(0);
pub static TOTAL_ALLOCS: AtomicU64 = AtomicU64::new(0);

extern "C" {
    fn pthread_self() -> usize;
}

#[inline(always)]
fn note() {
    TOTAL_ALLOCS.fetch_add(1, Ordering::Relaxed);
    if IN_CALLBACK.load(Ordering::Relaxed) {
        let thread = unsafe { pthread_self() };
        if thread == CALLBACK_THREAD.load(Ordering::Relaxed) {
            CALLBACK_ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note();
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        System.realloc(ptr, layout, new_size)
    }
}

/// Call at the top of the IOProc.
#[inline(always)]
pub fn enter_callback() {
    CALLBACK_THREAD.store(unsafe { pthread_self() }, Ordering::Relaxed);
    IN_CALLBACK.store(true, Ordering::Release);
}

#[inline(always)]
pub fn leave_callback() {
    IN_CALLBACK.store(false, Ordering::Release);
}

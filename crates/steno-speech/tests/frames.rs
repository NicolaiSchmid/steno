//! The sidecar protocol's readers under a counting allocator: a frame's
//! length prefix alone must not make the reader reserve what it claims.
//! A garbage prefix on the child's stdout, or on its stdin, would
//! otherwise cost up to 64 MiB for a header and 5.5 GB for samples before
//! a single byte of them arrived. Its own test binary, because the
//! allocator is global.

#![allow(clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use steno_speech::sidecar::protocol::{
    self, FrameError, MAX_HEADER_BYTES, MAX_SAMPLES, Reply, Request,
};

/// The system allocator, noting the largest request on each thread.
struct Largest;

thread_local! {
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

fn note(size: usize) {
    let _ = LARGEST.try_with(|largest| largest.set(largest.get().max(size)));
}

// SAFETY: every call is forwarded unchanged to `System`; the bookkeeping
// touches a const-initialised thread-local and never allocates.
unsafe impl GlobalAlloc for Largest {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Largest = Largest;

/// The largest allocation `work` made on this thread.
fn largest_allocation(work: impl FnOnce()) -> usize {
    LARGEST.with(|largest| largest.set(0));
    work();
    LARGEST.with(Cell::get)
}

const LITTLE: usize = 1 << 20;

#[test]
fn a_header_length_alone_reserves_little() {
    let mut wire = MAX_HEADER_BYTES.to_le_bytes().to_vec();
    wire.extend_from_slice(br#"{"type":"#);
    let largest = largest_allocation(|| {
        assert!(matches!(
            protocol::read_header::<_, Reply>(&mut wire.as_slice()),
            Err(FrameError::Truncated)
        ));
    });
    assert!(largest < LITTLE, "{largest} bytes reserved");
}

#[test]
fn a_sample_count_alone_reserves_little() {
    let largest = largest_allocation(|| {
        assert!(matches!(
            protocol::read_samples(&mut &[0u8; 64][..], MAX_SAMPLES),
            Err(FrameError::Truncated)
        ));
    });
    assert!(largest < LITTLE, "{largest} bytes reserved");
}

#[test]
fn a_real_frame_still_reads_whole() {
    let samples: Vec<f32> = (0..300_000u32).map(|i| i as f32 / 65_536.0).collect();
    let request = Request::Transcribe {
        id: 7,
        sample_count: samples.len() as u64,
        hint: None,
    };
    let mut wire = Vec::new();
    protocol::write_frame(&mut wire, &request, &protocol::encode_samples(&samples)).unwrap();
    let mut input = wire.as_slice();
    let read: Request = protocol::read_header(&mut input).unwrap().unwrap();
    assert_eq!(read, request);
    assert_eq!(
        protocol::read_samples(&mut input, request.payload_bytes() / 4).unwrap(),
        samples
    );
    assert_eq!(input.len(), 0);
}

//! The decoder's working set under a counting allocator: decoding a lane
//! holds its 16 kHz output and a bounded working set, never the lane at
//! the source rate or the file's bytes, and a mixdown holds neither. A
//! regression to whole-file reads (a two-hour two-lane master is 2.8 GB
//! on disk and 1.4 GB a lane at 48 kHz) fails here. Its own test binary,
//! because the allocator is global; one test, because the bookkeeping is
//! per thread and the harness runs tests on several.
//!
//! The lanes are a minute long by default; `STENO_CODEC_MEMORY_SECONDS`
//! sets the master's length (7 200 for a two-hour master).
//! Files go under the target dir's scratch directory and are deleted at
//! the end.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::Path;

use steno_audio::codec::SymphoniaAudioCodec;
use steno_audio::writer::{CafStreamWriter, WavFile, WavStreamWriter};
use steno_core::AudioLane;

/// The system allocator, keeping this thread's live bytes and their peak.
struct Peak;

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn grow(size: usize) {
    let _ = LIVE.try_with(|live| {
        live.set(live.get() + size as isize);
        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(live.get())));
    });
}

fn shrink(size: usize) {
    let _ = LIVE.try_with(|live| live.set(live.get() - size as isize));
}

// SAFETY: every call is forwarded unchanged to `System`; the bookkeeping
// touches const-initialised thread-locals and never allocates.
unsafe impl GlobalAlloc for Peak {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        grow(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        grow(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        shrink(layout.size());
        unsafe { System.dealloc(ptr, layout) }
    }

    /// Counted as a new block next to the old one, so a buffer that grows
    /// by moving shows both at their peak.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        grow(new_size);
        shrink(layout.size());
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Peak = Peak;

/// What `work` returns and the most bytes it held at once on this thread
/// beyond what was live before it.
fn peak_during<T>(work: impl FnOnce() -> T) -> (T, usize) {
    let before = LIVE.with(Cell::get);
    PEAK.with(|peak| peak.set(before));
    let result = work();
    let peak = PEAK.with(Cell::get);
    (result, (peak - before).max(0) as usize)
}

const MIB: usize = 1 << 20;
/// What a decode may hold beyond its output: the block read from the
/// file, its samples, the resampler's history and the reserve on the
/// output. About 0.8 MiB for a two-lane master.
const WORKING_SET: usize = MIB;

fn seconds() -> usize {
    std::env::var("STENO_CODEC_MEMORY_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60)
}

/// A two-lane 48 kHz CAF of `frames`, written a second at a time.
fn write_master(path: &Path, frames: usize) {
    let mut writer = CafStreamWriter::create(path, 48_000.0, 2).unwrap();
    let mut block = vec![0.0f32; 48_000 * 2];
    let mut written = 0;
    while written < frames {
        let count = (frames - written).min(48_000);
        for (i, pair) in block[..count * 2]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .enumerate()
        {
            let t = (written + i) as f64 / 48_000.0;
            pair[0] = (0.5 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as f32;
            pair[1] = (0.25 * (2.0 * std::f64::consts::PI * 1_000.0 * t).sin()) as f32;
        }
        writer.write(&block[..count * 2], count).unwrap();
        written += count;
    }
    writer.finish().unwrap();
}

/// A mono 16-bit WAV of `frames` at `rate`, written a second at a time.
fn write_wav(path: &Path, rate: u32, frames: usize) {
    let mut writer = WavStreamWriter::create(path, rate).unwrap();
    let rate = rate as usize;
    let mut block = vec![0i16; rate];
    let mut written = 0;
    while written < frames {
        let count = (frames - written).min(rate);
        for (i, sample) in block[..count].iter_mut().enumerate() {
            let t = (written + i) as f64 / rate as f64;
            *sample = (8_000.0 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16;
        }
        writer.write(&block[..count]).unwrap();
        written += count;
    }
    writer.finish().unwrap();
}

fn report(what: &str, peak: usize, output: usize, file: u64) {
    println!(
        "{what}: peak {:.2} MiB beyond the {:.2} MiB output ({:.1} MiB file)",
        (peak.saturating_sub(output)) as f64 / MIB as f64,
        output as f64 / MIB as f64,
        file as f64 / MIB as f64
    );
}

#[test]
fn decoding_holds_the_16k_output_and_a_bounded_working_set() {
    let seconds = seconds();
    let directory = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();

    // The writer's master, two lanes at 48 kHz, through the 3:1 FIR.
    let master = directory.path().join("master.caf");
    write_master(&master, seconds * 48_000);
    let master_bytes = std::fs::metadata(&master).unwrap().len();
    let (lane, peak) =
        peak_during(|| SymphoniaAudioCodec::decode_path(&master, 1, AudioLane::System).unwrap());
    let output = lane.len() * 4;
    assert_eq!(lane.len(), seconds * 16_000);
    report("48 kHz CAF lane", peak, output, master_bytes);
    assert!(
        peak <= output + WORKING_SET,
        "decoding a {seconds} s lane held {peak} bytes for a {output}-byte output"
    );
    drop(lane);

    // The mixdown streams to its file and holds no lane at all.
    let mixdown = directory.path().join("audio.wav");
    let ((), peak) = peak_during(|| SymphoniaAudioCodec::mixdown_path(&master, &mixdown).unwrap());
    report("mixdown", peak, 0, master_bytes);
    assert!(peak <= WORKING_SET, "the mixdown held {peak} bytes");
    assert_eq!(
        WavFile::read_16k_mono(&mixdown).unwrap().len(),
        seconds * 16_000
    );
    std::fs::remove_file(&mixdown).unwrap();
    std::fs::remove_file(&master).unwrap();

    // The phone's rate through symphonia and the windowed sinc.
    let phone = directory.path().join("phone.wav");
    write_wav(&phone, 44_100, seconds * 44_100);
    let phone_bytes = std::fs::metadata(&phone).unwrap().len();
    let (lane, peak) =
        peak_during(|| SymphoniaAudioCodec::decode_path(&phone, 0, AudioLane::Mixed).unwrap());
    let output = lane.len() * 4;
    report("44.1 kHz WAV", peak, output, phone_bytes);
    assert!(
        peak <= output + WORKING_SET,
        "the 44.1 kHz WAV held {peak} bytes"
    );
    drop(lane);
    std::fs::remove_file(&phone).unwrap();

    // The 16 kHz sidecar, at least five minutes so its file outweighs the
    // working set.
    let sidecar = directory.path().join("sidecar.wav");
    write_wav(&sidecar, 16_000, seconds.max(300) * 16_000);
    let sidecar_bytes = std::fs::metadata(&sidecar).unwrap().len();
    let (lane, peak) = peak_during(|| WavFile::read_16k_mono(&sidecar).unwrap());
    let output = lane.len() * 4;
    report("16 kHz sidecar", peak, output, sidecar_bytes);
    assert!(
        peak <= output + WORKING_SET / 4,
        "the sidecar held {peak} bytes for a {output}-byte output"
    );
}

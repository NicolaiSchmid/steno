//! `steno-capture-spike --seconds N --out DIR [--lanes call|system|in-person]
//! [--no-aec] [--tail-ms 200]`
//!
//! Records the live lanes through a CoreAudio process tap + private aggregate
//! device + one IOProc into lock-free SPSC rings, writes mic.wav, system.wav,
//! master.wav (stereo: mic left, system right) and mic-aec.wav (Speex), and
//! prints what `steno dev capture-spike` prints plus the callback timing,
//! the callback allocation count and the ERLE of its own cancellation.
mod hal;
mod layout;
mod metrics;
mod ring;
mod rt;
mod speex;
mod synthetic;

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use objc2_core_audio::{
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultSystemOutputDevice,
    kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeOutput, AudioObjectID,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};

use hal::OSStatus;
use layout::{ChannelRef, Lane, LaneSource, StreamLayout};
use ring::{LaneRing, LaneRings};

#[global_allocator]
static ALLOCATOR: rt::CountingAllocator = rt::CountingAllocator;

const SAMPLE_RATE: usize = 48_000;
const FRAME: usize = 480;

/// Shared with the IOProc through a raw pointer; lives until the IOProc is destroyed.
struct CallbackContext {
    rings: LaneRings,
    sources: Vec<LaneSource>,
    callbacks: AtomicU64,
    max_ns: AtomicU64,
    total_ns: AtomicU64,
    frames: AtomicU64,
    first_callback: AtomicU64,
}

#[inline(always)]
unsafe fn samples_of(list: &AudioBufferList, channel: &ChannelRef) -> Option<*const f32> {
    if channel.buffer >= list.mNumberBuffers as usize {
        return None;
    }
    let buffer = &*list.mBuffers.as_ptr().add(channel.buffer);
    if buffer.mData.is_null() || buffer.mDataByteSize == 0 {
        return None;
    }
    Some((buffer.mData as *const f32).add(channel.offset))
}

/// The IOProc body. Follows the precomputed layout only: pointer arithmetic
/// from the buffer list into the rings. No allocation, no lock, no syscall
/// other than the clock read for the timing figure.
#[inline(always)]
unsafe fn deliver(ctx: &CallbackContext, list: &AudioBufferList) {
    if list.mNumberBuffers == 0 || ctx.sources.is_empty() {
        return;
    }
    let first = &ctx.sources[0].left;
    if first.buffer >= list.mNumberBuffers as usize {
        return;
    }
    let buffer = &*list.mBuffers.as_ptr().add(first.buffer);
    let channels = buffer.mNumberChannels as usize;
    if channels == 0 {
        return;
    }
    let frames = buffer.mDataByteSize as usize / (channels * 4);
    if frames == 0 || !ctx.rings.reserve(frames) {
        return;
    }
    for (lane, source) in ctx.sources.iter().enumerate() {
        let ring = &ctx.rings.rings[lane];
        match samples_of(list, &source.left) {
            Some(left) => match source.right.and_then(|r| samples_of(list, &r).map(|p| (p, r.stride))) {
                Some((right, right_stride)) => ring.write_mixed(left, right, frames, source.left.stride, right_stride),
                None => ring.write(left, frames, source.left.stride),
            },
            None => ring.write_zeros(frames),
        }
    }
    ctx.frames.fetch_add(frames as u64, Ordering::Relaxed);
}

unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> OSStatus {
    rt::enter_callback();
    let start = Instant::now();
    let ctx = &*(client as *const CallbackContext);
    deliver(ctx, input.as_ref());
    let elapsed = start.elapsed().as_nanos() as u64;
    ctx.max_ns.fetch_max(elapsed, Ordering::Relaxed);
    ctx.total_ns.fetch_add(elapsed, Ordering::Relaxed);
    if ctx.callbacks.fetch_add(1, Ordering::Relaxed) == 0 {
        ctx.first_callback.store(START.elapsed().as_micros() as u64, Ordering::Relaxed);
    }
    rt::leave_callback();
    0
}

static START_INIT: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
struct StartClock;
impl StartClock {
    fn elapsed(&self) -> Duration {
        START_INIT.get().map(|s| s.elapsed()).unwrap_or_default()
    }
}
static START: StartClock = StartClock;

struct Options {
    seconds: f64,
    out: String,
    lanes: Vec<Lane>,
    aec: bool,
    tail_ms: usize,
    synthetic: bool,
}

fn parse_args() -> Result<Options, String> {
    let mut options = Options { seconds: 10.0, out: String::new(), lanes: vec![Lane::Mic, Lane::System], aec: true, tail_ms: 200, synthetic: false };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seconds" => options.seconds = args.next().ok_or("--seconds needs a value")?.parse().map_err(|e| format!("--seconds: {e}"))?,
            "--out" => options.out = args.next().ok_or("--out needs a value")?,
            "--tail-ms" => options.tail_ms = args.next().ok_or("--tail-ms needs a value")?.parse().map_err(|e| format!("--tail-ms: {e}"))?,
            "--no-aec" => options.aec = false,
            "--synthetic" => options.synthetic = true,
            "--lanes" => {
                options.lanes = match args.next().ok_or("--lanes needs a value")?.as_str() {
                    "call" => vec![Lane::Mic, Lane::System],
                    "system" => vec![Lane::System],
                    "in-person" => vec![Lane::Mic],
                    other => return Err(format!("unknown lanes {other}")),
                }
            }
            "-h" | "--help" => {
                println!("steno-capture-spike --seconds N --out DIR [--lanes call|system|in-person] [--no-aec] [--tail-ms 200]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if options.out.is_empty() && !options.synthetic {
        return Err("--out is required".into());
    }
    Ok(options)
}

fn write_wav(path: &str, channels: &[&[f32]]) -> Result<(), String> {
    let spec = hound::WavSpec { channels: channels.len() as u16, sample_rate: SAMPLE_RATE as u32, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|e| format!("{path}: {e}"))?;
    let frames = channels.iter().map(|c| c.len()).min().unwrap_or(0);
    for i in 0..frames {
        for channel in channels {
            writer.write_sample(channel[i]).map_err(|e| format!("{path}: {e}"))?;
        }
    }
    writer.finalize().map_err(|e| format!("{path}: {e}"))
}

fn describe(c: &ChannelRef) -> String {
    format!("buffer {} channel {} stride {}", c.buffer, c.offset, c.stride)
}

fn main() {
    START_INIT.set(Instant::now()).ok();
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let options = parse_args()?;
    if options.synthetic {
        return run_synthetic(options.tail_ms);
    }
    std::fs::create_dir_all(&options.out).map_err(|e| format!("{}: {e}", options.out))?;
    let needs_mic = options.lanes.contains(&Lane::Mic);
    let needs_tap = options.lanes.contains(&Lane::System);

    let output = hal::default_device(kAudioHardwarePropertyDefaultSystemOutputDevice)?;
    let output_uid = hal::uid(output)?;
    println!("output device: {} ({})", hal::name(output), output_uid);
    let mut mic: Option<(AudioObjectID, String)> = None;
    if needs_mic {
        let id = hal::default_device(kAudioHardwarePropertyDefaultInputDevice)?;
        let uid = hal::uid(id)?;
        println!("input device: {} ({})", hal::name(id), uid);
        mic = Some((id, uid));
    }

    let tap_started = Instant::now();
    let tap = if needs_tap {
        let own = hal::own_process_object()?;
        println!("own process object: {own}");
        let tap = hal::ProcessTap::new(&[own])?;
        println!(
            "tap: id {} rate {} Hz channels {} flags {:#x} ({:.1} ms to create)",
            tap.id, tap.format.mSampleRate, tap.format.mChannelsPerFrame, tap.format.mFormatFlags,
            tap_started.elapsed().as_secs_f64() * 1000.0
        );
        Some(tap)
    } else {
        None
    };

    let mut sub_device_uids = vec![output_uid.clone()];
    let mut sub_device_counts = vec![hal::channel_counts(output, kAudioObjectPropertyScopeInput)];
    let mut mic_sub_device = None;
    if let Some((mic_id, mic_uid)) = &mic {
        if *mic_uid == output_uid {
            mic_sub_device = Some(0);
        } else {
            sub_device_uids.push(mic_uid.clone());
            sub_device_counts.push(hal::channel_counts(*mic_id, kAudioObjectPropertyScopeInput));
            mic_sub_device = Some(1);
        }
    }
    let tap_uids: Vec<String> = tap.iter().map(|t| t.uid.clone()).collect();
    let aggregate = hal::AggregateDevice::new("Steno capture (rust spike)", &output_uid, &sub_device_uids, &tap_uids)?;
    println!("aggregate: id {} uid {}", aggregate.id, aggregate.uid);

    if aggregate.nominal_sample_rate() != SAMPLE_RATE as f64 {
        if let Err(e) = aggregate.set_nominal_sample_rate(SAMPLE_RATE as f64) {
            println!("set nominal rate: {e}");
        }
    }
    let mut rate = aggregate.nominal_sample_rate();
    let settle = Instant::now();
    while rate != SAMPLE_RATE as f64 && settle.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(50));
        rate = aggregate.nominal_sample_rate();
    }
    if rate != SAMPLE_RATE as f64 {
        return Err(format!("aggregate stayed at {rate} Hz, wanted 48000"));
    }
    println!("aggregate rate: {} Hz (buffer frame size {})", rate as u32, hal::buffer_frame_size(aggregate.id));

    let aggregate_counts = aggregate.input_channel_counts();
    let tap_counts = tap.as_ref().map(|t| t.buffer_channel_counts()).unwrap_or_default();
    let layout = StreamLayout::resolve(&options.lanes, &aggregate_counts, &sub_device_counts, &tap_counts, mic_sub_device)?;
    println!("layout: tap first = {} (aggregate buffers {:?}, sub-devices {:?}, tap {:?})", layout.tap_first, aggregate_counts, sub_device_counts, tap_counts);
    for source in &layout.sources {
        println!("  {}: {}{}", source.lane.name(), describe(&source.left), source.right.map(|r| format!(" + {}", describe(&r))).unwrap_or_default());
    }
    let input_latency = mic.as_ref().map(|(id, _)| hal::latency_frames(*id, kAudioObjectPropertyScopeInput)).unwrap_or(0);
    let output_latency = hal::latency_frames(output, kAudioObjectPropertyScopeOutput);
    println!("input latency + safety offset: {input_latency} frames");
    println!("output latency + safety offset: {output_latency} frames");

    let ctx = Box::new(CallbackContext {
        rings: LaneRings::new(options.lanes.len(), SAMPLE_RATE * 2),
        sources: layout.sources.clone(),
        callbacks: AtomicU64::new(0),
        max_ns: AtomicU64::new(0),
        total_ns: AtomicU64::new(0),
        frames: AtomicU64::new(0),
        first_callback: AtomicU64::new(0),
    });
    let ctx_ptr: *const CallbackContext = &*ctx;
    let start_requested = START.elapsed();
    let io = unsafe { hal::IoProc::start(aggregate.id, Some(io_proc), ctx_ptr as *mut c_void)? };
    println!("recording {} s ...", options.seconds);

    // Consumer: drain whole 10 ms frames from every ring into memory and run AEC.
    let stop = Arc::new(AtomicBool::new(false));
    let consumer = {
        let stop = stop.clone();
        let lanes = options.lanes.clone();
        let aec_enabled = options.aec && lanes.len() == 2;
        let delay_frames = {
            let total = input_latency as usize + output_latency as usize;
            if total >= FRAME { total } else { 0 }
        };
        let tail = options.tail_ms * SAMPLE_RATE / 1000;
        // SAFETY: ctx outlives the consumer (joined before ctx drops).
        let ctx_ref: &'static CallbackContext = unsafe { &*ctx_ptr };
        std::thread::spawn(move || {
            let mut lanes_out: Vec<Vec<f32>> = lanes.iter().map(|_| Vec::with_capacity(SAMPLE_RATE * 12)).collect();
            let mut aec_out: Vec<f32> = Vec::new();
            let mut frame_buffers: Vec<Vec<f32>> = lanes.iter().map(|_| vec![0.0; FRAME]).collect();
            let mut aec = if aec_enabled { speex::SpeexEchoCanceller::new(SAMPLE_RATE as u32, FRAME, tail).ok() } else { None };
            let delay_line = if aec.is_some() && delay_frames > 0 {
                let line = LaneRing::new(delay_frames + FRAME);
                line.write_zeros(delay_frames);
                Some(line)
            } else {
                None
            };
            let mut delayed = vec![0.0f32; FRAME];
            let mut processed = vec![0.0f32; FRAME];
            let mut max_queue = 0usize;
            loop {
                let available = ctx_ref.rings.available_to_read();
                max_queue = max_queue.max(available);
                if available >= FRAME {
                    for (lane, buffer) in frame_buffers.iter_mut().enumerate() {
                        ctx_ref.rings.rings[lane].read(buffer);
                        lanes_out[lane].extend_from_slice(buffer);
                    }
                    if let Some(aec) = aec.as_mut() {
                        let far: &[f32] = match &delay_line {
                            Some(line) => {
                                unsafe { line.write(frame_buffers[1].as_ptr(), FRAME, 1) };
                                line.read(&mut delayed);
                                &delayed
                            }
                            None => &frame_buffers[1],
                        };
                        aec.process(&frame_buffers[0], far, &mut processed);
                        aec_out.extend_from_slice(&processed);
                    }
                    continue;
                }
                if stop.load(Ordering::Acquire) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            (lanes_out, aec_out, delay_frames, max_queue)
        })
    };

    std::thread::sleep(Duration::from_secs_f64(options.seconds));
    let callbacks_before_stop = ctx.callbacks.load(Ordering::Relaxed);
    drop(io);
    stop.store(true, Ordering::Release);
    let (lanes_out, aec_out, delay_frames, max_queue) = consumer.join().map_err(|_| "consumer thread panicked")?;
    drop(aggregate);
    drop(tap);

    // Report.
    let callbacks = ctx.callbacks.load(Ordering::Relaxed);
    let frames = ctx.frames.load(Ordering::Relaxed);
    println!();
    println!("first callback: {:.1} ms after AudioDeviceStart", (ctx.first_callback.load(Ordering::Relaxed) as f64 - start_requested.as_micros() as f64) / 1000.0);
    println!("callbacks: {callbacks} ({} before stop), frames {} ({:.2} s), mean {} frames/callback", callbacks_before_stop, frames, frames as f64 / SAMPLE_RATE as f64, if callbacks > 0 { frames / callbacks } else { 0 });
    println!(
        "IOProc duration: max {:.1} us, mean {:.1} us",
        ctx.max_ns.load(Ordering::Relaxed) as f64 / 1000.0,
        if callbacks > 0 { ctx.total_ns.load(Ordering::Relaxed) as f64 / callbacks as f64 / 1000.0 } else { 0.0 }
    );
    println!("allocations inside IOProc: {} (process total {})", rt::CALLBACK_ALLOCS.load(Ordering::Relaxed), rt::TOTAL_ALLOCS.load(Ordering::Relaxed));
    let dropped: Vec<String> = options.lanes.iter().zip(ctx.rings.rings.iter()).filter(|(_, r)| r.dropped_samples() > 0).map(|(l, r)| format!("{}: {}", l.name(), r.dropped_samples())).collect();
    println!("dropped frames: {}", if dropped.is_empty() { "none".to_string() } else { dropped.join(", ") });
    println!("max ring occupancy seen by consumer: {max_queue} frames ({:.1} ms)", max_queue as f64 * 1000.0 / SAMPLE_RATE as f64);

    let mut onsets: Vec<(Lane, f64)> = Vec::new();
    for (lane, samples) in options.lanes.iter().zip(lanes_out.iter()) {
        let onset = metrics::onset(samples, -30.0, SAMPLE_RATE);
        if let Some(o) = onset {
            onsets.push((*lane, o));
        }
        println!(
            "{}: rms {:.1} dBFS, peak {:.1} dBFS, 1 kHz {:.1} dBFS, idle floor {:.1} dBFS, onset {}",
            lane.name(),
            metrics::decibels(metrics::rms(samples)),
            metrics::decibels(metrics::peak(samples)),
            metrics::decibels(metrics::tone_level(samples, 1000.0, SAMPLE_RATE as f64)),
            metrics::decibels(metrics::idle_floor(samples, SAMPLE_RATE)),
            onset.map(|o| format!("{o:.3} s")).unwrap_or("none".into())
        );
    }
    let mic_onset = onsets.iter().find(|(l, _)| *l == Lane::Mic).map(|(_, o)| *o);
    let system_onset = onsets.iter().find(|(l, _)| *l == Lane::System).map(|(_, o)| *o);
    if let (Some(m), Some(s)) = (mic_onset, system_onset) {
        println!("mic - system onset: {:.1} ms", (m - s) * 1000.0);
    }
    if lanes_out.len() == 2 {
        if let Some(lag) = metrics::envelope_lag_ms(&lanes_out[1], &lanes_out[0], SAMPLE_RATE, 150) {
            println!("mic lags system by (envelope cross-correlation, 1 ms blocks): {lag} ms");
        }
    }

    // Files.
    let out = |name: &str| format!("{}/{}", options.out.trim_end_matches('/'), name);
    for (lane, samples) in options.lanes.iter().zip(lanes_out.iter()) {
        write_wav(&out(&format!("{}.wav", lane.name())), &[samples])?;
    }
    if lanes_out.len() == 2 {
        write_wav(&out("master.wav"), &[&lanes_out[0], &lanes_out[1]])?;
    }
    if !aec_out.is_empty() {
        write_wav(&out("mic-aec.wav"), &[&aec_out])?;
        let mic_lane = &lanes_out[0];
        println!();
        println!("AEC: speex, frame {FRAME}, tail {} ms, far-end delayed {delay_frames} frames", options.tail_ms);
        let seconds = aec_out.len() / SAMPLE_RATE;
        for second in 0..seconds {
            let range = second * SAMPLE_RATE..(second + 1) * SAMPLE_RATE;
            println!("  {second:3} s: ERLE {:5.1} dB  (mic {:.1} dBFS, aec {:.1} dBFS)", metrics::erle(mic_lane, &aec_out, range.clone()), metrics::decibels(metrics::rms(&mic_lane[range.clone()])), metrics::decibels(metrics::rms(&aec_out[range])));
        }
        println!("ERLE overall {:.1} dB", metrics::erle(mic_lane, &aec_out, 0..aec_out.len()));
        if let Some(s) = system_onset {
            let start = (s * SAMPLE_RATE as f64) as usize;
            let end = (start + 3 * SAMPLE_RATE).min(aec_out.len());
            println!(
                "ERLE on tone segment {:.2}-{:.2} s: {:.1} dB; 1 kHz in mic {:.1} dBFS, in mic-aec {:.1} dBFS",
                s,
                end as f64 / SAMPLE_RATE as f64,
                metrics::erle(mic_lane, &aec_out, start..end),
                metrics::decibels(metrics::tone_level(&mic_lane[start..end], 1000.0, SAMPLE_RATE as f64)),
                metrics::decibels(metrics::tone_level(&aec_out[start..end], 1000.0, SAMPLE_RATE as f64))
            );
        }
    }
    println!("wrote {}", options.out);
    Ok(())
}

/// `--synthetic`: the Swift `aec-bench --synthetic` scenario on the vendored
/// Speex, printed in the same shape, so the two builds can be compared.
fn run_synthetic(tail_ms: usize) -> Result<(), String> {
    let far = synthetic::speech_like_far(6.0);
    let near = synthetic::echo_mic(&far, &synthetic::room_impulse_response());
    let mut aec = speex::SpeexEchoCanceller::new(SAMPLE_RATE as u32, FRAME, tail_ms * SAMPLE_RATE / 1000)?;
    let frames = near.len().min(far.len()) / FRAME;
    let mut processed = vec![0.0f32; frames * FRAME];
    let started = Instant::now();
    for frame in 0..frames {
        let range = frame * FRAME..(frame + 1) * FRAME;
        let (n, f) = (&near[range.clone()], &far[range.clone()]);
        let out = &mut processed[range];
        aec.process(n, f, out);
    }
    let elapsed = started.elapsed();
    println!("engine: speex (vendored, rust), tail {tail_ms} ms, {} frames", processed.len());
    println!(
        "mic {:.1} dBFS, far {:.1} dBFS, processed {:.1} dBFS",
        metrics::decibels(metrics::rms(&near[..processed.len()])),
        metrics::decibels(metrics::rms(&far[..processed.len()])),
        metrics::decibels(metrics::rms(&processed))
    );
    for second in 0..processed.len() / SAMPLE_RATE {
        println!("  {second:3} s: ERLE {:5.1} dB", metrics::erle(&near, &processed, second * SAMPLE_RATE..(second + 1) * SAMPLE_RATE));
    }
    println!(
        "ERLE overall {:.1} dB, after 3 s {:.1} dB",
        metrics::erle(&near, &processed, 0..processed.len()),
        metrics::erle(&near, &processed, (3 * SAMPLE_RATE).min(processed.len())..processed.len())
    );
    println!(
        "processing time {:.1} ms for {:.1} s of audio ({:.1} us per 10 ms frame)",
        elapsed.as_secs_f64() * 1000.0,
        processed.len() as f64 / SAMPLE_RATE as f64,
        elapsed.as_secs_f64() * 1e6 / frames as f64
    );
    Ok(())
}

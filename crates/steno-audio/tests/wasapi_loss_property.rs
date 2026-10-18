//! Property tests over WASAPI's loss counting: random packet streams
//! (discontinuities, timestamp errors, idles, odd position units, broken
//! QPCs, rebuilds) through `DevicePositions`, the master's `PacketRouter`
//! and the recorder's note. Invariants:
//!
//! 1. a flagged packet counts at least what was lost before it, less the
//!    clocks' resolution;
//! 2. the recorder's note counts at least everything lost;
//! 3. no zeros are written without a sane QPC;
//! 4. no packet fills more than was lost or idle before it;
//! 5. the sink's counts and the lane's length are what was reported;
//! 6. on a unit the stream can explain, a flagged packet counts at most
//!    `OVER` times what was lost or idle before it.
//!
//! A loss nothing can read is exempt from 1 and 2. Fixed seed, bounded
//! case count (`WASAPI_PROPERTY_CASES`, `WASAPI_CALL_CASES`,
//! `WASAPI_PROPERTY_SEED`); a failure is shrunk before it is reported.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::similar_names,
    clippy::format_push_string
)]

use std::sync::Arc;

use steno_audio::SAMPLE_RATE;
use steno_audio::capture::SplitStreamPlan;
use steno_audio::capture::split_streams::{DevicePositions, Packet};
use steno_audio::realtime::{LaneFrameSink, Missed, PacketRouter, SliceView, StreamBody};
use steno_core::AudioLane;

/// Whether the generator may produce device positions that never move (as
/// the CI runner's process loopback reports them).
const STUCK_POSITIONS: bool = true;
/// Under 30 s in a debug build on CI; 20 000 locally before a push.
const DEFAULT_CASES: usize = 1_000;
const DEFAULT_SEED: u64 = 0x5EED_A18C_0005;
const ENGINE: u64 = SAMPLE_RATE as u64;
const QPC_PER_SECOND: u64 = 10_000_000;

// --- the generator --------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, per_thousand: u64) -> bool {
        self.below(1000) < per_thousand
    }
    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len() as u64) as usize]
    }
}

/// What the device positions count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Frames,
    /// The engine's frames at the mix format's rate (48 kHz without one).
    Engine,
    Rate(u64),
    Stuck,
}

/// What `GetBuffer` gives for the QPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qpc {
    Present,
    Absent,
    Stuck,
    Double,
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A packet the engine delivers.
    Packet {
        frames: usize,
        silent: bool,
        timestamp_error: bool,
        spurious: bool,
    },
    /// The engine dropped this many frames; the next packet is flagged.
    Loss(usize),
    /// Endpoint loopback with nothing playing: no packets, the device runs
    /// on, nothing is lost; the packet after may be flagged.
    Idle { frames: usize, flagged: bool },
    /// A new client (a rebuild): fresh positions, its first packet's flag
    /// ignored, a loss before it the session's gap.
    Rebuild,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    pub engine_rate: Option<u32>,
    pub unit: Unit,
    pub qpc: Qpc,
    pub start: u64,
    /// The engine's packet in 48 kHz frames; losses are whole packets.
    pub period: usize,
    /// Packets each client's device positions stand still for before they
    /// move (process loopback starting in silence); 0 for most streams.
    pub still: usize,
    pub events: Vec<Event>,
}

impl Case {
    fn unit_rate(&self) -> Option<u64> {
        match self.unit {
            Unit::Frames => Some(ENGINE),
            Unit::Engine => Some(u64::from(self.engine_rate.unwrap_or(48_000))),
            Unit::Rate(rate) => Some(rate),
            Unit::Stuck => None,
        }
    }
}

fn generate(rng: &mut Rng) -> Case {
    let engine_rate = rng.pick(&[
        None,
        Some(8_000),
        Some(16_000),
        Some(22_050),
        Some(44_100),
        Some(48_000),
        Some(48_000),
        Some(96_000),
        Some(192_000),
    ]);
    let unit = match rng.below(100) {
        0..35 => Unit::Frames,
        35..70 => Unit::Engine,
        // A unit neither explains: one within 2 % of the 48 kHz frames or
        // of the engine's would be admitted by the 1 % band.
        70..92 => loop {
            let rate = 8_000 + rng.below(184_001);
            let near = |of: u64| rate.abs_diff(of) * 50 <= of;
            if !near(ENGINE) && !engine_rate.is_some_and(|e| near(u64::from(e))) {
                break Unit::Rate(rate);
            }
        },
        _ if STUCK_POSITIONS => Unit::Stuck,
        _ => Unit::Frames,
    };
    let qpc = match rng.below(100) {
        0..40 => Qpc::Present,
        40..70 => Qpc::Absent,
        70..80 => Qpc::Stuck,
        80..90 => Qpc::Double,
        _ => Qpc::Random,
    };
    let period = rng.pick(&[480, 480, 480, 144, 960, 1_056]);
    let still = if rng.chance(150) {
        rng.pick(&[1, 2, 5, 50, 100, 300]) as usize
    } else {
        0
    };
    // A quarter of the streams lose a few packets once or twice, under the
    // recorder's drift allowance, so only a count reads it.
    let sparse = rng.chance(250);
    let packets = if sparse {
        1_000 + rng.below(3_000) as usize
    } else {
        50 + rng.below(1_150) as usize
    };
    let mut events = Vec::with_capacity(packets + 64);
    for _ in 0..packets {
        if sparse && rng.below(packets as u64) < 2 {
            events.push(Event::Loss((1 + rng.below(3)) as usize * period));
        } else if !sparse && rng.chance(30) {
            let periods = if rng.chance(50) {
                1 + rng.below(6_000)
            } else {
                1 + rng.below(150)
            };
            events.push(Event::Loss(periods as usize * period));
        }
        if rng.chance(10) {
            events.push(Event::Idle {
                frames: (1 + rng.below(1_000)) as usize * period,
                flagged: rng.chance(500),
            });
        }
        if rng.chance(3) {
            events.push(Event::Rebuild);
        }
        let frames = if rng.chance(10) {
            1 + rng.below(period as u64) as usize
        } else {
            period
        };
        events.push(Event::Packet {
            frames,
            silent: rng.chance(100),
            timestamp_error: rng.chance(20),
            spurious: rng.chance(5),
        });
    }
    Case {
        engine_rate,
        unit,
        qpc,
        start: rng.below(1 << 40),
        period,
        still,
        events,
    }
}

// --- the counters ---------------------------------------------------------

pub trait Counter {
    /// Whether the counter claims invariant 1, a count at each packet.
    const PER_PACKET: bool;
    fn new(engine_rate: Option<u32>) -> Self;
    fn packet(&mut self, packet: Packet) -> Missed;
    fn fills(&self) -> bool;
}

pub struct Code(DevicePositions);

impl Counter for Code {
    const PER_PACKET: bool = true;
    fn new(engine_rate: Option<u32>) -> Self {
        Self(DevicePositions::new(engine_rate))
    }
    fn packet(&mut self, packet: Packet) -> Missed {
        self.0.packet(packet)
    }
    fn fills(&self) -> bool {
        self.0.fills()
    }
}

/// The reference shape: the QPC alone, filled once sane, nothing read
/// without it. It is `DevicePositions` with the positions hidden.
pub struct QpcAlone(DevicePositions);

impl Counter for QpcAlone {
    const PER_PACKET: bool = false;
    fn new(engine_rate: Option<u32>) -> Self {
        Self(DevicePositions::new(engine_rate))
    }
    fn packet(&mut self, packet: Packet) -> Missed {
        let missed = self.0.packet(Packet {
            position: 0,
            ..packet
        });
        if self.0.fills() && !packet.timestamp_error {
            missed
        } else {
            Missed::default()
        }
    }
    fn fills(&self) -> bool {
        self.0.fills()
    }
}

// --- the run --------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub invariant: &'static str,
    pub event: usize,
    pub lost: usize,
    pub idle: usize,
    pub reported: Missed,
    pub fills: bool,
    pub timestamp_error: bool,
    pub previous_timestamp_error: bool,
}

/// The recorder's allowance for the device clocks' drift against the wall
/// clock (`CLOCK_DRIFT` in `steno-services`' recorder): 0.1 %.
const CLOCK_DRIFT: f64 = 0.001;

/// The most a count may exceed the loss and idle it reads, for a unit the
/// stream can explain (invariant 6): at most 48 / 8 = 6 (8 kHz mix format,
/// 48 kHz positions).
const OVER: usize = 6;

/// Runs `case` through a counter of type `C` and the master's router on a
/// system-only capture: the first invariant broken, if any.
pub fn run<C: Counter>(case: &Case) -> Result<(), Failure> {
    run_with::<C>(case, true)
}

#[allow(clippy::too_many_lines)]
pub fn run_with<C: Counter>(case: &Case, with_router: bool) -> Result<(), Failure> {
    let lanes = [AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let mut body = StreamBody::Master {
        router: PacketRouter::new(plan.layout.sources.clone(), None, 4_800),
        sink: Arc::clone(&sink),
    };
    let samples = vec![0.25f32; 2 * 2_000];
    let unit = case.unit_rate();
    let (mut t, mut client_start) = (case.start, case.start);
    let mut counter = C::new(case.engine_rate);
    let (mut first_packet, mut flag, mut lost, mut idle) = (true, false, 0usize, 0usize);
    let mut previous_timestamp_error = false;
    // Clean pairs of packets with no idle between them, since the client
    // started.
    let mut clean_pairs = 0usize;
    // Clean pairs over which the positions moved.
    let mut moving_pairs = 0usize;
    let mut was_moving = false;
    let mut jitter = Rng(case.start ^ 0xA5A5);
    let (mut delivered, mut written_fill, mut lane) = (0usize, 0usize, 0usize);
    let (mut reported_fill, mut reported_unfilled) = (0usize, 0usize);
    let (mut wall, mut gap) = (None::<(u64, u64)>, 0usize);
    let (mut total_lost, mut exempt, mut total_slack) = (0usize, 0usize, 0usize);
    for (index, event) in case.events.iter().enumerate() {
        match *event {
            Event::Loss(frames) => {
                t += frames as u64;
                lost += frames;
                flag = true;
            }
            Event::Idle { frames, flagged } => {
                t += frames as u64;
                idle += frames;
                flag |= flagged;
            }
            Event::Rebuild => {
                if wall.is_some() {
                    total_lost += lost;
                    gap += lost + idle;
                }
                counter = C::new(case.engine_rate);
                (first_packet, flag, lost, idle) = (true, false, 0, 0);
                clean_pairs = 0;
                moving_pairs = 0;
                was_moving = false;
                client_start = t;
                previous_timestamp_error = false;
            }
            Event::Packet {
                frames,
                silent,
                timestamp_error,
                spurious,
            } => {
                let since = t - client_start;
                let still_frames = (case.still * case.period) as u64;
                let moving = since > still_frames;
                let since = since.saturating_sub(still_frames);
                let position = match unit {
                    Some(rate) => {
                        (u128::from(since) * u128::from(rate) / u128::from(ENGINE)) as u64
                    }
                    None => 0,
                };
                let time = u128::from(t) * u128::from(QPC_PER_SECOND) / u128::from(ENGINE);
                let qpc = if timestamp_error {
                    jitter.next() >> 8
                } else {
                    match case.qpc {
                        Qpc::Present => {
                            (time as u64 + 1_000_000_000_000).saturating_add(jitter.below(60))
                        }
                        Qpc::Absent => 0,
                        Qpc::Stuck => 1_000_000_000_000,
                        Qpc::Double => 2 * time as u64 + 1_000_000_000_000,
                        Qpc::Random => jitter.next() >> 8,
                    }
                };
                let discontinuity = (flag || spurious) && !first_packet;
                let pending = if first_packet { 0 } else { lost };
                let first = first_packet;
                first_packet = false;
                let missed = counter.packet(Packet {
                    frames,
                    position,
                    qpc,
                    discontinuity,
                    timestamp_error,
                });
                let fills = counter.fills();
                let failure = |invariant| Failure {
                    invariant,
                    event: index,
                    lost: pending,
                    idle,
                    reported: missed,
                    fills,
                    timestamp_error,
                    previous_timestamp_error,
                };
                let resolution =
                    unit.map_or(2, |rate| 2 * ENGINE.div_ceil(rate.min(ENGINE)) as usize + 4);
                let slack = resolution
                    + if fills {
                        pending / 50 + 1
                    } else {
                        pending / 1_000 + 1
                    };
                let counted = missed.fill.saturating_add(missed.unfilled);
                // Before a client's first clean pair, a unit neither 48 kHz
                // nor the mix format explains leaves only the QPC.
                let explained = match case.unit {
                    Unit::Frames => true,
                    Unit::Engine => case.engine_rate.is_some(),
                    Unit::Rate(_) | Unit::Stuck => false,
                };
                // Positions that never move leave only the QPC, which a
                // timestamp error on this packet or the one before rules
                // out.
                let qpc_readable = case.qpc == Qpc::Present
                    && clean_pairs > 0
                    && !timestamp_error
                    && !previous_timestamp_error;
                let unreadable = !qpc_readable
                    && (case.unit == Unit::Stuck
                        || !moving
                        || !was_moving
                        || !explained && moving_pairs == 0);
                let unreadable = unreadable && !EXEMPT_NOTHING.with(std::cell::Cell::get);
                if C::PER_PACKET && counted + slack < pending && !unreadable {
                    return Err(failure("counted below the loss"));
                }
                if missed.fill > 0 && !fills {
                    return Err(failure("zeros written without a sane QPC"));
                }
                if missed.fill > pending + idle + slack {
                    return Err(failure("filled above the loss and idle"));
                }
                // Invariant 6: a reading scales the whole advance, the last
                // packet's frames included, before they come off.
                let bounded = !matches!(case.unit, Unit::Rate(_));
                if bounded && counted > OVER * (pending + idle + case.period + resolution) {
                    return Err(failure("counted above OVER x (loss + idle)"));
                }
                total_lost += pending;
                if pending > 0 {
                    total_slack += slack;
                }
                if unreadable {
                    exempt += pending;
                }
                reported_fill += missed.fill;
                reported_unfilled += missed.unfilled;
                let next = SliceView {
                    frames,
                    channels: 2,
                    samples: (!silent).then(|| &samples[..2 * frames]),
                };
                if with_router {
                    if !missed.is_none() {
                        let before = sink.ring(0).available_to_read();
                        body.write_missed(&next, missed);
                        written_fill += sink.ring(0).available_to_read() - before;
                    }
                    body.handle(next);
                    let ring = sink.ring(0);
                    lane += ring.discard(ring.available_to_read());
                }
                let begin = wall.map_or(t, |(begin, _)| begin);
                delivered += frames;
                t += frames as u64;
                wall = Some((begin, t));
                if !discontinuity
                    && !timestamp_error
                    && !previous_timestamp_error
                    && !first
                    && idle == 0
                {
                    clean_pairs += 1;
                    if was_moving {
                        moving_pairs += 1;
                    }
                }
                was_moving = moving;
                (flag, lost, idle) = (false, 0, 0);
                previous_timestamp_error = timestamp_error;
            }
        }
    }
    // The recorder's note: the gaps, the fills, and the larger of the
    // unfilled count and the shortfall beyond the drift allowance.
    let wall = wall.map_or(0, |(begin, end)| end - begin) as f64;
    let held = (delivered + reported_fill + gap) as f64;
    let short = wall - held - wall * CLOCK_DRIFT;
    let note = (gap + reported_fill) as f64 + (reported_unfilled as f64).max(short);
    if note + (total_slack as f64) < (total_lost - exempt) as f64 {
        return Err(Failure {
            invariant: "the note below the loss",
            event: case.events.len(),
            lost: total_lost - exempt,
            idle: note as usize,
            reported: Missed {
                fill: reported_fill,
                unfilled: reported_unfilled,
            },
            fills: counter.fills(),
            timestamp_error: false,
            previous_timestamp_error: false,
        });
    }
    if !with_router {
        return Ok(());
    }
    let filled = sink.filled_samples().values().sum::<usize>();
    let unfilled = sink.unfilled_samples().values().sum::<usize>();
    assert_eq!(
        (filled, unfilled),
        (reported_fill, reported_unfilled),
        "the sink's counts"
    );
    assert_eq!(written_fill, reported_fill, "the zeros written");
    assert_eq!(lane, delivered + reported_fill, "the lane's length");
    Ok(())
}

// --- shrinking ------------------------------------------------------------

fn same_class(a: &Failure, b: &Failure) -> bool {
    a.invariant == b.invariant
        && a.fills == b.fills
        && a.timestamp_error == b.timestamp_error
        && a.previous_timestamp_error == b.previous_timestamp_error
}

/// The smallest case found that still fails like `failure`.
pub fn shrink<C: Counter>(case: &Case, failure: &Failure) -> (Case, Failure) {
    let mut best = case.clone();
    let mut best_failure = failure.clone();
    let budget = std::cell::Cell::new(20_000usize);
    let try_case = |candidate: &Case, best_failure: &Failure| -> Option<Failure> {
        if budget.get() == 0 {
            return None;
        }
        budget.set(budget.get() - 1);
        match run_with::<C>(candidate, false) {
            Err(f) if same_class(&f, best_failure) => Some(f),
            _ => None,
        }
    };
    let mut cut = best.clone();
    cut.events.truncate(best_failure.event + 1);
    if let Some(f) = try_case(&cut, &best_failure) {
        (best, best_failure) = (cut, f);
    }
    loop {
        let mut changed = false;
        // Remove chunks, halving their size (delta debugging).
        let mut chunk = best.events.len() / 2;
        while chunk >= 1 {
            let mut at = 0;
            while at + chunk <= best.events.len() {
                let mut candidate = best.clone();
                candidate.events.drain(at..at + chunk);
                if let Some(f) = try_case(&candidate, &best_failure) {
                    (best, best_failure) = (candidate, f);
                    changed = true;
                } else {
                    at += chunk;
                }
            }
            chunk /= 2;
        }
        for i in 0..best.events.len() {
            let simpler: Vec<Event> = match best.events[i] {
                Event::Loss(frames) if frames > best.period => vec![
                    Event::Loss((frames / 2).div_ceil(best.period) * best.period),
                    Event::Loss(best.period),
                ],
                Event::Idle { frames, flagged } if frames > best.period => vec![Event::Idle {
                    frames: (frames / 2).div_ceil(best.period) * best.period,
                    flagged,
                }],
                Event::Packet {
                    frames,
                    silent,
                    timestamp_error,
                    spurious,
                } => {
                    let mut v = Vec::new();
                    if silent {
                        v.push(Event::Packet {
                            frames,
                            silent: false,
                            timestamp_error,
                            spurious,
                        });
                    }
                    if spurious {
                        v.push(Event::Packet {
                            frames,
                            silent,
                            timestamp_error,
                            spurious: false,
                        });
                    }
                    if timestamp_error {
                        v.push(Event::Packet {
                            frames,
                            silent,
                            timestamp_error: false,
                            spurious,
                        });
                    }
                    if frames != best.period {
                        v.push(Event::Packet {
                            frames: best.period,
                            silent,
                            timestamp_error,
                            spurious,
                        });
                    }
                    v
                }
                _ => Vec::new(),
            };
            for event in simpler {
                let mut candidate = best.clone();
                candidate.events[i] = event;
                if let Some(f) = try_case(&candidate, &best_failure) {
                    (best, best_failure) = (candidate, f);
                    changed = true;
                    break;
                }
            }
        }
        for (unit, qpc, rate, start, still) in [
            (
                Unit::Frames,
                best.qpc,
                best.engine_rate,
                best.start,
                best.still,
            ),
            (
                best.unit,
                Qpc::Absent,
                best.engine_rate,
                best.start,
                best.still,
            ),
            (best.unit, best.qpc, Some(48_000), best.start, best.still),
            (best.unit, best.qpc, best.engine_rate, 0, best.still),
            (best.unit, best.qpc, best.engine_rate, best.start, 0),
        ] {
            let candidate = Case {
                unit,
                qpc,
                engine_rate: rate,
                start,
                period: best.period,
                still,
                events: best.events.clone(),
            };
            if candidate != best
                && let Some(f) = try_case(&candidate, &best_failure)
            {
                (best, best_failure) = (candidate, f);
                changed = true;
            }
        }
        if !changed || budget.get() == 0 {
            return (best, best_failure);
        }
    }
}

/// The events as a short line, runs of the same packet as `N x`.
pub fn describe(case: &Case) -> String {
    let mut out = format!(
        "engine {:?}, unit {:?}, qpc {:?}, period {}, still {}, start {}: ",
        case.engine_rate, case.unit, case.qpc, case.period, case.still, case.start
    );
    let word = |event: &Event| match *event {
        Event::Packet {
            frames,
            silent,
            timestamp_error,
            spurious,
        } => format!(
            "P{}{}{}{}",
            if frames == case.period {
                String::new()
            } else {
                format!("({frames})")
            },
            if silent { "s" } else { "" },
            if timestamp_error { "T" } else { "" },
            if spurious { "D" } else { "" }
        ),
        Event::Loss(frames) => format!("LOSS({frames})"),
        Event::Idle { frames, flagged } => {
            format!("IDLE({frames}{})", if flagged { ",flagged" } else { "" })
        }
        Event::Rebuild => "REBUILD".to_string(),
    };
    let mut i = 0;
    while i < case.events.len() {
        let w = word(&case.events[i]);
        let mut n = 1;
        while i + n < case.events.len() && word(&case.events[i + n]) == w {
            n += 1;
        }
        if n > 1 {
            out.push_str(&format!("{n}x{w} "));
        } else {
            out.push_str(&format!("{w} "));
        }
        i += n;
    }
    out
}

fn settings() -> (usize, u64) {
    let cases = std::env::var("WASAPI_PROPERTY_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_CASES);
    let seed = std::env::var("WASAPI_PROPERTY_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_SEED);
    (cases, seed)
}

fn explore<C: Counter>(name: &str, skip: impl Fn(&Case) -> bool) -> Vec<String> {
    let (cases, seed) = settings();
    let mut rng = Rng(seed);
    let mut classes: Vec<(Failure, usize, String)> = Vec::new();
    let (mut failed, mut ran) = (0, 0);
    for _ in 0..cases {
        let case = generate(&mut rng);
        if skip(&case) {
            continue;
        }
        ran += 1;
        let begun = std::time::Instant::now();
        let outcome = run::<C>(&case);
        if std::env::var_os("WASAPI_PROPERTY_TRACE").is_some() {
            eprintln!(
                "{name} case {ran}: {} events, {:?}, {:?}",
                case.events.len(),
                begun.elapsed(),
                outcome.as_ref().err().map(|f| f.invariant)
            );
        }
        if let Err(failure) = outcome {
            failed += 1;
            if let Some(class) = classes.iter_mut().find(|(f, _, _)| same_class(f, &failure)) {
                class.1 += 1;
                continue;
            }
            let begun = std::time::Instant::now();
            let (small, small_failure) = shrink::<C>(&case, &failure);
            if std::env::var_os("WASAPI_PROPERTY_TRACE").is_some() {
                eprintln!(
                    "{name}: shrunk to {} events in {:?}",
                    small.events.len(),
                    begun.elapsed()
                );
            }
            let line = format!("{small_failure:?}\n    {}", describe(&small));
            classes.push((failure, 1, line));
        }
    }
    let lines: Vec<String> = classes
        .iter()
        .map(|(_, n, line)| format!("[{n} cases] {line}"))
        .collect();
    eprintln!(
        "{name}: seed {seed:#x}, {ran} cases, {failed} failed, {} classes",
        lines.len()
    );
    for line in &lines {
        eprintln!("  {line}");
    }
    lines
}

/// The code keeps every invariant on every stream.
#[test]
fn wasapi_counts_every_loss_and_fills_only_on_a_sane_qpc() {
    let classes = explore::<Code>("code", |_| false);
    assert!(
        classes.is_empty(),
        "{} classes of failure:\n{}",
        classes.len(),
        classes.join("\n")
    );
}

/// The QPC alone, with the recorder's wall-time shortfall behind it, does
/// not keep every invariant: without a sane QPC the drift allowance hides
/// small losses, which is why the code keeps the device positions.
#[test]
fn the_qpc_alone_misses_what_only_the_positions_read() {
    let (cases, seed) = settings();
    let mut rng = Rng(seed);
    let (mut failed, mut most_missing) = (0, 0.0f64);
    for _ in 0..cases {
        let case = generate(&mut rng);
        let Err(failure) = run::<QpcAlone>(&case) else {
            continue;
        };
        failed += 1;
        let context = format!("{failure:?}\n{}", describe(&case));
        assert_eq!(failure.invariant, "the note below the loss", "{context}");
        let missing = failure.lost.saturating_sub(failure.idle) as f64;
        assert!(
            missing <= wall_frames(&case) * CLOCK_DRIFT + 2.0 * 1_056.0,
            "more than the drift allowance: {context}"
        );
        most_missing = most_missing.max(missing / f64::from(SAMPLE_RATE as u32));
        assert_eq!(
            run::<Code>(&case),
            Ok(()),
            "the positions read it: {context}"
        );
    }
    eprintln!(
        "the QPC alone: seed {seed:#x}, {cases} cases, {failed} failed, the note short by up to \
         {most_missing:.3} s"
    );
    assert!(failed > 0, "the QPC alone kept every invariant");
}

fn wall_frames(case: &Case) -> f64 {
    let (mut t, mut begin, mut end) = (0u64, None, 0u64);
    for event in &case.events {
        match *event {
            Event::Loss(frames) | Event::Idle { frames, .. } => t += frames as u64,
            Event::Rebuild => {}
            Event::Packet { frames, .. } => {
                begin.get_or_insert(t);
                t += frames as u64;
                end = t;
            }
        }
    }
    (end - begin.unwrap_or(end)) as f64
}

/// The same streams with no loss exempt, for a report.
#[test]
#[ignore = "exploration for a report"]
fn explore_everything() {
    EXEMPT_NOTHING.with(|exempt| exempt.set(true));
    explore::<Code>("code, all", |_| false);
    explore::<QpcAlone>("the QPC alone, all", |_| false);
}

thread_local! {
    static EXEMPT_NOTHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

// --- a call: the microphone master and the system follower ---------------
//
// Both of a call's streams as `run_stream` drives them, with engines that
// keep `keep` packets while a thread is out, idles, drifting clocks, cut
// drains and outages into the stop. Invariants, per lane:
//
// 7. the system lane counts at least what its engine lost;
// 8. the microphone lane's note counts at least what its engine lost;
// 9. each lane counts at most `OVER` times the loss and idle before its
//    flagged packets, plus per flag the engine's buffer and two packets;
// 10. both lanes hold the same length.
//
// A loss no packet flags at the stop (or before the first packet) of a
// stream the counter cannot read is exempt from 7 and 8.

use std::collections::VecDeque;

use steno_audio::capture::split_streams::StreamSizes;
use steno_audio::realtime::{FollowerLane, count_at_stop};

/// Whether the engine may lose packets at the end of an outage that runs
/// into the stop with no flagged packet after them.
const STOP_WITHOUT_A_FLAG: bool = true;

fn stop_without_a_flag() -> bool {
    std::env::var("WASAPI_STOP_WITHOUT_A_FLAG").map_or(STOP_WITHOUT_A_FLAG, |v| v != "0")
}
const CALL_CASES: usize = 300;
const PERIOD: usize = 480;
const MIC: usize = 0;
const SYSTEM: usize = 1;

/// One stream's clocks in a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clocks {
    pub engine_rate: Option<u32>,
    pub unit: Unit,
    pub still: usize,
    pub qpc: Qpc,
}

impl Clocks {
    fn unit_rate(&self) -> Option<u64> {
        Case {
            engine_rate: self.engine_rate,
            unit: self.unit,
            qpc: self.qpc,
            start: 0,
            period: PERIOD,
            still: 0,
            events: Vec::new(),
        }
        .unit_rate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    Mic,
    System,
    Both,
}

impl Who {
    fn is(self, stream: usize) -> bool {
        matches!(
            (self, stream),
            (Who::Both, _) | (Who::Mic, MIC) | (Who::System, SYSTEM)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallCase {
    pub mic: Clocks,
    pub system: Clocks,
    /// The engine's buffer in packets: what it keeps while a thread is out.
    pub keep: usize,
    /// On an overrun the engine drops its oldest packet (else it refuses
    /// the newest).
    pub drop_oldest: bool,
    pub ticks: usize,
    pub outages: Vec<(Who, usize, usize)>,
    /// The system stream idles: from, for, and whether the packet after is
    /// flagged.
    pub idles: Vec<(usize, usize, bool)>,
    pub spurious: Vec<(Who, usize)>,
    pub timestamp_errors: Vec<(Who, usize)>,
    pub master_first: bool,
    pub extra_packet: bool,
    /// Per thousand: a thread's drain is cut part way and the other's runs
    /// before the rest.
    pub partial_drains: u64,
    /// The system device's clock drift against the microphone's, per
    /// million.
    pub system_drift_ppm: i64,
    pub start: u64,
}

fn generate_clocks(rng: &mut Rng, master: bool) -> Clocks {
    let case = generate(rng);
    let mut unit = case.unit;
    if master && matches!(unit, Unit::Rate(_) | Unit::Stuck) {
        // The microphone's loss is checked per lane, so its positions are
        // in a unit it can explain (the single-stream test covers the rest).
        unit = Unit::Frames;
    }
    Clocks {
        engine_rate: case.engine_rate,
        unit,
        still: if master { 0 } else { case.still },
        qpc: case.qpc,
    }
}

fn generate_call(rng: &mut Rng) -> CallCase {
    let mic = generate_clocks(rng, true);
    let system = generate_clocks(rng, false);
    let keep = rng.pick(&[10, 10, 12, 20]);
    let ticks = 300 + rng.below(2_700) as usize;
    let who = |rng: &mut Rng| match rng.below(100) {
        0..35 => Who::Mic,
        35..70 => Who::System,
        _ => Who::Both,
    };
    let mut outages = Vec::new();
    for _ in 0..rng.below(4) {
        let len = match rng.below(10) {
            0..5 => 1 + rng.below(keep as u64 + 5),
            5..9 => 1 + rng.below(150),
            _ => 1 + rng.below(300),
        } as usize;
        let from = if rng.chance(300) {
            ticks.saturating_sub(len)
        } else {
            rng.below(ticks as u64) as usize
        };
        outages.push((who(rng), from, len));
    }
    if rng.chance(150) {
        // A thread late to its first drain: an engine that drops its oldest
        // packets then loses audio before the stream's first packet.
        let len = 1 + rng.below(keep as u64 + 60) as usize;
        outages.push((who(rng), 0, len));
    }
    let mut idles = Vec::new();
    for _ in 0..rng.below(3) {
        idles.push((
            rng.below(ticks as u64) as usize,
            1 + rng.below(1_000) as usize,
            rng.chance(500),
        ));
    }
    let mut spurious = Vec::new();
    for _ in 0..rng.below(3) {
        spurious.push((who(rng), rng.below(ticks as u64) as usize));
    }
    let mut timestamp_errors = Vec::new();
    for _ in 0..rng.below(4) {
        timestamp_errors.push((who(rng), rng.below(ticks as u64) as usize));
    }
    CallCase {
        mic,
        system,
        keep,
        drop_oldest: rng.chance(500),
        ticks,
        outages,
        idles,
        spurious,
        timestamp_errors,
        master_first: rng.chance(500),
        extra_packet: !stop_without_a_flag() || rng.chance(500),
        partial_drains: if rng.chance(300) {
            rng.pick(&[100, 300, 600])
        } else {
            0
        },
        system_drift_ppm: if rng.chance(400) {
            rng.pick(&[-5_000, -1_000, -300, 300, 1_000, 5_000])
        } else {
            0
        },
        start: rng.below(1 << 40),
    }
}

/// A packet the engine holds for a thread.
#[derive(Debug, Clone, Copy)]
struct Produced {
    tick: usize,
    index: usize,
    flag: bool,
    timestamp_error: bool,
    gap: usize,
    lost: usize,
    near_idle: usize,
}

struct EngineStream {
    clocks: Clocks,
    drift_ppm: i64,
    grid: usize,
    positions: DevicePositions,
    buffer: VecDeque<Produced>,
    pending: Produced,
    lost: usize,
    first: bool,
    delivered: usize,
    /// Over every flagged packet: the loss and idle right before it, how
    /// many, and (the microphone's) the most it may count.
    flagged_gap: usize,
    flags: usize,
    /// The flagged packets of a loss that ran into the stop or overlapped
    /// the microphone's own outage: the padding may read up to two targets
    /// short.
    lag_flags: usize,
    last_idle: usize,
    since_idle: usize,
    last_error: bool,
    lost_both: usize,
    /// What was lost before the stream's first packet, whose flag is not
    /// read.
    lost_first: usize,
}

impl EngineStream {
    fn new(clocks: Clocks, drift_ppm: i64) -> Self {
        Self {
            clocks,
            drift_ppm,
            grid: 0,
            positions: DevicePositions::new(clocks.engine_rate),
            buffer: VecDeque::new(),
            pending: Produced {
                tick: 0,
                index: 0,
                flag: false,
                timestamp_error: false,
                gap: 0,
                lost: 0,
                near_idle: 0,
            },
            lost: 0,
            first: true,
            delivered: 0,
            flagged_gap: 0,
            flags: 0,
            lag_flags: 0,
            last_idle: 0,
            since_idle: usize::MAX,
            last_error: false,
            lost_both: 0,
            lost_first: 0,
        }
    }

    /// The engine's packet for `tick`, into what it keeps for the thread.
    fn produce(&mut self, tick: usize, keep: usize, drop_oldest: bool, flag: bool, error: bool) {
        let packet = Produced {
            tick,
            index: self.grid,
            flag: self.pending.flag || flag,
            timestamp_error: error,
            gap: self.pending.gap,
            lost: self.pending.lost,
            // An idle that ended within the follower lane's hold is counted
            // with a flag too.
            near_idle: if (1..=keep + 2 + keep + 1).contains(&self.since_idle) {
                self.last_idle
            } else {
                0
            },
        };
        self.since_idle = self.since_idle.saturating_add(1);
        if self.buffer.len() < keep {
            self.buffer.push_back(packet);
            self.pending = Produced {
                tick,
                index: 0,
                flag: false,
                timestamp_error: false,
                gap: 0,
                lost: 0,
                near_idle: 0,
            };
            return;
        }
        self.lost += PERIOD;
        if drop_oldest {
            let old = self.buffer.pop_front().unwrap();
            if let Some(front) = self.buffer.front_mut() {
                front.flag = true;
                front.gap += old.gap + PERIOD;
                front.lost += old.lost + PERIOD;
                self.buffer.push_back(packet);
                self.pending = Produced {
                    tick,
                    index: 0,
                    flag: false,
                    timestamp_error: false,
                    gap: 0,
                    lost: 0,
                    near_idle: 0,
                };
            } else {
                self.buffer.push_back(Produced {
                    flag: true,
                    gap: packet.gap + old.gap + PERIOD,
                    lost: packet.lost + old.lost + PERIOD,
                    ..packet
                });
                self.pending = Produced {
                    tick,
                    index: 0,
                    flag: false,
                    timestamp_error: false,
                    gap: 0,
                    lost: 0,
                    near_idle: 0,
                };
            }
        } else {
            self.pending.flag = true;
            self.pending.gap += PERIOD;
            self.pending.lost += PERIOD;
        }
    }

    /// The packets due on the device's clock by the end of `tick` not yet
    /// produced: one a tick, give or take one at the drift.
    fn due(&self, tick: usize) -> usize {
        let by =
            ((tick as i128 + 1) * (1_000_000 + i128::from(self.drift_ppm)) / 1_000_000) as usize;
        by.saturating_sub(self.grid)
    }

    /// The device's clocks at the packet `index` packets into its clock.
    fn clocks_at(&self, index: usize, start: u64, error: bool, jitter: &mut Rng) -> (u64, u64) {
        let since = (index * PERIOD).saturating_sub(self.clocks.still * PERIOD) as u64;
        let position = self.clocks.unit_rate().map_or(0, |rate| {
            (u128::from(since) * u128::from(rate) / u128::from(ENGINE)) as u64
        });
        let real = (index * PERIOD) as i128 * 1_000_000 / (1_000_000 + i128::from(self.drift_ppm));
        let t = start + real as u64;
        let time = (u128::from(t) * u128::from(QPC_PER_SECOND) / u128::from(ENGINE)) as u64;
        let qpc = if error {
            jitter.next() >> 8
        } else {
            match self.clocks.qpc {
                Qpc::Present => (time + 1_000_000_000_000).saturating_add(jitter.below(60)),
                Qpc::Absent => 0,
                Qpc::Stuck => 1_000_000_000_000,
                Qpc::Double => 2 * time + 1_000_000_000_000,
                Qpc::Random => jitter.next() >> 8,
            }
        };
        (position, qpc)
    }
}

pub fn run_call(case: &CallCase) -> Result<(), String> {
    run_call_with(case, true)
}

/// [`run_call`], the upper bounds (invariant 9 and the microphone's
/// `OVER`) only with `upper`.
#[allow(clippy::too_many_lines)]
pub fn run_call_with(case: &CallCase, upper: bool) -> Result<(), String> {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    // As the backend builds it.
    let follower = Arc::new(
        FollowerLane::for_streams_filling(PERIOD, case.keep * PERIOD)
            .with_reprime_hold((case.keep + 1) * PERIOD),
    );
    let mut master = StreamBody::Master {
        router: PacketRouter::new(
            plan.layout.sources.clone(),
            Some(Arc::clone(&follower)),
            4_800,
        ),
        sink: Arc::clone(&sink),
    };
    let mut staging = StreamBody::follower(Arc::clone(&follower), 4_800);
    let mut streams = [
        EngineStream::new(case.mic, 0),
        EngineStream::new(case.system, case.system_drift_ppm),
    ];
    // The system stream is process loopback, which delivers continuously,
    // when it never idles.
    let continuous = case.idles.is_empty();
    let started = (u128::from(case.start) * u128::from(QPC_PER_SECOND) / u128::from(ENGINE)) as u64
        + 1_000_000_000_000;
    streams[MIC].positions.start(started);
    if continuous {
        streams[SYSTEM].positions.start(started);
    }
    let mic_samples = vec![0.5f32; PERIOD];
    let tap_samples = vec![0.25f32; 2 * PERIOD];
    let mut jitter = Rng(case.start ^ 0x5A5A);
    let mut order = Rng(case.start ^ 0xC3C3);
    let mut lane_len = [0usize; 2];
    let mut skipped = 0usize;
    let resolution = |clocks: &Clocks| {
        clocks.unit_rate().map_or(2, |rate| {
            2 * ENGINE.div_ceil(rate.clamp(1, ENGINE)) as usize + 4
        })
    };
    let out = |stream: usize, tick: usize| {
        case.outages
            .iter()
            .any(|&(who, from, len)| who.is(stream) && (from..from + len).contains(&tick))
    };
    let idle = |tick: usize| {
        case.idles
            .iter()
            .find(|&&(from, len, _)| (from..from + len).contains(&tick))
            .map(|&(from, len, flagged)| (tick + 1 == from + len) && flagged)
    };
    let both_out = |tick: usize| out(MIC, tick) && out(SYSTEM, tick);
    let has = |list: &[(Who, usize)], stream: usize, tick: usize| {
        list.iter().any(|&(who, at)| who.is(stream) && at == tick)
    };
    let drain = |stream: usize,
                 tick: usize,
                 limit: usize,
                 streams: &mut [EngineStream; 2],
                 master: &mut StreamBody,
                 staging: &mut StreamBody,
                 lane_len: &mut [usize; 2],
                 skipped: &mut usize,
                 jitter: &mut Rng|
     -> Result<(), String> {
        let s = &mut streams[stream];
        for _ in 0..limit {
            let Some(produced) = s.buffer.pop_front() else {
                break;
            };
            let discontinuity = produced.flag && !s.first;
            if s.first {
                s.lost_first = produced.lost;
            }
            s.first = false;
            let (position, qpc) =
                s.clocks_at(produced.index, case.start, produced.timestamp_error, jitter);
            s.last_error = produced.timestamp_error;
            let missed = s.positions.packet(Packet {
                frames: PERIOD,
                position,
                qpc,
                discontinuity,
                timestamp_error: produced.timestamp_error,
            });
            if missed.fill > 0 && !s.positions.fills() {
                return Err(format!("stream {stream}: zeros written without a sane QPC"));
            }
            let counted = missed.fill.saturating_add(missed.unfilled);
            if discontinuity {
                s.flagged_gap += produced.gap;
                s.flagged_gap += produced.near_idle;
                s.flags += 1;
                let from = produced
                    .tick
                    .saturating_sub(produced.gap / PERIOD + case.keep + 3);
                if produced.lost > 0 && (tick == case.ticks || (from..=tick).any(|t| out(MIC, t))) {
                    s.lag_flags += 1;
                }
                if upper
                    && stream == MIC
                    && counted > OVER * (produced.gap + PERIOD + resolution(&s.clocks))
                {
                    return Err(format!(
                        "mic: counted {counted} above OVER x {} at tick {}",
                        produced.gap, produced.tick
                    ));
                }
            }
            if stream == MIC {
                let view = SliceView {
                    frames: PERIOD,
                    channels: 1,
                    samples: Some(&mic_samples),
                };
                if discontinuity {
                    let before = sink.unfilled_samples().get(&AudioLane::System).copied();
                    master.write_missed(&view, missed);
                    let after = sink.unfilled_samples().get(&AudioLane::System).copied();
                    *skipped += after.unwrap_or(0) - before.unwrap_or(0);
                }
                master.handle(view);
                for (lane, len) in lane_len.iter_mut().enumerate() {
                    let ring = sink.ring(lane);
                    *len += ring.discard(ring.available_to_read());
                }
            } else {
                let view = SliceView {
                    frames: PERIOD,
                    channels: 2,
                    samples: Some(&tap_samples),
                };
                if discontinuity {
                    staging.write_missed(&view, missed);
                }
                staging.handle(view);
            }
            s.delivered += PERIOD;
        }
        Ok(())
    };
    let produce = |streams: &mut [EngineStream; 2], stream: usize, tick: usize| {
        let flag = has(&case.spurious, stream, tick);
        let error = has(&case.timestamp_errors, stream, tick);
        for _ in 0..streams[stream].due(tick) {
            if stream == SYSTEM
                && let Some(flagged) = idle(tick)
            {
                let s = &mut streams[SYSTEM];
                s.pending.gap += PERIOD;
                s.pending.flag |= flagged;
                if s.since_idle != 0 && !(1..=2 * case.keep + 3).contains(&s.since_idle) {
                    s.last_idle = 0;
                }
                s.last_idle += PERIOD;
                s.since_idle = 0;
            } else {
                let before = streams[stream].lost;
                streams[stream].produce(tick, case.keep, case.drop_oldest, flag, error);
                if both_out(tick) {
                    streams[stream].lost_both += streams[stream].lost - before;
                }
            }
            streams[stream].grid += 1;
        }
    };
    for tick in 0..=case.ticks {
        let stop = tick == case.ticks;
        if !stop {
            for stream in [MIC, SYSTEM] {
                produce(&mut streams, stream, tick);
            }
        }
        let first = if stop {
            if case.master_first { MIC } else { SYSTEM }
        } else if order.chance(500) {
            MIC
        } else {
            SYSTEM
        };
        let mut cut = [false; 2];
        for (pass, stream) in [first, 1 - first, first, 1 - first].into_iter().enumerate() {
            if pass >= 2 {
                if cut[stream] {
                    drain(
                        stream,
                        tick,
                        usize::MAX,
                        &mut streams,
                        &mut master,
                        &mut staging,
                        &mut lane_len,
                        &mut skipped,
                        &mut jitter,
                    )?;
                }
                continue;
            }
            // At the stop every thread drains once more, and the engine may
            // produce one more packet meanwhile.
            if stop || !out(stream, tick) {
                let held = streams[stream].buffer.len();
                let limit = if !stop
                    && held > 1
                    && case.partial_drains > 0
                    && order.chance(case.partial_drains)
                {
                    cut[stream] = true;
                    1 + order.below(held as u64 - 1) as usize
                } else {
                    usize::MAX
                };
                drain(
                    stream,
                    tick,
                    limit,
                    &mut streams,
                    &mut master,
                    &mut staging,
                    &mut lane_len,
                    &mut skipped,
                    &mut jitter,
                )?;
            }
            if stop && case.extra_packet {
                produce(&mut streams, stream, tick);
                drain(
                    stream,
                    tick,
                    usize::MAX,
                    &mut streams,
                    &mut master,
                    &mut staging,
                    &mut lane_len,
                    &mut skipped,
                    &mut jitter,
                )?;
            }
        }
    }
    // The stop: the counter at wall time against each continuous stream's
    // last packet, then the backend's stop.
    let wall_frames = ((case.ticks + usize::from(case.extra_packet)) * PERIOD) as u64;
    let now = (u128::from(case.start + wall_frames) * u128::from(QPC_PER_SECOND)
        / u128::from(ENGINE)) as u64
        + 1_000_000_000_000;
    let mic_end = streams[MIC].positions.missed_until(now);
    let system_end = continuous.then(|| streams[SYSTEM].positions.missed_until(now));
    count_at_stop(&sink, Some((&follower, SYSTEM)), mic_end, system_end);
    // The recording's first start: what each continuous stream missed
    // before its first packet.
    let sizes = StreamSizes {
        buffer_frames: case.keep * PERIOD,
        period_frames: PERIOD,
        latency_frames: 0,
    };
    let mic_first = streams[MIC].positions.missed_before_first(sizes);
    let system_first = streams[SYSTEM].positions.missed_before_first(sizes);
    sink.count_before_first(mic_first, Some((SYSTEM, system_first)));
    let get = |counts: std::collections::BTreeMap<AudioLane, usize>, lane| {
        counts.get(&lane).copied().unwrap_or(0)
    };
    let filled = [
        get(sink.filled_samples(), AudioLane::Mic),
        get(sink.filled_samples(), AudioLane::System),
    ];
    let unfilled = [AudioLane::Mic, AudioLane::System]
        .map(|lane| get(sink.unfilled_samples(), lane) + get(sink.before_first_samples(), lane));
    let [mic, system] = &streams;
    // 10: one length, the microphone's frames and fills.
    if lane_len[MIC] != lane_len[SYSTEM] || lane_len[MIC] != mic.delivered + filled[MIC] {
        return Err(format!(
            "lanes {lane_len:?}, mic delivered {} filled {}",
            mic.delivered, filled[MIC]
        ));
    }
    let unflagged = |stream: &EngineStream, read: bool| {
        if read && stream.positions.fills() && !stream.last_error {
            0
        } else {
            stream.pending.lost
        }
    };
    let unread_first = |stream: &EngineStream, read: bool| {
        if read && stream.positions.qpc_sane() == Some(true) {
            0
        } else {
            stream.lost_first
        }
    };
    // 7: the system lane counts what its engine lost.
    let system_counted = filled[SYSTEM] + unfilled[SYSTEM];
    // The padding reads a loss to within the lane's lag, two targets, where
    // the master's last pulls take it.
    let lag_flags = system.lag_flags + usize::from(system.pending.lost > 0);
    let system_slack = PERIOD
        + system.lost / 100
        + system.flags * resolution(&system.clocks)
        + lag_flags * 4 * PERIOD;
    let system_lost =
        system.lost - unflagged(system, continuous) - unread_first(system, continuous);
    // Both threads out: the master counts the time on its own lane only,
    // and the note takes the lane that lost most.
    let wall = ((case.ticks + usize::from(case.extra_packet)) * PERIOD) as f64;
    let short = wall - lane_len[MIC] as f64 - wall * CLOCK_DRIFT;
    let mic_note = filled[MIC] as f64 + (unfilled[MIC] as f64).max(short);
    let covered = system_counted + system.lost_both.min(mic_note.max(0.0) as usize);
    if covered + system_slack < system_lost {
        return Err(format!(
            "system lane counted {system_counted} (filled {}, unfilled {}) below its engine's \
             loss {}",
            filled[SYSTEM], unfilled[SYSTEM], system.lost
        ));
    }
    // 8: the microphone's note counts what its engine lost.
    let mic_slack = PERIOD + mic.lost / 100 + mic.flags * resolution(&mic.clocks);
    let mic_lost = mic.lost - unflagged(mic, true) - unread_first(mic, true);
    if mic_note + (mic_slack as f64) < mic_lost as f64 {
        return Err(format!(
            "mic note {mic_note:.0} (filled {}, unfilled {}, short {short:.0}) below its \
             engine's loss {}",
            filled[MIC], unfilled[MIC], mic.lost
        ));
    }
    // 9: the stop's and the start's readings count at most what was lost
    // there.
    if upper && mic_end.unfilled > mic.pending.lost {
        return Err(format!(
            "mic: the stop read {} above the {} no packet flagged",
            mic_end.unfilled, mic.pending.lost
        ));
    }
    for (name, first, stream) in [("mic", mic_first, mic), ("system", system_first, system)] {
        if upper && first > stream.lost_first {
            return Err(format!(
                "{name}: the start read {first} above the {} lost before the first packet",
                stream.lost_first
            ));
        }
    }
    // 9: the system lane's upper bound, on a unit it can explain.
    if upper && !matches!(system.clocks.unit, Unit::Rate(_)) {
        let end_gap = system.pending.lost;
        let end_flags = usize::from(end_gap > 0 || continuous);
        // An idle that touches an outage of the system thread: the padding
        // cannot tell the idle from the loss.
        let idle_out = case
            .idles
            .iter()
            .filter(|&&(from, len, _)| {
                (from..=from + len + 3 + case.keep + 1)
                    .any(|tick| out(SYSTEM, tick) || out(SYSTEM, tick.saturating_sub(1)))
            })
            .map(|&(from, len, _)| len.min(case.ticks.saturating_sub(from)))
            .sum::<usize>()
            * PERIOD;
        // A continuous follower's padding at the stop counts the audio its
        // lane slipped in the master's last burst too.
        let slipped_at_stop = if continuous {
            follower.slipped_frames()
        } else {
            0
        };
        let most = OVER * (system.flagged_gap + idle_out + end_gap + system.lost_first)
            + (system.flags + end_flags)
                * (case.keep * PERIOD + 2 * PERIOD + OVER * (PERIOD + resolution(&system.clocks)))
            + skipped
            + slipped_at_stop
            + PERIOD;
        if system_counted > most {
            return Err(format!(
                "system lane counted {system_counted} (filled {}, unfilled {}) above {most}: \
                 OVER x {} lost or idle before {} flagged packets, {skipped} skipped",
                filled[SYSTEM], unfilled[SYSTEM], system.flagged_gap, system.flags
            ));
        }
    }
    Ok(())
}

/// A failure's class: the lane (its message's first word) and the bound.
fn call_class(message: &str) -> String {
    let lane = message.split([' ', ':']).next().unwrap_or("");
    let kind = if message.contains("below") {
        "below"
    } else if message.contains("above") {
        "above"
    } else {
        "other"
    };
    format!("{lane} {kind}")
}

/// The smallest call found that still fails with the same invariant.
fn shrink_call(case: &CallCase, failure: &str) -> (CallCase, String) {
    let same = |message: &str| call_class(message) == call_class(failure);
    let mut best = (case.clone(), failure.to_owned());
    loop {
        let mut changed = false;
        let mut candidates: Vec<CallCase> = Vec::new();
        let c = &best.0;
        for i in 0..c.outages.len() {
            let mut d = c.clone();
            d.outages.remove(i);
            candidates.push(d);
        }
        for i in 0..c.idles.len() {
            let mut d = c.clone();
            d.idles.remove(i);
            candidates.push(d);
        }
        for i in 0..c.spurious.len() {
            let mut d = c.clone();
            d.spurious.remove(i);
            candidates.push(d);
        }
        for i in 0..c.timestamp_errors.len() {
            let mut d = c.clone();
            d.timestamp_errors.remove(i);
            candidates.push(d);
        }
        for field in 0..5 {
            let mut d = c.clone();
            match field {
                0 => d.system.still = 0,
                1 => d.system.qpc = Qpc::Absent,
                2 => d.mic.qpc = Qpc::Absent,
                3 => d.system_drift_ppm = 0,
                _ => d.partial_drains = 0,
            }
            candidates.push(d);
        }
        for candidate in candidates {
            if candidate == best.0 {
                continue;
            }
            if let Err(message) = run_call(&candidate)
                && same(&message)
            {
                best = (candidate, message);
                changed = true;
                break;
            }
        }
        if !changed {
            return best;
        }
    }
}

/// The call test: invariants 7 to 10 on every call.
#[test]
fn a_call_counts_both_lanes_losses_within_bounds_up_to_the_stop() {
    let (_, seed) = settings();
    let cases = std::env::var("WASAPI_CALL_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(CALL_CASES);
    let mut rng = Rng(seed ^ 0xCA11);
    let mut classes: Vec<(String, usize, String)> = Vec::new();
    let mut failed = 0;
    for _ in 0..cases {
        let case = generate_call(&mut rng);
        let Err(message) = run_call(&case) else {
            continue;
        };
        failed += 1;
        let key = call_class(&message);
        if let Some(class) = classes.iter_mut().find(|(k, _, _)| *k == key) {
            class.1 += 1;
            continue;
        }
        let (small, small_message) = shrink_call(&case, &message);
        classes.push((key, 1, format!("{small_message}\n    {small:?}")));
    }
    eprintln!(
        "call: seed {seed:#x}, {cases} cases, {failed} failed, {} classes",
        classes.len()
    );
    for (_, n, line) in &classes {
        eprintln!("  [{n} cases] {line}");
    }
    assert!(
        classes.is_empty(),
        "{} classes of failure:\n{}",
        classes.len(),
        classes
            .iter()
            .map(|(_, n, line)| format!("[{n} cases] {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Both threads late to their first drain by 0.6 s on engines that drop
/// their oldest packets: on a sane QPC each lane counts that half second.
#[test]
fn a_call_whose_threads_are_late_to_their_first_drain_counts_it() {
    let clocks = Clocks {
        engine_rate: Some(48_000),
        unit: Unit::Frames,
        still: 0,
        qpc: Qpc::Present,
    };
    for who in [Who::Both, Who::Mic, Who::System] {
        for idles in [Vec::new(), vec![(300, 20, false)]] {
            let case = CallCase {
                mic: clocks,
                system: clocks,
                keep: 10,
                drop_oldest: true,
                ticks: 600,
                outages: vec![(who, 0, 60)],
                idles,
                spurious: Vec::new(),
                timestamp_errors: Vec::new(),
                master_first: true,
                extra_packet: true,
                partial_drains: 0,
                system_drift_ppm: 0,
                start: 734_561_190_013,
            };
            if let Err(message) = run_call_with(&case, true) {
                panic!("{who:?}: {message}\n    {case:?}");
            }
        }
    }
}

// --- the multi-flag generator ---------------------------------------------

/// Which stream's events an episode makes.
fn storm_who(rng: &mut Rng) -> Who {
    match rng.below(100) {
        0..40 => Who::Mic,
        40..85 => Who::System,
        _ => Who::Both,
    }
}

/// An outage length around the lane's thresholds: within the engine's
/// buffer, just past it, past the target and the hold, long.
fn storm_len(rng: &mut Rng, keep: usize) -> usize {
    match rng.below(100) {
        0..15 => 1 + rng.below(keep as u64) as usize,
        15..40 => keep + 1 + rng.below(4) as usize,
        40..75 => keep + 5 + rng.below(30) as usize,
        75..92 => 40 + rng.below(110) as usize,
        _ => 150 + rng.below(250) as usize,
    }
}

/// A call made of N episodes (outages, spurious flags, idles, timestamp
/// errors), each placed relative to the one before, so flags arrive in
/// any order around outages and re-primes.
fn generate_storm(rng: &mut Rng) -> CallCase {
    let mut mic = generate_clocks(rng, true);
    let mut system = generate_clocks(rng, false);
    if rng.chance(500) {
        system.unit = Unit::Stuck;
        system.qpc = Qpc::Absent;
    }
    if rng.chance(300) {
        mic.qpc = Qpc::Absent;
    }
    let keep = rng.pick(&[10, 10, 12, 20]);
    let mut outages = Vec::new();
    let mut idles = Vec::new();
    let mut spurious = Vec::new();
    let mut timestamp_errors = Vec::new();
    if rng.chance(150) {
        // A thread late to its first drain (see `generate_call`).
        outages.push((storm_who(rng), 0, storm_len(rng, keep)));
    }
    let episodes = 1 + rng.below(8) as usize;
    let (mut from, mut to) = (20 + rng.below(80) as usize, 0usize);
    to = to.max(from);
    for _ in 0..episodes {
        let at = match rng.below(100) {
            // Inside the previous episode.
            0..30 => from + rng.below((to - from + 1) as u64) as usize,
            // At its end, give or take a few packets.
            30..65 => (to + rng.below(6) as usize).saturating_sub(2),
            // Within the target and the hold after it.
            65..85 => to + 2 + rng.below((keep + 4) as u64) as usize,
            _ => to + rng.below(200) as usize,
        };
        let end = match rng.below(100) {
            0..45 => {
                let len = storm_len(rng, keep);
                outages.push((storm_who(rng), at, len));
                at + len
            }
            45..75 => {
                let who = if rng.chance(800) {
                    Who::System
                } else {
                    storm_who(rng)
                };
                spurious.push((who, at));
                at
            }
            75..90 => {
                let len = 1 + rng.pick(&[1, 2, 3, 10, 30, 100]) as usize;
                idles.push((at, len, rng.chance(500)));
                at + len
            }
            _ => {
                timestamp_errors.push((storm_who(rng), at));
                at
            }
        };
        from = at;
        to = end.max(at);
    }
    let ticks = match rng.below(100) {
        // The stop inside or right after the last episode.
        0..35 => (from + 1).max(to.saturating_sub(rng.below(5) as usize)) + rng.below(4) as usize,
        35..60 => to + rng.below((keep + 6) as u64) as usize,
        _ => to + 20 + rng.below(300) as usize,
    }
    .max(from + 1)
    .max(30);
    CallCase {
        mic,
        system,
        keep,
        drop_oldest: rng.chance(500),
        ticks,
        outages,
        idles,
        spurious,
        timestamp_errors,
        master_first: rng.chance(500),
        extra_packet: !stop_without_a_flag() || rng.chance(500),
        partial_drains: if rng.chance(300) {
            rng.pick(&[100, 300, 600])
        } else {
            0
        },
        system_drift_ppm: if rng.chance(400) {
            rng.pick(&[-5_000, -1_000, -300, 300, 1_000, 5_000])
        } else {
            0
        },
        start: rng.below(1 << 40),
    }
}

/// `shrink_call`, then smaller numbers; every step only lowers a number
/// or drops a feature, so it ends.
fn shrink_storm(case: &CallCase, failure: &str) -> (CallCase, String) {
    let same = |message: &str| call_class(message) == call_class(failure);
    let mut best = shrink_call(case, failure);
    loop {
        let c = &best.0;
        let mut candidates: Vec<CallCase> = Vec::new();
        for cut in [c.ticks / 2, 50, 10, 3, 1] {
            if cut > 0 && c.ticks > cut + 10 {
                let mut d = c.clone();
                d.ticks -= cut;
                candidates.push(d);
            }
        }
        for i in 0..c.outages.len() {
            let (who, from, len) = c.outages[i];
            for (f, l) in [
                (from, len / 2),
                (from, len.saturating_sub(1)),
                (from.saturating_sub(10), len),
                (from.saturating_sub(1), len),
            ] {
                if l > 0 && (f, l) != (from, len) {
                    let mut d = c.clone();
                    d.outages[i] = (who, f, l);
                    candidates.push(d);
                }
            }
            if who == Who::Both {
                for w in [Who::Mic, Who::System] {
                    let mut d = c.clone();
                    d.outages[i].0 = w;
                    candidates.push(d);
                }
            }
        }
        for i in 0..c.idles.len() {
            let (from, len, flagged) = c.idles[i];
            for (f, l, g) in [
                (from, len / 2, flagged),
                (from, len.saturating_sub(1), flagged),
                (from.saturating_sub(1), len, flagged),
                (from, len, false),
            ] {
                if l > 0 && (f, l, g) != (from, len, flagged) {
                    let mut d = c.clone();
                    d.idles[i] = (f, l, g);
                    candidates.push(d);
                }
            }
        }
        for i in 0..c.spurious.len() {
            let (who, at) = c.spurious[i];
            for a in [at.saturating_sub(10), at.saturating_sub(1)] {
                if a != at {
                    let mut d = c.clone();
                    d.spurious[i] = (who, a);
                    candidates.push(d);
                }
            }
            if who == Who::Both {
                for w in [Who::Mic, Who::System] {
                    let mut d = c.clone();
                    d.spurious[i].0 = w;
                    candidates.push(d);
                }
            }
        }
        for field in 0..6 {
            let mut d = c.clone();
            match field {
                0 => d.mic.engine_rate = None,
                1 => d.system.engine_rate = None,
                2 => d.start = 0,
                3 => d.keep = d.keep.min(10),
                4 => d.mic.unit = Unit::Frames,
                _ => d.mic.qpc = Qpc::Absent,
            }
            candidates.push(d);
        }
        let mut changed = false;
        for candidate in candidates {
            if candidate == best.0 {
                continue;
            }
            if let Err(message) = run_call_with(&candidate, storm_upper())
                && same(&message)
            {
                best = (candidate, message);
                changed = true;
                break;
            }
        }
        if !changed {
            return best;
        }
    }
}

/// The shape of a shrunk call, to tell classes apart within one invariant.
fn storm_shape(case: &CallCase) -> String {
    let mut parts: Vec<(usize, String)> = Vec::new();
    for &(who, from, len) in &case.outages {
        parts.push((from, format!("out {who:?} {len}")));
    }
    for &(from, len, flagged) in &case.idles {
        parts.push((
            from,
            format!("idle {len}{}", if flagged { " flagged" } else { "" }),
        ));
    }
    for &(who, at) in &case.spurious {
        parts.push((at, format!("spurious {who:?}")));
    }
    for &(who, at) in &case.timestamp_errors {
        parts.push((at, format!("ts {who:?}")));
    }
    parts.sort();
    parts
        .into_iter()
        .map(|(_, p)| p)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether the multi-flag test checks the upper bounds too
/// (`WASAPI_STORM_UPPER=1`); by default it checks that no loss is silent.
fn storm_upper() -> bool {
    std::env::var("WASAPI_STORM_UPPER").is_ok_and(|v| v != "0")
}

/// Under 30 s in a debug build on CI together with the other property
/// tests; `WASAPI_STORM_CASES` overrides it.
const STORM_CASES: usize = 2_000;

/// Invariants 7 to 10 on every call [`generate_storm`] makes.
/// `WASAPI_STORM_SHAPES=n` prints every distinct shrunk shape.
#[test]
fn a_call_with_many_flags_counts_both_lanes_losses_within_bounds() {
    let (_, seed) = settings();
    let cases = std::env::var("WASAPI_STORM_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(STORM_CASES);
    let shapes: usize = std::env::var("WASAPI_STORM_SHAPES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut rng = Rng(seed ^ 0x5707);
    let mut classes: Vec<(String, usize, String)> = Vec::new();
    let mut failed = 0;
    let mut shrunk = 0;
    for index in 0..cases {
        let case = generate_storm(&mut rng);
        if let Ok(only) = std::env::var("WASAPI_STORM_ONLY") {
            if only.parse::<usize>().ok() != Some(index) {
                continue;
            }
            eprintln!("case {index}: {case:?}");
        }
        let Err(message) = run_call_with(&case, storm_upper()) else {
            continue;
        };
        failed += 1;
        if std::env::var_os("WASAPI_STORM_LIST").is_some() {
            eprintln!("fail {index} {}", call_class(&message));
        }
        let mut key = call_class(&message);
        if shapes > 0 {
            if shrunk >= shapes {
                continue;
            }
            shrunk += 1;
            let (small, small_message) = shrink_storm(&case, &message);
            key = format!("{} | {}", call_class(&small_message), storm_shape(&small));
            if let Some(class) = classes.iter_mut().find(|(k, _, _)| *k == key) {
                class.1 += 1;
                continue;
            }
            classes.push((key, 1, format!("{small_message}\n    {small:?}")));
            continue;
        }
        if let Some(class) = classes.iter_mut().find(|(k, _, _)| *k == key) {
            class.1 += 1;
            continue;
        }
        let (small, small_message) = shrink_storm(&case, &message);
        classes.push((key, 1, format!("{small_message}\n    {small:?}")));
    }
    eprintln!(
        "storm: seed {seed:#x}, {cases} cases, {failed} failed, {} classes",
        classes.len()
    );
    for (key, n, line) in &classes {
        eprintln!("  [{n} cases] {key}\n    {line}");
    }
    assert!(
        classes.is_empty(),
        "{} classes of failure:\n{}",
        classes.len(),
        classes
            .iter()
            .map(|(_, n, line)| format!("[{n} cases] {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Shrunk calls, each a shape of the padding rule that once lost a
/// system-lane loss silently or counted an idle as one.
#[test]
fn the_calls_the_generator_shrank_count_within_bounds() {
    let clocks = |unit, qpc| Clocks {
        engine_rate: None,
        unit,
        still: 0,
        qpc,
    };
    let call = |outages: Vec<(Who, usize, usize)>,
                idles: Vec<(usize, usize, bool)>,
                keep,
                ticks,
                master_first,
                extra_packet,
                start| CallCase {
        mic: clocks(Unit::Frames, Qpc::Absent),
        system: clocks(Unit::Stuck, Qpc::Absent),
        keep,
        drop_oldest: false,
        ticks,
        outages,
        idles,
        spurious: Vec::new(),
        timestamp_errors: Vec::new(),
        master_first,
        extra_packet,
        partial_drains: 0,
        system_drift_ppm: 0,
        start,
    };
    let idle_into_a_mic_outage = CallCase {
        mic: Clocks {
            engine_rate: Some(192_000),
            unit: Unit::Engine,
            still: 0,
            qpc: Qpc::Absent,
        },
        system: Clocks {
            engine_rate: Some(44_100),
            unit: Unit::Frames,
            still: 0,
            qpc: Qpc::Absent,
        },
        keep: 12,
        drop_oldest: false,
        ticks: 1_427,
        outages: vec![(Who::Mic, 1_284, 143), (Who::System, 1_357, 70)],
        idles: vec![(617, 691, false)],
        spurious: Vec::new(),
        timestamp_errors: Vec::new(),
        master_first: true,
        extra_packet: true,
        partial_drains: 0,
        system_drift_ppm: 0,
        start: 333_836_868_986,
    };
    let stale_flag_then_an_outage = CallCase {
        drop_oldest: true,
        spurious: vec![(Who::System, 14)],
        ..call(
            vec![(Who::Mic, 0, 136), (Who::System, 137, 15)],
            Vec::new(),
            10,
            153,
            false,
            true,
            0,
        )
    };
    let two_reprimes_in_a_burst = CallCase {
        partial_drains: 600,
        ..call(
            vec![(Who::System, 0, 80), (Who::Both, 80, 167)],
            Vec::new(),
            10,
            249,
            false,
            true,
            150_404_545_173,
        )
    };
    let a_long_outage_then_both_into_the_stop = CallCase {
        partial_drains: 100,
        ..call(
            vec![
                (Who::Both, 0, 40),
                (Who::System, 459, 347),
                (Who::Both, 807, 8),
                (Who::System, 806, 1),
            ],
            Vec::new(),
            12,
            816,
            true,
            true,
            585_657_934_205,
        )
    };
    let cases = [
        (
            call(
                vec![(Who::System, 59, 12)],
                vec![(23, 3, true), (27, 31, false)],
                10,
                73,
                false,
                true,
                824_264_437_451,
            ),
            false,
        ),
        (
            call(
                vec![(Who::System, 24, 25), (Who::System, 50, 10)],
                Vec::new(),
                10,
                60,
                true,
                true,
                0,
            ),
            false,
        ),
        (
            call(
                vec![(Who::System, 44, 8), (Who::System, 96, 22)],
                Vec::new(),
                10,
                120,
                false,
                false,
                569_416_241_567,
            ),
            false,
        ),
        (
            call(
                vec![
                    (Who::System, 54, 38),
                    (Who::Both, 92, 61),
                    (Who::System, 155, 33),
                ],
                Vec::new(),
                10,
                189,
                true,
                true,
                1_026_962_746_256,
            ),
            false,
        ),
        (
            call(
                vec![
                    (Who::System, 54, 40),
                    (Who::Both, 94, 274),
                    (Who::System, 371, 17),
                ],
                Vec::new(),
                10,
                390,
                false,
                false,
                359_292_229_702,
            ),
            false,
        ),
        (idle_into_a_mic_outage, true),
        (stale_flag_then_an_outage, false),
        (two_reprimes_in_a_burst, true),
        (a_long_outage_then_both_into_the_stop, true),
    ];
    for (index, (case, upper)) in cases.iter().enumerate() {
        if let Err(message) = run_call_with(case, *upper) {
            panic!("call {index}: {message}\n    {case:?}");
        }
    }
}

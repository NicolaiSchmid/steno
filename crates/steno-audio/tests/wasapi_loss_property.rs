//! One property test over WASAPI's loss counting (A18 of
//! `.plans/2026-10-07-stable-promotion.md`): `DevicePositions::packet` as
//! `CaptureClient::drain` feeds it (the first packet's flag aside), the
//! master's `PacketRouter` writing what it reports as `drain_into` does,
//! and the recorder's note over the result. Random packet streams mix
//! discontinuities, timestamp errors and silent packets, device positions
//! in units from 8 to 192 kHz against the 48 kHz stream and positions that
//! never move, a QPC that is present, absent, stuck, twice real time or
//! garbage, idle gaps of endpoint loopback, losses of random sizes (inside
//! the QPC's measuring window and after a rebuild among them). Invariants:
//!
//! 1. a flagged packet counts (filled plus unfilled) at least what was lost
//!    before it, less the clocks' resolution and the 2 % a sane QPC may be
//!    off by;
//! 2. the recorder's note (the gaps, the fills, and the larger of the
//!    unfilled count and the master's shortfall to wall time beyond the
//!    0.1 % drift allowance) counts at least everything lost;
//! 3. no zeros are written without a sane QPC;
//! 4. no packet fills more than was lost or idle before it;
//! 5. the sink's counts and the lane's length are what was reported.
//!
//! A loss nothing can read (positions that never move, or a unit neither
//! 48 kHz nor the mix format explains before its first clean pair, with no
//! readable QPC) is exempt from 1 and 2: the note's shortfall counts it
//! past the drift allowance only.
//!
//! The same streams also judge a reference counter that reads the QPC
//! alone (filled on a sane QPC, nothing otherwise), the shape that would
//! drop the device positions: it misses losses the positions read when
//! there is no sane QPC and they add up to less than the drift allowance,
//! which is why the positions stay in the count.
//!
//! Fixed seed, bounded case count; `WASAPI_PROPERTY_CASES` and
//! `WASAPI_PROPERTY_SEED` override them. A failure is shrunk to a minimal
//! case before it is reported.

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
/// the CI runner's process loopback reports them). With no sane QPC such a
/// stream cannot count a loss from its packets at all.
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
    /// The 48 kHz frames the stream asked for.
    Frames,
    /// The engine's frames at the mix format's rate (48 kHz without one).
    Engine,
    /// Frames at a rate neither explains.
    Rate(u64),
    /// Positions that never move.
    Stuck,
}

/// What `GetBuffer` gives for the QPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qpc {
    /// The time of the packet's first frame, a few microseconds of jitter.
    Present,
    /// Zero (not asked for, or not given).
    Absent,
    /// The same nonzero value on every packet.
    Stuck,
    /// Running at twice the time.
    Double,
    /// Garbage.
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A packet the engine delivers.
    Packet {
        frames: usize,
        silent: bool,
        timestamp_error: bool,
        /// A discontinuity flag with nothing lost.
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
    pub events: Vec<Event>,
}

impl Case {
    /// Positions a second.
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
        // A unit neither explains: real units are exact, so one within
        // 2 % of the 48 kHz frames or of the engine's would be a unit the
        // 1 % band admits and the clock then drifts against.
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
    // A quarter of the streams run longer and lose a few packets once or
    // twice, as a recording with one stall does: what they lose stays
    // under the recorder's drift allowance, so only a count reads it.
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

/// The code's `DevicePositions`, fed as `drain` feeds it.
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
/// without it, so the master's loss is left to the recorder's wall-time
/// shortfall. It is `DevicePositions` with the positions hidden.
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

/// Runs `case` through a counter of type `C` and the master's router on a
/// system-only capture: the first invariant broken, if any.
pub fn run<C: Counter>(case: &Case) -> Result<(), Failure> {
    run_with::<C>(case, true)
}

/// [`run`], with the router (`with_router`) or the counter alone (to shrink).
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
    // started: what a measured ratio and a matched QPC come from.
    let mut clean_pairs = 0usize;
    let mut jitter = Rng(case.start ^ 0xA5A5);
    let (mut delivered, mut written_fill, mut lane) = (0usize, 0usize, 0usize);
    let (mut reported_fill, mut reported_unfilled) = (0usize, 0usize);
    // The recording: from its first packet to the end of its last one.
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
                // The session's gap covers the time since the last frame,
                // written as silence and counted.
                if wall.is_some() {
                    total_lost += lost;
                    gap += lost + idle;
                }
                counter = C::new(case.engine_rate);
                (first_packet, flag, lost, idle) = (true, false, 0, 0);
                clean_pairs = 0;
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
                // A sane QPC may be off by 2 %; a measured ratio rounds
                // over the sums.
                let slack = resolution
                    + if fills {
                        pending / 50 + 1
                    } else {
                        pending / 1_000 + 1
                    };
                let counted = missed.fill.saturating_add(missed.unfilled);
                // Before the first clean pair of a client, a unit neither
                // the 48 kHz frames nor the mix format explains has no
                // ratio to be read at, and only a QPC can count the loss.
                let explained = match case.unit {
                    Unit::Frames => true,
                    Unit::Engine => case.engine_rate.is_some(),
                    Unit::Rate(_) | Unit::Stuck => false,
                };
                // Positions that never move (the CI runner's process
                // loopback reports them) leave only the QPC, read from a
                // clean pair before, which a timestamp error on this packet
                // or the one before rules out.
                let qpc_readable = case.qpc == Qpc::Present
                    && clean_pairs > 0
                    && !timestamp_error
                    && !previous_timestamp_error;
                let unreadable =
                    !qpc_readable && (case.unit == Unit::Stuck || !explained && clean_pairs == 0);
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
                }
                (flag, lost, idle) = (false, 0, 0);
                previous_timestamp_error = timestamp_error;
            }
        }
    }
    // The recorder's note: the gaps and the fills, and the larger of the
    // unfilled count and the master's shortfall to wall time beyond the
    // drift allowance.
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
    // The router writes what was reported, and nothing else.
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
    // A bounded number of runs, so a shrink always ends.
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
    // Cut everything after the failing event.
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
        // Simplify each event.
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
        // Simplify the stream.
        for (unit, qpc, rate, start) in [
            (Unit::Frames, best.qpc, best.engine_rate, best.start),
            (best.unit, Qpc::Absent, best.engine_rate, best.start),
            (best.unit, best.qpc, Some(48_000), best.start),
            (best.unit, best.qpc, best.engine_rate, 0),
        ] {
            let candidate = Case {
                unit,
                qpc,
                engine_rate: rate,
                start,
                period: best.period,
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
        "engine {:?}, unit {:?}, qpc {:?}, period {}, start {}: ",
        case.engine_rate, case.unit, case.qpc, case.period, case.start
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

/// Every case, shrinking one failure of each class: the classes found.
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

/// The CI test: the code keeps every invariant on every stream (a loss
/// nothing can read is exempt from invariants 1 and 2 only, see
/// `run_with`).
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

/// The structural question, judged on the same streams: the QPC alone,
/// with the recorder's wall-time shortfall behind it, would do without the
/// device positions only if it kept every invariant. It does not: on
/// streams without a sane QPC, or in its first two seconds while it is
/// measured, the shortfall leaves the drift allowance
/// (0.1 % of the wall time, 3.6 s in an hour) out of the note, so a
/// recording whose losses add up to less than that shows none of them.
/// Every one of its failures is that, and the code, which keeps the
/// positions in its count, counts each of those streams in full.
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

/// The frames from a case's first packet to the end of its last.
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

//! Window layout over a long recording: FluidAudio's `ChunkProcessor`
//! (0.17.4) for v3 with `melChunkContext = false`, the path Steno's
//! `ParakeetEngine` takes through `AsrManager.transcribe`.
//!
//! "Chunk" is FluidAudio's name for the region a start begins; "window"
//! is the samples the model sees for it, which differ only by the warm-up
//! prefix.
//!
//! Everything here is arithmetic over the samples; no model is involved,
//! so it builds and is tested on every platform.

/// Audio sample rate the models expect (`ASRConstants.sampleRate`).
pub const SAMPLE_RATE: usize = 16_000;
/// Samples per encoder frame: 160-sample mel hop times 8x subsampling
/// (`ASRConstants.samplesPerEncoderFrame`), 80 ms.
pub const FRAME_SAMPLES: usize = 1280;
/// One encoder frame in seconds (`ASRConstants.secondsPerEncoderFrame`).
pub const FRAME_SECONDS: f64 = 0.08;
/// The encoder's input window, 15 s (`ASRConstants.maxModelSamples`).
pub const MAX_MODEL_SAMPLES: usize = 240_000;
/// Mel hop in samples (`ASRConstants.melHopSize`).
pub const MEL_HOP: usize = 160;
/// Overlap between consecutive windows (`ChunkProcessor.overlapSeconds`).
pub const OVERLAP_SECONDS: f64 = 2.0;
/// Overlap in encoder frames, 25 (`ASRConstants.standardOverlapFrames`).
pub const OVERLAP_FRAMES: usize = 25;
/// Below the quietest real speech, about -66 dBFS
/// (`ChunkProcessor.speechRmsFloor`).
pub const SPEECH_RMS_FLOOR: f32 = 0.0005;
/// The pre-adaptive fixed speech gate, about -42 dBFS
/// (`ChunkProcessor.speechRmsCeiling`).
pub const SPEECH_RMS_CEILING: f32 = 0.008;
/// About -10.5 dB under the speech reference
/// (`ChunkProcessor.speechRmsReferenceScale`).
pub const SPEECH_RMS_REFERENCE_SCALE: f32 = 0.3;
/// Reference percentile over non-digital-silence frames
/// (`ChunkProcessor.speechRmsReferencePercentile`).
pub const SPEECH_RMS_REFERENCE_PERCENTILE: f64 = 0.75;

/// Frames of a sample count, rounded up (`ASRConstants.calculateEncoderFrames`).
#[must_use]
pub fn encoder_frames(samples: usize) -> usize {
    samples.div_ceil(FRAME_SAMPLES)
}

/// Seconds of a frame index. Exact: frame counts stay far below 2^53.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn frame_seconds(frame: usize) -> f64 {
    frame as f64 * FRAME_SECONDS
}

/// Seconds of a sample index. Exact for the same reason.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn sample_seconds(sample: usize) -> f64 {
    sample as f64 / SAMPLE_RATE as f64
}

/// The sizes of one window and the step between windows, all frame aligned.
/// `ChunkProcessor.chunkLayout` with no mel context reserved: the window is
/// 239,360 samples (14.96 s), the overlap 32,000 (2.0 s), the stride
/// 207,360 (12.96 s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Samples one window spans.
    pub chunk_samples: usize,
    /// Samples consecutive windows share.
    pub overlap_samples: usize,
    /// Samples between consecutive regular starts.
    pub stride_samples: usize,
}

impl Layout {
    /// The v3 layout for `melChunkContext = false`.
    #[must_use]
    pub fn v3() -> Layout {
        let raw = (MAX_MODEL_SAMPLES - MEL_HOP).max(FRAME_SAMPLES);
        let chunk_samples = raw / FRAME_SAMPLES * FRAME_SAMPLES;
        // Rounding 2.0 s at 16 kHz is exact.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let requested = (OVERLAP_SECONDS * SAMPLE_RATE as f64) as usize;
        let overlap_samples = requested.min(chunk_samples / 2) / FRAME_SAMPLES * FRAME_SAMPLES;
        let stride_samples =
            (chunk_samples - overlap_samples).max(FRAME_SAMPLES) / FRAME_SAMPLES * FRAME_SAMPLES;
        Layout {
            chunk_samples,
            overlap_samples,
            stride_samples,
        }
    }
}

/// Where a window starts and whether it decodes a suppressed warm-up prefix
/// before it (`ChunkProcessor.ChunkStartDecision`). The prefix is never
/// used on the v3 default path (`noMelWarmupPrefixFrames = 0`) but the
/// decision carries the flag so the port stays one for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkStart {
    /// First sample of the chunk.
    pub start: usize,
    /// Decode a suppressed warm-up prefix before `start`.
    pub use_warmup_prefix: bool,
}

impl ChunkStart {
    /// A start without a warm-up prefix.
    fn plain(start: usize) -> ChunkStart {
        ChunkStart {
            start,
            use_warmup_prefix: false,
        }
    }
}

/// Mean of squares over `samples`; zero for an empty slice.
fn mean_square(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    // Slice lengths are far below 2^24 per call here.
    #[allow(clippy::cast_precision_loss)]
    let count = samples.len() as f32;
    sum / count
}

/// RMS over `samples`; zero for an empty slice.
fn rms(samples: &[f32]) -> f32 {
    mean_square(samples).sqrt()
}

/// Last speech-bearing sample: the length minus the trailing run of frames
/// whose RMS sits below [`SPEECH_RMS_FLOOR`]. The whole length for an
/// all-quiet file (`ChunkProcessor.speechEndSamples`).
#[must_use]
pub fn speech_end_samples(audio: &[f32]) -> usize {
    let mut end = audio.len();
    while end > 0 {
        let frame_start = end.saturating_sub(FRAME_SAMPLES);
        if rms(&audio[frame_start..end]) >= SPEECH_RMS_FLOOR {
            return end;
        }
        end = frame_start;
    }
    audio.len()
}

/// End-align the final window (FluidAudio issue #747): a short last chunk
/// fills its window backwards with real audio, decoded as a suppressed
/// prefix, so it ends at `speech_end` instead of being zero padded.
/// Non-final chunks, full windows and single-chunk files return
/// `default_warmup` unchanged (`ChunkProcessor.lastChunkWarmupSamples`).
#[must_use]
pub fn last_chunk_warmup_samples(
    chunk_start: usize,
    default_warmup: usize,
    chunk_samples: usize,
    total_samples: usize,
    speech_end: usize,
) -> usize {
    let default_visible = chunk_samples
        .saturating_sub(default_warmup)
        .max(FRAME_SAMPLES);
    let is_last = chunk_start + default_visible >= total_samples;
    let remaining = speech_end.min(total_samples).saturating_sub(chunk_start);
    if !is_last || remaining == 0 || chunk_start == 0 {
        return default_warmup;
    }
    // Frame aligned so the suppression boundary maps to an exact frame.
    let fill = chunk_samples.saturating_sub(remaining) / FRAME_SAMPLES * FRAME_SAMPLES;
    if fill == 0 {
        return default_warmup;
    }
    let available = chunk_start / FRAME_SAMPLES * FRAME_SAMPLES;
    default_warmup.max(available.min(fill))
}

/// Regular starts every `stride` (`ChunkProcessor.regularChunkStarts`); the
/// path FluidAudio takes for non-v3 models, kept for tests and comparison.
#[must_use]
pub fn regular_chunk_starts(total_samples: usize, stride_samples: usize) -> Vec<ChunkStart> {
    std::iter::once(0)
        .chain((stride_samples..total_samples).step_by(stride_samples))
        .map(ChunkStart::plain)
        .collect()
}

/// A boundary candidate: the quietest frame near the target, its energy and
/// the median energy of the frames searched.
#[derive(Debug, Clone, Copy)]
struct BoundaryCandidate {
    start: usize,
    score: f32,
    median_score: f32,
}

/// The chunk starts the v3 path uses: each boundary moves to the quietest
/// frame within 4 s of its regular position when that frame is near
/// silence (5 % of the median energy), else to a 0.5 s valley (35 %), else
/// stays; never earlier than one frame after the previous start and never
/// later than the previous window's end minus six frames of overlap
/// (`ChunkProcessor.silenceAlignedChunkStarts`).
///
/// `can_use_warmup_prefix` is `warmupPrefixSamples > 0`, false on the v3
/// default path, so the warm-up and compress-tail rules below never fire
/// there; they are ported so the switch is one flag.
#[must_use]
pub fn silence_aligned_chunk_starts(
    audio: &[f32],
    layout: Layout,
    can_use_warmup_prefix: bool,
) -> Vec<ChunkStart> {
    let total = audio.len();
    let chunk = layout.chunk_samples;
    let stride = layout.stride_samples;
    // 4.0 s and 0.5 s in frames: 50 and 6 at 16 kHz.
    let silence_radius = ((4 * SAMPLE_RATE) / FRAME_SAMPLES).max(1);
    let valley_radius = ((SAMPLE_RATE / 2) / FRAME_SAMPLES).max(1);
    let half_energy_window = FRAME_SAMPLES;
    let minimum_overlap = FRAME_SAMPLES * 6;

    let mut starts = vec![ChunkStart::plain(0)];
    let mut previous_start = 0usize;
    let mut target = stride;

    while target < total {
        let target_frame = target / FRAME_SAMPLES;
        let latest_covered_start = previous_start + chunk - minimum_overlap;
        let target_start = (target_frame * FRAME_SAMPLES)
            .max(previous_start + FRAME_SAMPLES)
            .min(latest_covered_start);

        let silence = best_boundary_candidate(
            audio,
            target_frame,
            silence_radius,
            previous_start,
            latest_covered_start,
            half_energy_window,
        );
        let mut use_warmup_prefix = false;
        let mut best_start = if is_near_silence_boundary(silence) {
            let should_warmup =
                can_use_warmup_prefix && should_use_warmup_prefix(audio, silence.start);
            let compresses_tail = should_warmup
                && silence.start < target_start
                && would_compress_speech_tail(
                    audio,
                    silence.start,
                    target_start,
                    chunk,
                    minimum_overlap,
                    silence.median_score,
                    half_energy_window,
                );
            if compresses_tail {
                target_start
            } else {
                use_warmup_prefix = should_warmup;
                silence.start
            }
        } else {
            let valley = best_boundary_candidate(
                audio,
                target_frame,
                valley_radius,
                previous_start,
                latest_covered_start,
                half_energy_window,
            );
            if is_usable_valley_boundary(valley) {
                valley.start
            } else {
                target_start
            }
        };

        if best_start <= previous_start {
            best_start = (previous_start + stride).min(total);
        }
        starts.push(ChunkStart {
            start: best_start,
            use_warmup_prefix,
        });
        previous_start = best_start;
        target += stride;
    }
    starts
}

/// The quietest frame-aligned boundary within `radius` frames of
/// `target_frame`, inside `(previous_start, latest_covered_start]`
/// (`ChunkProcessor.bestBoundaryCandidate`).
fn best_boundary_candidate(
    audio: &[f32],
    target_frame: usize,
    radius: usize,
    previous_start: usize,
    latest_covered_start: usize,
    half_window: usize,
) -> BoundaryCandidate {
    let total = audio.len();
    let lower_frame = target_frame.saturating_sub(radius).max(1);
    let upper_frame = ((total.saturating_sub(1)) / FRAME_SAMPLES).min(target_frame + radius);
    let target_start = (target_frame * FRAME_SAMPLES)
        .max(previous_start + FRAME_SAMPLES)
        .min(latest_covered_start);

    let mut best_start = target_start;
    let mut best_score = f32::MAX;
    let mut scores: Vec<f32> = Vec::new();
    // An inverted range (target near the start) is simply empty.
    for frame in lower_frame..=upper_frame {
        let candidate = frame * FRAME_SAMPLES;
        if candidate <= previous_start || candidate > latest_covered_start {
            continue;
        }
        let score = boundary_energy_score(audio, candidate, half_window);
        scores.push(score);
        if score < best_score {
            best_score = score;
            best_start = candidate;
        }
    }
    if scores.is_empty() {
        return BoundaryCandidate {
            start: target_start,
            score: f32::MAX,
            median_score: 0.0,
        };
    }
    scores.sort_by(f32::total_cmp);
    BoundaryCandidate {
        start: best_start,
        score: best_score,
        median_score: scores[scores.len() / 2],
    }
}

fn adaptive_boundary_threshold(median_score: f32, ratio: f32) -> f32 {
    if median_score > 0.0 {
        median_score * ratio
    } else {
        0.0
    }
}

fn is_near_silence_boundary(candidate: BoundaryCandidate) -> bool {
    candidate.score <= adaptive_boundary_threshold(candidate.median_score, 0.05)
}

fn is_usable_valley_boundary(candidate: BoundaryCandidate) -> bool {
    candidate.score <= adaptive_boundary_threshold(candidate.median_score, 0.35)
}

/// Whether moving the boundary earlier to `candidate_start` would force
/// the next boundary into speech on both ends
/// (`ChunkProcessor.wouldCompressSpeechTail`).
fn would_compress_speech_tail(
    audio: &[f32],
    candidate_start: usize,
    target_start: usize,
    chunk_samples: usize,
    minimum_overlap: usize,
    median_score: f32,
    half_window: usize,
) -> bool {
    if median_score <= 0.0 {
        return false;
    }
    let forced_next_boundary = candidate_start + chunk_samples - minimum_overlap;
    if forced_next_boundary >= audio.len() {
        return false;
    }
    let speech_like = median_score * 0.8;
    let target_score = boundary_energy_score(audio, target_start, half_window);
    let forced_score = boundary_energy_score(audio, forced_next_boundary, half_window);
    target_score > speech_like && forced_score > speech_like
}

/// Whether the 0.5 s after `center` holds no stable 0.2 s of quiet (20 ms
/// windows under RMS 0.003): then a warm-up prefix is worth decoding
/// (`ChunkProcessor.shouldUseWarmupPrefix`).
fn should_use_warmup_prefix(audio: &[f32], center: usize) -> bool {
    let total = audio.len();
    let lookahead = SAMPLE_RATE / 2;
    let minimum_stable_quiet = SAMPLE_RATE / 5;
    let window = (SAMPLE_RATE / 50).max(1);
    let quiet_rms = 0.003f32;
    let mut offset = 0usize;
    let mut quiet = 0usize;
    while offset < lookahead {
        let start = center + offset;
        if start >= total {
            break;
        }
        let count = window.min(total - start).min(lookahead - offset);
        if count == 0 {
            break;
        }
        if rms(&audio[start..start + count]) >= quiet_rms {
            break;
        }
        quiet += count;
        if quiet >= minimum_stable_quiet {
            return false;
        }
        offset += count;
    }
    true
}

/// Mean energy of the samples within `half_window` of `center`
/// (`ChunkProcessor.boundaryEnergyScore`).
fn boundary_energy_score(audio: &[f32], center: usize, half_window: usize) -> f32 {
    let start = center.saturating_sub(half_window);
    let end = (center + half_window).min(audio.len());
    if end <= start {
        return 0.0;
    }
    mean_square(&audio[start..end])
}

/// One window as the pipeline decodes it: the samples `[context_start,
/// audio_end)`, the frame before which emitted tokens are suppressed, and
/// the offset the decoder adds to its frame indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Position in the plan, the merge order.
    pub index: usize,
    /// First sample fed to the model (the chunk start minus any warm-up).
    pub context_start: usize,
    /// One past the last sample fed to the model.
    pub audio_end: usize,
    /// The sample the decoder's frame zero refers to
    /// (`chunkStartOffset`): `context_start` when a warm-up prefix is
    /// decoded, else the chunk start. Without mel context the two
    /// coincide, so this always equals `context_start`.
    pub frame_origin: usize,
    /// Tokens before this global frame are decoded but suppressed
    /// (`emitTokensAfterFrame`).
    pub emit_after_frame: Option<usize>,
    /// Run the end-of-audio flush (`isLastChunk`).
    pub is_last: bool,
}

impl Window {
    /// The window's sample count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.audio_end - self.context_start
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The windows `ChunkProcessor.process` decodes for `starts` over
/// `total_samples` of audio ending in speech at `speech_end`. Mirrors the
/// loop one step at a time: the warm-up is the end-aligned fill for the
/// last chunk, the window is `chunk - warmup` wide, the last window stops
/// at `speech_end`, and once `starts` runs out the stride continues.
#[must_use]
pub fn plan_windows(
    total_samples: usize,
    speech_end: usize,
    starts: &[ChunkStart],
    layout: Layout,
    warmup_prefix_samples: usize,
) -> Vec<Window> {
    let mut windows = Vec::new();
    let mut decision = starts.first().copied().unwrap_or(ChunkStart::plain(0));
    let mut index = 0usize;
    while decision.start < total_samples {
        let chunk_start = decision.start;
        let default_warmup = if index > 0 && decision.use_warmup_prefix {
            warmup_prefix_samples.min(chunk_start)
        } else {
            0
        };
        let warmup = last_chunk_warmup_samples(
            chunk_start,
            default_warmup,
            layout.chunk_samples,
            total_samples,
            speech_end,
        );
        let visible = layout
            .chunk_samples
            .saturating_sub(warmup)
            .max(FRAME_SAMPLES);
        let candidate_end = chunk_start + visible;
        let is_last = candidate_end >= total_samples;
        // The last window stops at the file end, or earlier at the end of
        // speech; a non-final window is at least one frame long.
        let audio_end = if is_last {
            total_samples.min(speech_end)
        } else {
            candidate_end
        };
        if audio_end <= chunk_start {
            break;
        }
        let context_start = chunk_start - warmup;
        windows.push(Window {
            index,
            context_start,
            audio_end,
            frame_origin: context_start,
            emit_after_frame: (warmup > 0).then_some(chunk_start / FRAME_SAMPLES),
            is_last,
        });
        index += 1;
        if is_last {
            break;
        }
        decision = starts
            .get(index)
            .copied()
            .unwrap_or_else(|| ChunkStart::plain(chunk_start + layout.stride_samples));
    }
    windows
}

/// Speech-energy threshold scaled to the recording's own level: the 75th
/// percentile of per-frame RMS over non-silent frames, times 0.3, clamped
/// to `[floor, ceiling]` (`ChunkProcessor.adaptiveSpeechRmsThreshold`).
/// All-zero frames are excluded so digital silence does not drag the
/// percentile to the floor.
#[must_use]
pub fn adaptive_speech_rms_threshold(audio: &[f32]) -> f32 {
    let mut frame_rms: Vec<f32> = audio
        .as_chunks::<FRAME_SAMPLES>()
        .0
        .iter()
        .map(|frame| mean_square(frame))
        .filter(|sum| *sum > 0.0)
        .map(f32::sqrt)
        .collect();
    if frame_rms.is_empty() {
        return SPEECH_RMS_CEILING;
    }
    frame_rms.sort_by(f32::total_cmp);
    // The percentile index is exact: counts stay far below 2^53.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let reference_index = ((frame_rms.len() as f64 * SPEECH_RMS_REFERENCE_PERCENTILE) as usize)
        .min(frame_rms.len() - 1);
    let reference = frame_rms[reference_index];
    (reference * SPEECH_RMS_REFERENCE_SCALE).clamp(SPEECH_RMS_FLOOR, SPEECH_RMS_CEILING)
}

/// Seconds of frames in `[start, end)` whose RMS exceeds `threshold`
/// (`ChunkProcessor.speechLikeSeconds`).
#[must_use]
pub fn speech_like_seconds(audio: &[f32], start: usize, end: usize, threshold: f32) -> f64 {
    let end = end.min(audio.len());
    if start >= end {
        return 0.0;
    }
    let frames = audio[start..end]
        .as_chunks::<FRAME_SAMPLES>()
        .0
        .iter()
        .filter(|frame| rms(frame.as_slice()) > threshold)
        .count();
    frame_seconds(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(seconds: f64, amplitude: f32) -> Vec<f32> {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let count = (seconds * SAMPLE_RATE as f64) as usize;
        (0..count)
            .map(|i| if i % 2 == 0 { amplitude } else { -amplitude })
            .collect()
    }

    #[test]
    fn v3_layout_is_the_spike_and_fluidaudio_layout() {
        let layout = Layout::v3();
        assert_eq!(layout.chunk_samples, 239_360);
        assert_eq!(layout.overlap_samples, 32_000);
        assert_eq!(layout.stride_samples, 207_360);
        assert_eq!(encoder_frames(239_360), 187);
        assert_eq!(encoder_frames(239_361), 188);
    }

    #[test]
    fn speech_end_skips_trailing_quiet_frames() {
        let mut audio = tone(3.0, 0.1);
        audio.extend(std::iter::repeat_n(0.0, 2 * FRAME_SAMPLES));
        audio.extend(std::iter::repeat_n(0.0001, FRAME_SAMPLES));
        assert_eq!(speech_end_samples(&audio), 48_000);
        let quiet = vec![0.0f32; 4 * FRAME_SAMPLES];
        assert_eq!(speech_end_samples(&quiet), quiet.len());
        assert_eq!(speech_end_samples(&[]), 0);
    }

    #[test]
    fn last_chunk_warmup_fills_a_short_final_window_backwards() {
        let layout = Layout::v3();
        let total = layout.stride_samples + 5 * SAMPLE_RATE; // 12.96 s + 5 s
        let warmup =
            last_chunk_warmup_samples(layout.stride_samples, 0, layout.chunk_samples, total, total);
        // 14.96 s window minus 5 s remaining = 9.96 s of fill, frame aligned.
        assert_eq!(
            warmup,
            (layout.chunk_samples - 5 * SAMPLE_RATE) / FRAME_SAMPLES * FRAME_SAMPLES
        );
        // Not the last chunk: untouched.
        assert_eq!(
            last_chunk_warmup_samples(0, 0, layout.chunk_samples, total, total),
            0
        );
        // Speech ends before the chunk start: untouched.
        assert_eq!(
            last_chunk_warmup_samples(layout.stride_samples, 0, layout.chunk_samples, total, 1000),
            0
        );
        // A full final window needs no fill.
        let exact = layout.stride_samples + layout.chunk_samples;
        assert_eq!(
            last_chunk_warmup_samples(layout.stride_samples, 0, layout.chunk_samples, exact, exact),
            0
        );
    }

    #[test]
    fn regular_starts_step_by_the_stride() {
        let starts = regular_chunk_starts(500_000, 207_360);
        assert_eq!(
            starts.iter().map(|s| s.start).collect::<Vec<_>>(),
            vec![0, 207_360, 414_720]
        );
        assert!(starts.iter().all(|s| !s.use_warmup_prefix));
    }

    #[test]
    fn silence_aligned_starts_move_to_the_quiet_frame() {
        let layout = Layout::v3();
        // 30 s of speech-like signal with one silent second at 11.0..12.0 s,
        // inside the 4 s search radius of the 12.96 s target.
        let mut audio = tone(30.0, 0.1);
        for sample in &mut audio[11 * SAMPLE_RATE..12 * SAMPLE_RATE] {
            *sample = 0.0;
        }
        let starts = silence_aligned_chunk_starts(&audio, layout, false);
        assert_eq!(starts[0].start, 0);
        let second = starts[1].start;
        assert_eq!(second % FRAME_SAMPLES, 0);
        assert!(
            (11 * SAMPLE_RATE..=12 * SAMPLE_RATE).contains(&second),
            "{second}"
        );
        assert!(!starts[1].use_warmup_prefix);
        // Uniform energy: no silence and no valley, the target stands.
        let flat = tone(30.0, 0.1);
        let flat_starts = silence_aligned_chunk_starts(&flat, layout, false);
        assert_eq!(flat_starts[1].start, layout.stride_samples);
        assert_eq!(flat_starts.len(), 3);
    }

    #[test]
    fn a_valley_is_taken_when_no_silence_is_near() {
        let layout = Layout::v3();
        let mut audio = tone(30.0, 0.1);
        // A dip to 30 % amplitude (9 % energy) just after the target, within
        // the 0.5 s valley radius but far too loud for the 5 % silence rule.
        let dip_start = layout.stride_samples + 2 * FRAME_SAMPLES;
        for sample in &mut audio[dip_start..dip_start + 2 * FRAME_SAMPLES] {
            *sample *= 0.3;
        }
        let starts = silence_aligned_chunk_starts(&audio, layout, false);
        assert!(
            starts[1].start > layout.stride_samples
                && starts[1].start <= dip_start + 2 * FRAME_SAMPLES,
            "{}",
            starts[1].start
        );
    }

    #[test]
    fn warmup_prefix_rules_are_inert_unless_enabled() {
        let layout = Layout::v3();
        let mut audio = tone(30.0, 0.1);
        for sample in &mut audio[11 * SAMPLE_RATE..12 * SAMPLE_RATE] {
            *sample = 0.0;
        }
        let without = silence_aligned_chunk_starts(&audio, layout, false);
        let with = silence_aligned_chunk_starts(&audio, layout, true);
        assert_eq!(without[1].start, with[1].start);
        assert!(!without[1].use_warmup_prefix);
        // The silent second runs on after the boundary: stable quiet, no
        // warm-up wanted.
        assert!(!with[1].use_warmup_prefix);
        // Speech right after the boundary asks for the prefix.
        assert!(should_use_warmup_prefix(&tone(2.0, 0.1), SAMPLE_RATE));
        assert!(!should_use_warmup_prefix(
            &vec![0.0; 2 * SAMPLE_RATE],
            SAMPLE_RATE
        ));
    }

    #[test]
    fn an_earlier_silence_that_would_compress_the_tail_keeps_the_target() {
        // Two silent frames at 10.0 s, speech on both sides of the regular
        // 12.96 s boundary and of the boundary a start at 10.08 s would
        // force: without the prefix the start moves to the silence; with
        // it the compress-tail rule keeps the regular start, no warm-up.
        let layout = Layout::v3();
        let mut audio = tone(30.0, 0.1);
        for sample in &mut audio[125 * FRAME_SAMPLES..127 * FRAME_SAMPLES] {
            *sample = 0.0;
        }
        let without = silence_aligned_chunk_starts(&audio, layout, false);
        assert_eq!(without[1].start, 126 * FRAME_SAMPLES);
        let with = silence_aligned_chunk_starts(&audio, layout, true);
        assert_eq!(with[1].start, layout.stride_samples);
        assert!(!with[1].use_warmup_prefix);
    }

    #[test]
    fn a_valley_above_the_near_silence_ratio_never_asks_for_a_warmup() {
        // Two frames at 9 % of the median energy just before the regular
        // boundary: a usable valley (35 %) but not near silence (5 %), so
        // the start moves there and the warm-up rules stay out of it.
        let layout = Layout::v3();
        let mut audio = tone(30.0, 0.1);
        for sample in &mut audio[160 * FRAME_SAMPLES..162 * FRAME_SAMPLES] {
            *sample *= 0.3;
        }
        let with = silence_aligned_chunk_starts(&audio, layout, true);
        assert_eq!(with[1].start, 161 * FRAME_SAMPLES);
        assert!(!with[1].use_warmup_prefix);
    }

    #[test]
    fn compress_tail_rule_needs_speech_at_both_forced_boundaries() {
        let layout = Layout::v3();
        let audio = tone(40.0, 0.1);
        let median = 0.01;
        assert!(would_compress_speech_tail(
            &audio,
            10 * SAMPLE_RATE,
            12 * SAMPLE_RATE,
            layout.chunk_samples,
            6 * FRAME_SAMPLES,
            median,
            FRAME_SAMPLES
        ));
        let mut quiet_target = audio.clone();
        for sample in
            &mut quiet_target[12 * SAMPLE_RATE - FRAME_SAMPLES..12 * SAMPLE_RATE + FRAME_SAMPLES]
        {
            *sample = 0.0;
        }
        assert!(!would_compress_speech_tail(
            &quiet_target,
            10 * SAMPLE_RATE,
            12 * SAMPLE_RATE,
            layout.chunk_samples,
            6 * FRAME_SAMPLES,
            median,
            FRAME_SAMPLES
        ));
        // Forced boundary past the end: nothing to compress.
        assert!(!would_compress_speech_tail(
            &tone(20.0, 0.1),
            10 * SAMPLE_RATE,
            12 * SAMPLE_RATE,
            layout.chunk_samples,
            6 * FRAME_SAMPLES,
            median,
            FRAME_SAMPLES
        ));
    }

    #[test]
    fn windows_follow_the_starts_then_the_stride_and_end_align_the_last() {
        let layout = Layout::v3();
        let total = 40 * SAMPLE_RATE;
        let starts = regular_chunk_starts(total, layout.stride_samples);
        assert_eq!(starts.len(), 4);
        let windows = plan_windows(total, total, &starts, layout, 0);
        // The third window (start 25.92 s) reaches past 40 s, so it is the
        // last one and the fourth start is never used.
        assert_eq!(windows.len(), 3);
        assert_eq!(
            (windows[0].context_start, windows[0].audio_end),
            (0, layout.chunk_samples)
        );
        assert_eq!(windows[0].emit_after_frame, None);
        assert!(!windows[0].is_last);
        assert_eq!(windows[1].context_start, layout.stride_samples);
        let last = windows[2];
        assert!(last.is_last);
        assert_eq!(last.audio_end, total);
        // 14.08 s remain after the chunk start, so 0.88 s (11 frames) of
        // real audio fill the window backwards and are suppressed.
        let chunk_start = 2 * layout.stride_samples;
        let fill = 11 * FRAME_SAMPLES;
        assert_eq!(last.context_start, chunk_start - fill);
        assert_eq!(last.frame_origin, last.context_start);
        assert_eq!(last.emit_after_frame, Some(chunk_start / FRAME_SAMPLES));
        assert_eq!(last.len(), layout.chunk_samples);
        // Speech ending a second early: the end-aligned fill shrinks the
        // third window so it stops short of the file end and is not the
        // last one; the fourth start then decodes (almost) the same span
        // with everything before 38.88 s suppressed, and no window carries
        // `is_last`. The Swift loop does exactly this.
        let trimmed = plan_windows(total, total - SAMPLE_RATE, &starts, layout, 0);
        assert_eq!(trimmed.len(), 4);
        assert!(trimmed.iter().all(|window| !window.is_last));
        assert_eq!(trimmed[2].context_start, chunk_start - 23 * FRAME_SAMPLES);
        assert_eq!(
            trimmed[2].audio_end,
            chunk_start + layout.chunk_samples - 23 * FRAME_SAMPLES
        );
        assert_eq!(
            trimmed[3].emit_after_frame,
            Some(3 * layout.stride_samples / FRAME_SAMPLES)
        );
        assert_eq!(trimmed[3].audio_end, trimmed[2].audio_end);
        // Starts exhausted early: the stride continues.
        let few = plan_windows(total, total, &starts[..2], layout, 0);
        assert_eq!(few.len(), 3);
        assert_eq!(few[2].context_start, chunk_start - fill);
        // A silence-aligned start moves the window with it.
        let moved = [
            ChunkStart {
                start: 0,
                use_warmup_prefix: false,
            },
            ChunkStart {
                start: 11 * SAMPLE_RATE,
                use_warmup_prefix: false,
            },
        ];
        let windows = plan_windows(total, total, &moved, layout, 0);
        assert_eq!(windows[1].context_start, 11 * SAMPLE_RATE);
        assert_eq!(
            windows[1].audio_end,
            11 * SAMPLE_RATE + layout.chunk_samples
        );
        // Nothing to decode: no windows.
        assert_eq!(plan_windows(0, 0, &[], layout, 0), Vec::new());
    }

    #[test]
    fn adaptive_threshold_scales_with_the_recording_and_clamps() {
        assert_eq!(adaptive_speech_rms_threshold(&[]), SPEECH_RMS_CEILING);
        assert_eq!(
            adaptive_speech_rms_threshold(&vec![0.0; 10 * FRAME_SAMPLES]),
            SPEECH_RMS_CEILING
        );
        // Square wave at 0.01: RMS 0.01, times 0.3 = 0.003.
        let quiet = tone(2.0, 0.01);
        assert!((adaptive_speech_rms_threshold(&quiet) - 0.003).abs() < 1e-6);
        // Loud: clamped to the ceiling.
        assert_eq!(
            adaptive_speech_rms_threshold(&tone(2.0, 0.5)),
            SPEECH_RMS_CEILING
        );
        // Nearly inaudible: clamped to the floor.
        assert_eq!(
            adaptive_speech_rms_threshold(&tone(2.0, 0.0001)),
            SPEECH_RMS_FLOOR
        );
    }

    #[test]
    fn speech_like_seconds_counts_frames_above_the_gate() {
        let mut audio = tone(1.0, 0.1);
        audio.extend(std::iter::repeat_n(0.0, SAMPLE_RATE));
        let seconds = speech_like_seconds(&audio, 0, audio.len(), 0.01);
        // 12 full frames of the first second (16000 / 1280 = 12.5) plus the
        // straddling 13th, half tone, whose RMS still clears the gate.
        assert!((seconds - frame_seconds(13)).abs() < 1e-9);
        assert_eq!(
            speech_like_seconds(&audio, SAMPLE_RATE, audio.len(), 0.01),
            0.0
        );
        assert_eq!(speech_like_seconds(&audio, 10, 5, 0.01), 0.0);
    }
}

//! Port of FluidAudio's TdtDecoderV3 greedy loop (language filter and
//! top-K paths omitted: Steno passes `language: nil`).

use crate::coreml::{output, provider, Array, Features, Model};

pub const BLANK: usize = 8192;
pub const DURATION_BINS: [usize; 5] = [0, 1, 2, 3, 4];
pub const MAX_SYMBOLS_PER_STEP: usize = 10;
pub const MAX_TOKENS_PER_CHUNK: usize = 150;
pub const CONSECUTIVE_BLANK_LIMIT: usize = 5;
pub const ENC_HIDDEN: usize = 1024;
pub const DEC_HIDDEN: usize = 640;
pub const DEC_LAYERS: usize = 2;

#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub id: usize,
    /// Global encoder frame index (80 ms) at emission.
    pub frame: usize,
    pub conf: f32,
    pub dur: usize,
}

/// Reusable buffers; one set per decoding thread.
pub struct DecoderBuffers {
    targets: Array,
    target_len: Array,
    h: Array,
    c: Array,
    enc_step: Array,
    dec_step: Array,
    /// Cached decoder projection for the last fed token ("predictorOutput").
    proj: Vec<f32>,
}

impl DecoderBuffers {
    pub fn new() -> Result<Self, String> {
        let target_len = Array::zeros_i32(&[1])?;
        target_len.i32_mut()[0] = 1;
        Ok(Self {
            targets: Array::zeros_i32(&[1, 1])?,
            target_len,
            h: Array::zeros_f32(&[DEC_LAYERS, 1, DEC_HIDDEN])?,
            c: Array::zeros_f32(&[DEC_LAYERS, 1, DEC_HIDDEN])?,
            enc_step: Array::zeros_f32(&[1, ENC_HIDDEN, 1])?,
            dec_step: Array::zeros_f32(&[1, DEC_HIDDEN, 1])?,
            proj: vec![0.0; DEC_HIDDEN],
        })
    }

    fn reset(&mut self) {
        self.h.f32_mut().fill(0.0);
        self.c.f32_mut().fill(0.0);
        self.proj.fill(0.0);
    }

    /// Run the LSTM decoder on `token`, update h/c in place, cache projection.
    fn run_decoder(&mut self, dec: &Model) -> Result<(), String> {
        let input = provider(&[
            ("targets", &self.targets),
            ("target_length", &self.target_len),
            ("h_in", &self.h),
            ("c_in", &self.c),
        ])?;
        let out = dec.predict(&input)?;
        copy_dense(&output(&out, "decoder")?, &mut self.proj)?;
        copy_dense(&output(&out, "h_out")?, self.h.f32_mut())?;
        copy_dense(&output(&out, "c_out")?, self.c.f32_mut())?;
        Ok(())
    }

    fn feed(&mut self, dec: &Model, token: usize) -> Result<(), String> {
        self.targets.i32_mut()[0] = token as i32;
        self.run_decoder(dec)
    }

    fn joint(&mut self, joint: &Model, enc: &EncoderView, t: usize) -> Result<(usize, f32, usize), String> {
        enc.copy_frame(t, self.enc_step.f32_mut());
        self.dec_step.f32_mut().copy_from_slice(&self.proj);
        let input = provider(&[("encoder_step", &self.enc_step), ("decoder_step", &self.dec_step)])?;
        let out = joint.predict(&input)?;
        let id = output(&out, "token_id")?.i32_mut()[0] as usize;
        let prob = output(&out, "token_prob")?.f32_mut()[0];
        let bin = output(&out, "duration")?.i32_mut()[0] as usize;
        Ok((id, prob, bin))
    }
}

fn copy_dense(src: &Array, dst: &mut [f32]) -> Result<(), String> {
    if src.count() != dst.len() {
        return Err(format!("shape mismatch {:?} vs {}", src.shape, dst.len()));
    }
    if !src.is_contiguous() {
        return Err(format!("non-contiguous array {:?} strides {:?}", src.shape, src.strides));
    }
    dst.copy_from_slice(unsafe { std::slice::from_raw_parts(src.ptr_f32(), dst.len()) });
    Ok(())
}

/// Strided view of the encoder output [1, 1024, T].
pub struct EncoderView {
    arr: Array,
    pub valid: usize,
    hidden_stride: usize,
    time_stride: usize,
}

impl EncoderView {
    pub fn new(arr: Array, valid: usize) -> Result<Self, String> {
        if arr.shape.len() != 3 || arr.shape[1] != ENC_HIDDEN {
            return Err(format!("unexpected encoder shape {:?}", arr.shape));
        }
        let valid = valid.min(arr.shape[2]);
        Ok(Self { hidden_stride: arr.strides[1], time_stride: arr.strides[2], arr, valid })
    }

    fn copy_frame(&self, t: usize, dst: &mut [f32]) {
        let base = self.arr.ptr_f32();
        for h in 0..ENC_HIDDEN {
            dst[h] = unsafe { *base.add(h * self.hidden_stride + t * self.time_stride) };
        }
    }
}

fn clamp_prob(p: f32) -> f32 {
    if !p.is_finite() {
        0.0
    } else {
        p.clamp(0.0, 1.0)
    }
}

fn map_duration(bin: usize) -> Result<usize, String> {
    DURATION_BINS.get(bin).copied().ok_or_else(|| format!("duration bin out of range: {bin}"))
}

pub struct DecodeStats {
    pub decoder_calls: usize,
    pub joint_calls: usize,
    pub recoveries_tried: usize,
    pub recoveries_accepted: usize,
}

/// Greedy TDT decode of one window with a fresh decoder state (ChunkProcessor
/// creates a new `TdtDecoderState` per chunk).
pub fn decode_window(
    bufs: &mut DecoderBuffers,
    dec: &Model,
    joint: &Model,
    enc: &EncoderView,
    actual_audio_frames: usize,
    global_frame_offset: usize,
    emit_after_global_frame: Option<usize>,
    is_last: bool,
    stats: &mut DecodeStats,
) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    if enc.valid <= 1 {
        return Ok(tokens);
    }
    let eff_len = enc.valid.min(actual_audio_frames);
    let mut t: usize = 0; // initialTimeIndex: timeJump nil, contextFrameAdjustment 0
    if t >= eff_len {
        return Ok(tokens);
    }
    let last_ts = eff_len - 1;
    let mut safe_t = t.min(last_ts);
    let mut active = t < eff_len;

    bufs.reset();
    bufs.feed(dec, BLANK)?; // SOS priming
    stats.decoder_calls += 1;

    let should_emit = |frame: usize| emit_after_global_frame.map_or(true, |e| frame >= e);

    let mut last_token: Option<usize> = None;
    let mut last_emission_ts: isize = -1;
    let mut emissions_at_ts = 0usize;
    let mut processed = 0usize;
    let mut t_label;

    while active {
        let (mut label, prob, bin) = bufs.joint(joint, enc, safe_t)?;
        stats.joint_calls += 1;
        let mut score = clamp_prob(prob);
        let mut dur = map_duration(bin)?;
        let mut blank = label == BLANK;
        let cur_t = t as isize;
        if !blank && dur == 0 && cur_t == last_emission_ts && emissions_at_ts >= 1 {
            dur = 1;
        }
        if blank && dur == 0 {
            dur = 1;
        }
        t_label = t;
        t += dur;
        safe_t = t.min(last_ts);
        active = t < eff_len;
        let mut advance = active && blank;
        while advance {
            t_label = t;
            let (l, p, b) = bufs.joint(joint, enc, safe_t)?;
            stats.joint_calls += 1;
            label = l;
            score = clamp_prob(p);
            dur = map_duration(b)?;
            blank = label == BLANK;
            if blank && dur == 0 {
                dur = 1;
            }
            t += dur;
            safe_t = t.min(last_ts);
            active = t < eff_len;
            advance = active && blank;
        }
        if active && label != BLANK {
            processed += 1;
            if processed > MAX_TOKENS_PER_CHUNK {
                break;
            }
            let frame = t_label + global_frame_offset;
            if should_emit(frame) {
                tokens.push(Token { id: label, frame, conf: score, dur });
            }
            last_token = Some(label);
            bufs.feed(dec, label)?;
            stats.decoder_calls += 1;
            if t_label as isize == last_emission_ts {
                emissions_at_ts += 1;
            } else {
                last_emission_ts = t_label as isize;
                emissions_at_ts = 1;
            }
            if emissions_at_ts >= MAX_SYMBOLS_PER_STEP {
                t = (t + 1).min(last_ts);
                safe_t = t.min(last_ts);
                emissions_at_ts = 0;
                last_emission_ts = -1;
            }
        }
        active = t < eff_len;
    }

    if is_last {
        let mut additional = 0usize;
        let mut consecutive_blanks = 0usize;
        let mut fpt = t;
        let count = enc.valid;
        while additional < MAX_SYMBOLS_PER_STEP && consecutive_blanks < CONSECUTIVE_BLANK_LIMIT {
            let variations = [
                fpt.min(count - 1),
                (eff_len - 1).min(count - 1),
                eff_len.saturating_sub(2).min(count - 1),
            ];
            let frame_index = variations[additional % 3];
            let (label, prob, bin) = bufs.joint(joint, enc, frame_index)?;
            stats.joint_calls += 1;
            let score = clamp_prob(prob);
            let dur = map_duration(bin)?;
            if label == BLANK {
                consecutive_blanks += 1;
            } else {
                consecutive_blanks = 0;
                let frame = fpt.min(eff_len - 1) + global_frame_offset;
                if should_emit(frame) {
                    tokens.push(Token { id: label, frame, conf: score, dur });
                }
                last_token = Some(label);
                bufs.feed(dec, label)?;
                stats.decoder_calls += 1;
            }
            fpt = (fpt + dur.max(1)).min(eff_len);
            additional += 1;
        }
    }
    let _ = last_token;
    Ok(tokens)
}

pub fn encoder_view_from(features: &Features) -> Result<EncoderView, String> {
    let enc = output(features, "encoder")?;
    let len = output(features, "encoder_length")?.i32_mut()[0] as usize;
    EncoderView::new(enc, len)
}

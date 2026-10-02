//! RMS, peak, Goertzel tone level, onset and ERLE, the same definitions as
//! `Sources/StenoAudio/AEC/EchoMetrics.swift` and `DevCaptureSpike.swift`.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
}

/// dBFS of a linear magnitude; -160 for silence.
pub fn decibels(linear: f32) -> f32 {
    if linear <= 0.0 {
        -160.0
    } else {
        (20.0 * linear.log10()).max(-160.0)
    }
}

pub fn tone_level(samples: &[f32], frequency: f64, sample_rate: f64) -> f32 {
    let count = samples.len();
    if count == 0 {
        return 0.0;
    }
    let omega = 2.0 * std::f64::consts::PI * frequency / sample_rate;
    let coefficient = 2.0 * omega.cos();
    let (mut previous, mut before_previous) = (0.0f64, 0.0f64);
    for &sample in samples {
        let current = sample as f64 + coefficient * previous - before_previous;
        before_previous = previous;
        previous = current;
    }
    let real = previous - before_previous * omega.cos();
    let imaginary = before_previous * omega.sin();
    ((real * real + imaginary * imaginary).sqrt() * 2.0 / count as f64) as f32
}

/// Quietest one-second window RMS (linear).
pub fn idle_floor(samples: &[f32], sample_rate: usize) -> f32 {
    let mut quietest = f32::MAX;
    let mut start = 0;
    while start + sample_rate <= samples.len() {
        quietest = quietest.min(rms(&samples[start..start + sample_rate]));
        start += sample_rate;
    }
    if quietest == f32::MAX {
        0.0
    } else {
        quietest
    }
}

/// First 480-sample frame above `threshold_db`, in seconds.
pub fn onset(samples: &[f32], threshold_db: f32, sample_rate: usize) -> Option<f64> {
    let mut frame = 0;
    while frame + 480 <= samples.len() {
        if decibels(rms(&samples[frame..frame + 480])) > threshold_db {
            return Some(frame as f64 / sample_rate as f64);
        }
        frame += 480;
    }
    None
}

/// Echo return loss enhancement over `range` in dB; positive is better.
pub fn erle(near_end: &[f32], processed: &[f32], range: std::ops::Range<usize>) -> f32 {
    let end = range.end.min(near_end.len()).min(processed.len());
    let start = range.start.min(end);
    let before = rms(&near_end[start..end]);
    let after = rms(&processed[start..end]);
    if before <= 0.0 {
        return 0.0;
    }
    decibels(before) - decibels(after)
}

/// Lag (in samples, may be negative) of `b` relative to `a` maximising the
/// normalised cross-correlation of the two envelopes over `max_lag`.
/// Envelopes are 1 ms RMS blocks, so the resolution is 1 ms.
pub fn envelope_lag_ms(a: &[f32], b: &[f32], sample_rate: usize, max_lag_ms: usize) -> Option<i64> {
    let block = sample_rate / 1000;
    let env = |x: &[f32]| -> Vec<f32> {
        x.chunks_exact(block).map(rms).collect()
    };
    let ea = env(a);
    let eb = env(b);
    let n = ea.len().min(eb.len());
    if n < 2 * max_lag_ms + 10 {
        return None;
    }
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
    let (ma, mb) = (mean(&ea[..n]), mean(&eb[..n]));
    let mut best = (f32::MIN, 0i64);
    for lag in -(max_lag_ms as i64)..=(max_lag_ms as i64) {
        let mut num = 0.0f64;
        let mut da = 0.0f64;
        let mut db = 0.0f64;
        for i in 0..n {
            let j = i as i64 + lag;
            if j < 0 || j >= n as i64 {
                continue;
            }
            let x = (ea[i] - ma) as f64;
            let y = (eb[j as usize] - mb) as f64;
            num += x * y;
            da += x * x;
            db += y * y;
        }
        let corr = if da > 0.0 && db > 0.0 { (num / (da * db).sqrt()) as f32 } else { 0.0 };
        if corr > best.0 {
            best = (corr, lag);
        }
    }
    Some(best.1)
}

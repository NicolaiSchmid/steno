//! The synthetic echo fixtures of `Sources/StenoAudio/Testing/AudioFixtures.swift`
//! (same seeds, same SplitMix64) so `--synthetic` is comparable with
//! `steno dev aec-bench --synthetic`: six seconds of speech-like far-end
//! through a seeded sparse room at 60 ms, echo gain 0.5, -60 dBFS floor.
const RATE: f64 = 48_000.0;

pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

pub fn noise(seconds: f64, seed: u64, amplitude: f64) -> Vec<f32> {
    let mut g = SplitMix64(seed);
    (0..(seconds * RATE) as usize).map(|_| (amplitude * (g.unit() * 2.0 - 1.0)) as f32).collect()
}

pub fn speech_like_far(seconds: f64) -> Vec<f32> {
    let raw = noise(seconds, 0x5EED_0048, 1.0);
    let tilt: f32 = 0.7;
    let syllable = RATE / 4.0;
    let mut state: f32 = 0.0;
    let mut samples = vec![0.0f32; raw.len()];
    for (index, sample) in samples.iter_mut().enumerate() {
        state = tilt * state + (1.0 - tilt) * raw[index];
        let position = (index as f64 % syllable) / syllable;
        let syllable_index = (index as f64 / syllable) as usize;
        let envelope = if syllable_index % 4 == 3 { 0.0 } else { 0.5 * (1.0 - (2.0 * std::f64::consts::PI * position).cos()) };
        *sample = envelope as f32 * state;
    }
    let loudest = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if loudest > 0.0 {
        let scale = 0.7f32 / loudest;
        for s in &mut samples {
            *s *= scale;
        }
    }
    samples
}

pub fn room_impulse_response() -> Vec<f32> {
    let count = (0.1 * RATE) as usize;
    let mut g = SplitMix64(0x1200_0000);
    let mut response = vec![0.0f32; count];
    response[0] = 1.0;
    let decay = -3.0 * 10f64.ln() / count as f64;
    for _ in 0..48 {
        let position = 1 + (g.next() % (count as u64 - 1)) as usize;
        let unit = g.unit();
        response[position] += (0.1 * (decay * position as f64).exp() * (unit * 2.0 - 1.0)) as f32;
    }
    response
}

pub fn convolve(signal: &[f32], impulse_response: &[f32], delay: usize) -> Vec<f32> {
    let mut output = vec![0.0f32; signal.len()];
    for (tap_index, &tap) in impulse_response.iter().enumerate().filter(|(_, t)| **t != 0.0) {
        let shift = tap_index + delay;
        if shift >= signal.len() {
            continue;
        }
        for n in shift..signal.len() {
            output[n] += signal[n - shift] * tap;
        }
    }
    output
}

pub fn echo_mic(far: &[f32], impulse_response: &[f32]) -> Vec<f32> {
    let delayed = convolve(far, impulse_response, (0.060 * RATE).round() as usize);
    let floor = noise(far.len() as f64 / RATE, 0x0A0B_0C0D, 0.001);
    delayed.iter().zip(floor.iter()).map(|(d, f)| 0.5 * d + f).collect()
}

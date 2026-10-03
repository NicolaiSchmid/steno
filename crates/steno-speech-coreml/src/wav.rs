//! A RIFF/WAVE reader for the calibration corpus: 16 kHz mono PCM, 16-bit
//! integer or 32-bit float. Enough for the harness; the app decodes
//! recordings through `AudioDecoder`.

use std::path::Path;

use crate::SpeechError;
use crate::chunking::SAMPLE_RATE;

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    bytes
        .get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// The samples of a 16 kHz mono WAV file as `f32` in `[-1, 1]`.
pub fn read_mono_16k(path: &Path) -> Result<Vec<f32>, SpeechError> {
    let bytes = std::fs::read(path).map_err(|e| SpeechError::Wav {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    parse_mono_16k(&bytes).map_err(|message| SpeechError::Wav {
        path: path.to_path_buf(),
        message,
    })
}

/// [`read_mono_16k`] over bytes already in memory.
pub fn parse_mono_16k(bytes: &[u8]) -> Result<Vec<f32>, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".to_owned());
    }
    let mut pos = 12usize;
    let mut format: Option<(u16, u16, u32, u16)> = None;
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32_at(bytes, pos + 4).ok_or("truncated chunk header")? as usize;
        let body_start = pos + 8;
        let body_end = body_start.saturating_add(size).min(bytes.len());
        match id {
            b"fmt " => {
                let tag = u16_at(bytes, body_start).ok_or("truncated fmt chunk")?;
                let channels = u16_at(bytes, body_start + 2).ok_or("truncated fmt chunk")?;
                let rate = u32_at(bytes, body_start + 4).ok_or("truncated fmt chunk")?;
                let bits = u16_at(bytes, body_start + 14).ok_or("truncated fmt chunk")?;
                format = Some((tag, channels, rate, bits));
            }
            b"data" => data = Some(&bytes[body_start..body_end]),
            _ => {}
        }
        pos = body_start.saturating_add(size).saturating_add(size & 1);
    }
    let (tag, channels, rate, bits) = format.ok_or("no fmt chunk")?;
    let data = data.ok_or("no data chunk")?;
    if channels != 1 || rate as usize != SAMPLE_RATE {
        return Err(format!("want 16 kHz mono, got {rate} Hz {channels} ch"));
    }
    match (tag, bits) {
        (1, 16) => Ok(data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| f32::from(i16::from_le_bytes(*c)) / 32768.0)
            .collect()),
        (3, 32) => Ok(data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect()),
        _ => Err(format!(
            "want 16-bit PCM or 32-bit float, got format {tag} with {bits} bits"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(tag: u16, bits: u16, rate: u32, channels: u16, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * u32::from(channels) * u32::from(bits) / 8).to_le_bytes());
        out.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"LIST");
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&[1, 2, 3, 0]); // odd chunk, padded
        out.extend_from_slice(b"data");
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn reads_pcm16_and_float32() {
        let pcm = wav(1, 16, 16_000, 1, &[0x00, 0x40, 0x00, 0xC0]);
        assert_eq!(parse_mono_16k(&pcm).unwrap(), vec![0.5, -0.5]);
        let mut floats = Vec::new();
        floats.extend_from_slice(&0.25f32.to_le_bytes());
        floats.extend_from_slice(&(-1.0f32).to_le_bytes());
        let f32s = wav(3, 32, 16_000, 1, &floats);
        assert_eq!(parse_mono_16k(&f32s).unwrap(), vec![0.25, -1.0]);
    }

    #[test]
    fn rejects_other_layouts() {
        assert!(parse_mono_16k(b"RIFX").is_err());
        assert!(parse_mono_16k(&wav(1, 16, 44_100, 1, &[0, 0])).is_err());
        assert!(parse_mono_16k(&wav(1, 16, 16_000, 2, &[0, 0, 0, 0])).is_err());
        assert!(parse_mono_16k(&wav(1, 8, 16_000, 1, &[0])).is_err());
    }
}

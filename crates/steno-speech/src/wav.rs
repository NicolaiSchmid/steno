//! The crate's one WAV reader: 16 kHz PCM-16, channel 0 of a
//! multi-channel file, which is what the FLEURS clips, the calibration
//! corpus and the Swift writer's per-lane 16 kHz files (`<lane>.wav`) are.
//! Promoted from the model-gated tests so the `transcribe` example, the
//! FLEURS gate and the speech sidecar's parity test read audio the same
//! way.
//! Swift: `WAVAudioDecoder` in `Sources/StenoCore/Audio/WAVAudioDecoder.swift`,
//! which also reads 32-bit float.

use std::path::Path;

use crate::backend::SAMPLE_RATE;
use crate::error::SpeechError;

/// Reads a 16 kHz PCM-16 WAV into `f32` samples in `[-1, 1)`.
pub fn read_pcm16(path: &Path) -> Result<Vec<f32>, SpeechError> {
    let bytes = std::fs::read(path).map_err(|e| SpeechError::io(path, e))?;
    decode_pcm16(&bytes).map_err(|detail| SpeechError::Wav {
        path: path.to_path_buf(),
        detail,
    })
}

/// [`read_pcm16`] over bytes in memory.
fn decode_pcm16(bytes: &[u8]) -> Result<Vec<f32>, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF WAVE file".to_owned());
    }
    let u16_at = |b: &[u8], at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
    let u32_at = |b: &[u8], at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    let mut format = None;
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32_at(bytes, offset + 4) as usize;
        let start = offset + 8;
        let body = &bytes[start..start.saturating_add(size).min(bytes.len())];
        if id == b"fmt " {
            if body.len() < 16 {
                return Err("fmt chunk too short".to_owned());
            }
            format = Some((u16_at(body, 2), u32_at(body, 4), u16_at(body, 14)));
        } else if id == b"data" {
            let (channels, rate, bits) = format.ok_or("data chunk before fmt chunk")?;
            if usize::try_from(rate).ok() != Some(SAMPLE_RATE) {
                return Err(format!("{rate} Hz, the pipeline takes 16 kHz"));
            }
            if bits != 16 || channels == 0 {
                return Err(format!("{bits}-bit, {channels} channels; PCM-16 only"));
            }
            return Ok(body
                .chunks_exact(2 * usize::from(channels))
                .map(|frame| f32::from(i16::from_le_bytes([frame[0], frame[1]])) / 32_768.0)
                .collect());
        }
        offset = start + size + (size & 1);
    }
    Err("no data chunk".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(rate: u32, channels: u16, bits: u16, samples: &[i16]) -> Vec<u8> {
        let mut out = b"RIFF\0\0\0\0WAVE".to_vec();
        // An odd-sized chunk ahead of fmt exercises the padding byte.
        out.extend_from_slice(b"LIST\x03\0\0\0abc\0");
        out.extend_from_slice(b"fmt \x10\0\0\0\x01\0");
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2 * u32::from(channels)).to_le_bytes());
        out.extend_from_slice(&(2 * channels).to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&((samples.len() * 2) as u32).to_le_bytes());
        for sample in samples {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out
    }

    #[test]
    fn pcm16_is_scaled_and_a_stereo_file_gives_its_left_channel() {
        assert_eq!(
            decode_pcm16(&wav(16_000, 1, 16, &[16_384, -32_768, 0])).unwrap(),
            [0.5, -1.0, 0.0]
        );
        assert_eq!(
            decode_pcm16(&wav(16_000, 2, 16, &[16_384, 1, -16_384, 2])).unwrap(),
            [0.5, -0.5]
        );
    }

    #[test]
    fn other_rates_depths_and_garbage_are_errors() {
        assert!(
            decode_pcm16(&wav(44_100, 1, 16, &[]))
                .unwrap_err()
                .contains("44100 Hz")
        );
        assert!(
            decode_pcm16(&wav(16_000, 1, 24, &[]))
                .unwrap_err()
                .contains("PCM-16")
        );
        assert!(decode_pcm16(b"not a wav").unwrap_err().contains("RIFF"));
        assert!(
            decode_pcm16(&wav(16_000, 1, 16, &[])[..48])
                .unwrap_err()
                .contains("no data")
        );
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            read_pcm16(&dir.path().join("missing.wav")),
            Err(SpeechError::Io { .. })
        ));
        let path = dir.path().join("bad.wav");
        std::fs::write(&path, b"RIFF....WAVE").unwrap();
        assert!(matches!(read_pcm16(&path), Err(SpeechError::Wav { .. })));
    }
}

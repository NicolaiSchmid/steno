//! Tiny RIFF/WAVE reader for 16 kHz mono 16-bit PCM.

pub fn read_wav_mono_i16(path: &str) -> Result<Vec<f32>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(format!("{path}: not a RIFF/WAVE file"));
    }
    let mut pos = 12;
    let mut channels = 1u16;
    let mut bits = 16u16;
    let mut rate = 0u32;
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body_start = pos + 8;
        let body_end = (body_start + size).min(bytes.len());
        match id {
            b"fmt " => {
                channels = u16::from_le_bytes(bytes[body_start + 2..body_start + 4].try_into().unwrap());
                rate = u32::from_le_bytes(bytes[body_start + 4..body_start + 8].try_into().unwrap());
                bits = u16::from_le_bytes(bytes[body_start + 14..body_start + 16].try_into().unwrap());
            }
            b"data" => data = Some(&bytes[body_start..body_end]),
            _ => {}
        }
        pos = body_start + size + (size & 1);
    }
    if channels != 1 || bits != 16 || rate != 16_000 {
        return Err(format!("{path}: want 16 kHz mono 16-bit, got {rate} Hz {channels} ch {bits} bit"));
    }
    let data = data.ok_or_else(|| format!("{path}: no data chunk"))?;
    Ok(data
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect())
}

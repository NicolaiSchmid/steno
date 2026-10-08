//! The encoder priming an MP4 (m4a) file declares for its audio track,
//! which symphonia 0.5 parses and does not apply. AVFoundation dropped it,
//! so the decode starts on the first sample the encoder was given.
//! Swift: `AVAudioFile`, which applies it (no code of Steno's).
//!
//! Two places hold it:
//!
//! - the track's edit list (`moov/trak/edts/elst`): the media time of its
//!   first edit that is not empty, in the track's timescale (`mdhd`). ffmpeg writes it (1 024
//!   samples for its AAC encoder), as do other ISO writers;
//! - iTunes' gapless tag (`moov/udta/meta/ilst/----`, named `iTunSMPB`):
//!   its second field, in samples. Apple's own writer stores the priming
//!   (2 112 samples) only there; the AVFoundation mixdowns of the Swift app
//!   carry no edit list.
//!
//! The edit list wins when a file has both. Only the boxes on the way to
//! those two are read, by seeking; the sample tables are skipped. An empty
//! edit (media time -1, a delay before the track) and an edit list that
//! starts later than [`MAX_PRIMING_SECONDS`] are not applied: the first
//! is silence nobody recorded (the next edit's media time is the
//! priming), the second would cut audio an editor kept; either way
//! nothing of the recording is lost. The padding after the last sample
//! stays: under a packet of silence.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The longest start the decoder trims: about four times Apple's priming
/// at 44.1 kHz. A longer edit is an edit, not priming.
pub const MAX_PRIMING_SECONDS: f64 = 0.2;

/// Where the priming is counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priming {
    /// The edit list's media time, in ticks of the track's `timescale`.
    EditList {
        /// Media time of the first edit.
        media_time: u64,
        /// The track's ticks per second.
        timescale: u32,
    },
    /// iTunes' gapless tag, in samples at the track's rate.
    Gapless {
        /// Samples to drop.
        samples: u64,
    },
}

impl Priming {
    /// The frames at `rate` hertz to drop, `None` past
    /// [`MAX_PRIMING_SECONDS`] or for a zero rate or timescale.
    #[must_use]
    pub fn frames(self, rate: u32) -> Option<u64> {
        let frames = match self {
            Self::EditList {
                media_time,
                timescale,
            } => {
                if timescale == 0 {
                    return None;
                }
                // Rounded to the nearest frame; exact when the timescale is
                // the rate, as every writer seen makes it.
                let scaled = u128::from(media_time) * u128::from(rate);
                let timescale = u128::from(timescale);
                u64::try_from((scaled + timescale / 2) / timescale).ok()?
            }
            Self::Gapless { samples } => samples,
        };
        // The bound is a fraction of a second, so the cast is exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let limit = (MAX_PRIMING_SECONDS * f64::from(rate)) as u64;
        (rate > 0 && frames <= limit).then_some(frames)
    }

    /// The priming the MP4 file at `path` declares for its first audio
    /// track, `None` when it declares none, is not an MP4 file, or cannot
    /// be read (the decode then starts where symphonia starts it).
    #[must_use]
    pub fn read(path: &Path) -> Option<Self> {
        let mut file = File::open(path).ok()?;
        let end = file.metadata().ok()?.len();
        let mut boxes = Boxes::new(&mut file, 0, end);
        let first = boxes.next()?;
        if &first.kind != b"ftyp" {
            return None;
        }
        let moov = std::iter::from_fn(|| boxes.next()).find(|b| &b.kind == b"moov")?;
        let mut audio_seen = false;
        let mut edit = None;
        let mut gapless = None;
        for child in children(&mut file, moov) {
            match &child.kind {
                b"trak" if !audio_seen => {
                    if let Some(mdia) = sound_media(&mut file, child) {
                        audio_seen = true;
                        edit = edit_list_priming(&mut file, child, mdia);
                    }
                }
                b"udta" if gapless.is_none() => gapless = gapless_priming(&mut file, child),
                _ => {}
            }
        }
        edit.or(gapless)
    }
}

/// One box: its type and where its body lies in the file.
#[derive(Debug, Clone, Copy)]
struct Mp4Box {
    kind: [u8; 4],
    /// The body's first byte.
    start: u64,
    /// One past the body's last byte.
    end: u64,
}

/// The boxes between two offsets, in order; a box that runs past `end` or
/// cannot be read ends the walk.
struct Boxes<'a> {
    file: &'a mut File,
    at: u64,
    end: u64,
}

impl<'a> Boxes<'a> {
    fn new(file: &'a mut File, start: u64, end: u64) -> Self {
        Self {
            file,
            at: start,
            end,
        }
    }

    fn next(&mut self) -> Option<Mp4Box> {
        if self.at.checked_add(8)? > self.end {
            return None;
        }
        let mut header = [0u8; 8];
        read_at(self.file, self.at, &mut header)?;
        let size = u64::from(u32::from_be_bytes(header[..4].try_into().ok()?));
        let kind: [u8; 4] = header[4..].try_into().ok()?;
        let (body, size) = match size {
            // To the end of the enclosing box (or file).
            0 => (self.at + 8, self.end - self.at),
            // A 64-bit size follows the type.
            1 => {
                let mut large = [0u8; 8];
                read_at(self.file, self.at + 8, &mut large)?;
                (self.at + 16, u64::from_be_bytes(large))
            }
            size => (self.at + 8, size),
        };
        let end = self.at.checked_add(size)?;
        if end > self.end || body > end {
            return None;
        }
        self.at = end;
        Some(Mp4Box {
            kind,
            start: body,
            end,
        })
    }
}

/// The boxes directly inside `parent`, from `skip` bytes into its body.
fn children_from(file: &mut File, parent: Mp4Box, skip: u64) -> Vec<Mp4Box> {
    let mut boxes = Boxes::new(file, parent.start.saturating_add(skip), parent.end);
    // A box holds few children, and a corrupt one is cut off at 64.
    std::iter::from_fn(|| boxes.next()).take(64).collect()
}

fn children(file: &mut File, parent: Mp4Box) -> Vec<Mp4Box> {
    children_from(file, parent, 0)
}

fn child(file: &mut File, parent: Mp4Box, kind: [u8; 4]) -> Option<Mp4Box> {
    children(file, parent).into_iter().find(|b| b.kind == kind)
}

/// Reads `buffer.len()` bytes at `offset`.
fn read_at(file: &mut File, offset: u64, buffer: &mut [u8]) -> Option<()> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(buffer).ok()
}

/// Up to `limit` bytes of `b`'s body.
fn body(file: &mut File, b: Mp4Box, limit: usize) -> Option<Vec<u8>> {
    let length = usize::try_from(b.end - b.start).ok()?.min(limit);
    let mut bytes = vec![0u8; length];
    read_at(file, b.start, &mut bytes)?;
    Some(bytes)
}

fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn be_u64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// The track's `mdia` box when it is a sound track.
fn sound_media(file: &mut File, trak: Mp4Box) -> Option<Mp4Box> {
    let mdia = child(file, trak, *b"mdia")?;
    // `hdlr`: version and flags, pre-defined, then the handler type.
    let hdlr = child(file, mdia, *b"hdlr")?;
    let handler = body(file, hdlr, 12)?;
    (handler.get(8..12)? == b"soun").then_some(mdia)
}

/// The priming a sound track's edit list declares, in its timescale.
fn edit_list_priming(file: &mut File, trak: Mp4Box, mdia: Mp4Box) -> Option<Priming> {
    let mdhd = child(file, mdia, *b"mdhd")?;
    let mdhd = body(file, mdhd, 32)?;
    // Version 1 has 64-bit creation and modification times.
    let timescale = match mdhd.first()? {
        1 => be_u32(&mdhd, 20),
        _ => be_u32(&mdhd, 12),
    };
    let edts = child(file, trak, *b"edts")?;
    let elst = child(file, edts, *b"elst")?;
    let media_time = first_media_time(&body(file, elst, 8 + 4 * 20)?)?;
    Some(Priming::EditList {
        media_time,
        timescale: timescale?,
    })
}

/// The media time of the first edit that is not empty (-1, a delay).
fn first_media_time(elst: &[u8]) -> Option<u64> {
    let version = *elst.first()?;
    let count = be_u32(elst, 4)? as usize;
    // Entries from byte 8: duration, media time, rate; 64-bit in version 1.
    let (size, at) = if version == 1 { (20, 8) } else { (12, 4) };
    (0..count).find_map(|entry| {
        let offset = 8 + entry * size + at;
        let media_time = if version == 1 {
            i64::from_be_bytes(be_u64(elst, offset)?.to_be_bytes())
        } else {
            i64::from(i32::from_be_bytes(be_u32(elst, offset)?.to_be_bytes()))
        };
        u64::try_from(media_time).ok()
    })
}

/// The priming in iTunes' gapless tag under `udta`.
fn gapless_priming(file: &mut File, udta: Mp4Box) -> Option<Priming> {
    let meta = child(file, udta, *b"meta")?;
    // An ISO `meta` is a full box (four bytes of version and flags before
    // its children); QuickTime's starts with a child at once.
    let head = body(file, meta, 8)?;
    let skip = if head.get(4..8) == Some(b"hdlr") {
        0
    } else {
        4
    };
    let ilst = children_from(file, meta, skip)
        .into_iter()
        .find(|b| &b.kind == b"ilst")?;
    for item in children(file, ilst) {
        if &item.kind != b"----" {
            continue;
        }
        let parts = children(file, item);
        let named = parts.iter().find(|b| &b.kind == b"name").and_then(|name| {
            // Version and flags, then the name.
            body(file, *name, 4 + 16).map(|bytes| bytes.get(4..) == Some(b"iTunSMPB"))
        });
        if named != Some(true) {
            continue;
        }
        let data = parts.iter().find(|b| &b.kind == b"data")?;
        // Type and locale, then the text.
        let text = body(file, *data, 8 + 256)?;
        return parse_smpb(text.get(8..)?).map(|samples| Priming::Gapless { samples });
    }
    None
}

/// The priming field of an `iTunSMPB` value: space-separated hex words,
/// the second of which is the priming in samples.
fn parse_smpb(text: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(text).ok()?;
    let word = text.split_ascii_whitespace().nth(1)?;
    u64::from_str_radix(word, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_list_converts_to_frames_at_the_rate() {
        let edit = |media_time, timescale| Priming::EditList {
            media_time,
            timescale,
        };
        assert_eq!(edit(1_024, 44_100).frames(44_100), Some(1_024));
        assert_eq!(edit(2_112, 48_000).frames(48_000), Some(2_112));
        // A timescale other than the rate: 23.2 ms at 1 kHz ticks.
        assert_eq!(edit(23, 1_000).frames(44_100), Some(1_014));
        assert_eq!(edit(1, 0).frames(44_100), None, "no timescale");
        assert_eq!(edit(0, 44_100).frames(44_100), Some(0));
    }

    #[test]
    fn a_start_past_the_bound_is_not_priming() {
        let limit = (MAX_PRIMING_SECONDS * 44_100.0) as u64;
        let gapless = |samples| Priming::Gapless { samples };
        assert_eq!(gapless(limit).frames(44_100), Some(limit));
        assert_eq!(gapless(limit + 1).frames(44_100), None);
        assert_eq!(gapless(2_112).frames(0), None, "no rate");
        let edit = Priming::EditList {
            media_time: 44_100,
            timescale: 44_100,
        };
        assert_eq!(edit.frames(44_100), None, "a second is an edit");
    }

    #[test]
    fn the_gapless_tag_names_the_priming_second() {
        let value = b" 00000000 00000840 000000E0 00000000020446E0 00000000 00000000";
        assert_eq!(parse_smpb(value), Some(2_112));
        assert_eq!(parse_smpb(b" 00000000"), None);
        assert_eq!(parse_smpb(b" 00000000 zz"), None);
    }

    #[test]
    fn an_empty_edit_is_not_priming() {
        let mut elst = vec![0u8, 0, 0, 0];
        elst.extend_from_slice(&1u32.to_be_bytes());
        elst.extend_from_slice(&500u32.to_be_bytes());
        elst.extend_from_slice(&(-1i32).to_be_bytes());
        elst.extend_from_slice(&[0, 1, 0, 0]);
        assert_eq!(first_media_time(&elst), None);
        // A delay, then the track from its priming.
        elst[4..8].copy_from_slice(&2u32.to_be_bytes());
        elst.extend_from_slice(&500u32.to_be_bytes());
        elst.extend_from_slice(&1_024i32.to_be_bytes());
        elst.extend_from_slice(&[0, 1, 0, 0]);
        assert_eq!(first_media_time(&elst), Some(1_024));
        // A count past the bytes read ends at the last whole entry.
        elst[4..8].copy_from_slice(&9u32.to_be_bytes());
        assert_eq!(first_media_time(&elst), Some(1_024));

        let mut v1 = vec![1u8, 0, 0, 0];
        v1.extend_from_slice(&1u32.to_be_bytes());
        v1.extend_from_slice(&500u64.to_be_bytes());
        v1.extend_from_slice(&2_112i64.to_be_bytes());
        assert_eq!(first_media_time(&v1), Some(2_112));
        let no_entries = [0u8, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(first_media_time(&no_entries), None);
    }
}

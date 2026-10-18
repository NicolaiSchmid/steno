//! The encoder priming of an MP4 (m4a) file's AAC track, which symphonia
//! ignores: the edit list's first non-empty media time, else iTunes'
//! gapless tag, else [`APPLE_PRIMING`] for a file in `AVAudioRecorder`'s
//! layout (brands `M4A `/`mp42`, no `udta`, ES_ID 0, stream byte 0x14),
//! else none. A wrong guess costs a quiet start, never speech.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The longest priming the decoder trims, in frames at the track's rate:
/// a longer start is an edit, not priming.
pub const MAX_PRIMING_FRAMES: u64 = 4_096;

/// The priming Apple's AAC encoder adds, in samples at the track's rate,
/// assumed only in `AVAudioRecorder`'s layout (the phone's recorder writes
/// neither an edit list nor a gapless tag).
pub const APPLE_PRIMING: u64 = 2_112;

/// The priming of an MP4 file's sound track, in ticks of its timescale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Priming {
    pub ticks: u64,
    /// The track's ticks per second (`mdhd`).
    pub timescale: u32,
    pub source: PrimingSource,
}

/// Where a [`Priming`] was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimingSource {
    EditList,
    Gapless,
    /// Neither, in `AVAudioRecorder`'s layout: [`APPLE_PRIMING`].
    Unstated,
}

impl Priming {
    /// The frames at `rate` hertz to drop, rounded to the nearest frame
    /// (exact when the timescale is the rate); `None` past
    /// [`MAX_PRIMING_FRAMES`] or for a zero rate or timescale.
    #[must_use]
    pub fn frames(self, rate: u32) -> Option<u64> {
        if self.timescale == 0 || rate == 0 {
            return None;
        }
        let scaled = u128::from(self.ticks) * u128::from(rate);
        let timescale = u128::from(self.timescale);
        let frames = u64::try_from((scaled + timescale / 2) / timescale).ok()?;
        (frames <= MAX_PRIMING_FRAMES).then_some(frames)
    }

    /// The priming of the first sound track of the MP4 file at `path`;
    /// `None` when there is none or it cannot be read.
    #[must_use]
    pub fn read(path: &Path) -> Option<Self> {
        let mut file = File::open(path).ok()?;
        let end = file.metadata().ok()?.len();
        let mut boxes = Boxes::new(&mut file, 0, end);
        let ftyp = boxes.next()?;
        if &ftyp.kind != b"ftyp" {
            return None;
        }
        let moov = std::iter::from_fn(|| boxes.next()).find(|b| &b.kind == b"moov")?;
        // The first sound track, as symphonia's default track is.
        let mut sound = None;
        let mut gapless = None;
        let mut udta = false;
        for child in children(&mut file, moov) {
            match &child.kind {
                b"trak" if sound.is_none() => {
                    sound = sound_media(&mut file, child).map(|mdia| (child, mdia));
                }
                b"udta" => {
                    udta = true;
                    if gapless.is_none() {
                        gapless = gapless_priming(&mut file, child);
                    }
                }
                _ => {}
            }
        }
        let (trak, mdia) = sound?;
        let (timescale, edit) = timescale_and_edit(&mut file, trak, mdia)?;
        let (ticks, source) = match (edit, gapless) {
            (Some(ticks), _) => (ticks, PrimingSource::EditList),
            (None, Some(ticks)) => (ticks, PrimingSource::Gapless),
            (None, None) if !udta && recorder_layout(&mut file, ftyp, mdia) => {
                (APPLE_PRIMING, PrimingSource::Unstated)
            }
            (None, None) => return None,
        };
        Some(Self {
            ticks,
            timescale,
            source,
        })
    }
}

/// One box: its type and where its body lies in the file.
#[derive(Debug, Clone, Copy)]
pub(super) struct Mp4Box {
    pub(super) kind: [u8; 4],
    pub(super) start: u64,
    pub(super) end: u64,
}

/// Why [`Boxes::next_sized`] stopped short of a box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BoxError {
    /// Its header could not be read.
    Unreadable,
    /// It runs past the end of what holds it, or is shorter than its own
    /// header: its type, its offset and the size it declares.
    Oversized { kind: [u8; 4], at: u64, size: u64 },
}

/// The boxes between two offsets, in order; a box that runs past `end` or
/// cannot be read ends the walk.
pub(super) struct Boxes<'a> {
    file: &'a mut File,
    at: u64,
    end: u64,
}

impl<'a> Boxes<'a> {
    pub(super) fn new(file: &'a mut File, start: u64, end: u64) -> Self {
        Self {
            file,
            at: start,
            end,
        }
    }

    fn next(&mut self) -> Option<Mp4Box> {
        self.next_sized().ok().flatten()
    }

    /// The next box; `Ok(None)` once no header fits before `end`, and why
    /// not for a box that does not fit or cannot be read.
    pub(super) fn next_sized(&mut self) -> Result<Option<Mp4Box>, BoxError> {
        if self
            .at
            .checked_add(8)
            .is_none_or(|header| header > self.end)
        {
            return Ok(None);
        }
        let mut header = [0u8; 8];
        read_at(self.file, self.at, &mut header).ok_or(BoxError::Unreadable)?;
        let size = u64::from(u32::from_be_bytes(
            field(&header, 0).ok_or(BoxError::Unreadable)?,
        ));
        let kind = field(&header, 4).ok_or(BoxError::Unreadable)?;
        let (body, size) = match size {
            // To the end of the enclosing box (or file).
            0 => (self.at + 8, self.end - self.at),
            // A 64-bit size follows the type.
            1 => {
                let mut large = [0u8; 8];
                read_at(self.file, self.at + 8, &mut large).ok_or(BoxError::Unreadable)?;
                (self.at + 16, u64::from_be_bytes(large))
            }
            size => (self.at + 8, size),
        };
        let oversized = BoxError::Oversized {
            kind,
            at: self.at,
            size,
        };
        let end = self.at.checked_add(size).ok_or(oversized)?;
        if end > self.end || body > end {
            return Err(oversized);
        }
        self.at = end;
        Ok(Some(Mp4Box {
            kind,
            start: body,
            end,
        }))
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

pub(super) fn read_at(file: &mut File, offset: u64, buffer: &mut [u8]) -> Option<()> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(buffer).ok()
}

fn body(file: &mut File, b: Mp4Box, limit: usize) -> Option<Vec<u8>> {
    let length = usize::try_from(b.end - b.start).ok()?.min(limit);
    let mut bytes = vec![0u8; length];
    read_at(file, b.start, &mut bytes)?;
    Some(bytes)
}

/// The `N` bytes at `at`, `None` past the end: a big-endian field.
fn field<const N: usize>(bytes: &[u8], at: usize) -> Option<[u8; N]> {
    bytes.get(at..at + N)?.try_into().ok()
}

/// The track's `mdia` box when it is a sound track.
fn sound_media(file: &mut File, trak: Mp4Box) -> Option<Mp4Box> {
    let mdia = child(file, trak, *b"mdia")?;
    // `hdlr`: version and flags, pre-defined, then the handler type.
    let hdlr = child(file, mdia, *b"hdlr")?;
    let handler = body(file, hdlr, 12)?;
    (handler.get(8..12)? == b"soun").then_some(mdia)
}

/// A sound track's timescale and the media time its edit list starts at,
/// `None` for the second when it has no edit list or only empty edits.
fn timescale_and_edit(file: &mut File, trak: Mp4Box, mdia: Mp4Box) -> Option<(u32, Option<u64>)> {
    let mdhd = child(file, mdia, *b"mdhd")?;
    let mdhd = body(file, mdhd, 32)?;
    // Version 1 has 64-bit creation and modification times.
    let timescale = match mdhd.first()? {
        1 => field(&mdhd, 20),
        _ => field(&mdhd, 12),
    }
    .map(u32::from_be_bytes)?;
    let edit = child(file, trak, *b"edts")
        .and_then(|edts| child(file, edts, *b"elst"))
        .and_then(|elst| body(file, elst, 8 + 4 * 20))
        .and_then(|elst| first_media_time(&elst));
    Some((timescale, edit))
}

/// The media time of the first edit that is not empty (-1, a delay).
fn first_media_time(elst: &[u8]) -> Option<u64> {
    let version = *elst.first()?;
    let count = u32::from_be_bytes(field(elst, 4)?) as usize;
    // Entries from byte 8: duration, media time, rate; 64-bit in version 1.
    let (size, at) = if version == 1 { (20, 8) } else { (12, 4) };
    // A count past the bytes read ends at the last whole entry.
    let whole = elst.len().saturating_sub(8) / size;
    (0..count.min(whole)).find_map(|entry| {
        let offset = 8 + entry * size + at;
        let media_time = if version == 1 {
            i64::from_be_bytes(field(elst, offset)?)
        } else {
            i64::from(i32::from_be_bytes(field(elst, offset)?))
        };
        u64::try_from(media_time).ok()
    })
}

/// Whether a file that declares no priming is laid out as
/// `AVAudioRecorder` writes AAC in an `.m4a`, apart from the missing
/// `udta`, which the caller checks.
fn recorder_layout(file: &mut File, ftyp: Mp4Box, mdia: Mp4Box) -> bool {
    body(file, ftyp, 64).is_some_and(|ftyp| recorder_brands(&ftyp))
        && stream_signature(file, mdia) == Some((0, 0x14))
}

/// Whether the body of an `ftyp` box names `AVAudioRecorder`'s brands for
/// an `.m4a`: the major brand `M4A ` and `mp42` among the compatible ones.
pub(crate) fn recorder_brands(ftyp: &[u8]) -> bool {
    // The major brand, the minor version, then the compatible brands.
    ftyp.get(..4) == Some(b"M4A ")
        && ftyp
            .get(8..)
            .is_some_and(|compatible| compatible.as_chunks::<4>().0.contains(b"mp42"))
}

/// The ES_ID and the stream byte (the stream type, the upstream flag and
/// a reserved bit) of the `esds` in the sound track's first sample entry,
/// when that is `mp4a`.
fn stream_signature(file: &mut File, mdia: Mp4Box) -> Option<(u16, u8)> {
    let minf = child(file, mdia, *b"minf")?;
    let stbl = child(file, minf, *b"stbl")?;
    let stsd = child(file, stbl, *b"stsd")?;
    // Version and flags and the entry count, then the entries.
    let entry = children_from(file, stsd, 8).into_iter().next()?;
    if &entry.kind != b"mp4a" {
        return None;
    }
    // A sound sample entry's fields take 28 bytes, and QuickTime's
    // versions 1 and 2 add 16 and 36; its version is at byte 8.
    let head = body(file, entry, 10)?;
    let skip = match u16::from_be_bytes(field(&head, 8)?) {
        0 => 28,
        1 => 44,
        2 => 64,
        _ => return None,
    };
    let esds = children_from(file, entry, skip)
        .into_iter()
        .find(|b| &b.kind == b"esds")?;
    let esds = body(file, esds, 64)?;
    // Version and flags, then the ES descriptor: its ES_ID, a flags byte
    // and the fields those flags add.
    let at = descriptor(&esds, 4, 0x03)?;
    let es_id = u16::from_be_bytes(field(&esds, at)?);
    let flags = *esds.get(at + 2)?;
    let mut next = at + 3;
    if flags & 0x80 != 0 {
        next += 2;
    }
    if flags & 0x40 != 0 {
        next += 1 + usize::from(*esds.get(next)?);
    }
    if flags & 0x20 != 0 {
        next += 2;
    }
    // The decoder config: the object type, then the stream byte.
    let at = descriptor(&esds, next, 0x04)?;
    Some((es_id, *esds.get(at + 1)?))
}

/// Where the body of the descriptor at `at` starts, when its tag is `tag`:
/// one tag byte, then a length of one to four bytes, seven bits each.
fn descriptor(bytes: &[u8], at: usize, tag: u8) -> Option<usize> {
    if *bytes.get(at)? != tag {
        return None;
    }
    (1..=4).find_map(|length| (*bytes.get(at + length)? & 0x80 == 0).then_some(at + length + 1))
}

/// The priming in iTunes' gapless tag under `udta`, in samples.
fn gapless_priming(file: &mut File, udta: Mp4Box) -> Option<u64> {
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
        return parse_smpb(text.get(8..)?);
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

    fn priming(ticks: u64, timescale: u32) -> Priming {
        Priming {
            ticks,
            timescale,
            source: PrimingSource::EditList,
        }
    }

    #[test]
    fn a_priming_converts_to_frames_at_the_rate() {
        assert_eq!(priming(1_024, 44_100).frames(44_100), Some(1_024));
        assert_eq!(priming(2_112, 48_000).frames(48_000), Some(2_112));
        // A timescale other than the rate: 23.2 ms at 1 kHz ticks.
        assert_eq!(priming(23, 1_000).frames(44_100), Some(1_014));
        // HE-AAC: a full-rate timescale, decoded at the core rate.
        assert_eq!(priming(2_112, 44_100).frames(22_050), Some(1_056));
        assert_eq!(priming(1, 0).frames(44_100), None, "no timescale");
        assert_eq!(priming(0, 44_100).frames(44_100), Some(0));
    }

    #[test]
    fn a_start_past_the_bound_is_not_priming() {
        let limit = MAX_PRIMING_FRAMES;
        assert_eq!(priming(limit, 44_100).frames(44_100), Some(limit));
        assert_eq!(priming(limit + 1, 44_100).frames(44_100), None);
        // A frame count at any rate: Apple's priming at 8 kHz is trimmed.
        assert_eq!(priming(2_112, 8_000).frames(8_000), Some(2_112));
        assert_eq!(priming(2_112, 44_100).frames(0), None, "no rate");
        assert_eq!(priming(44_100, 44_100).frames(44_100), None, "a second");
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
        v1.extend_from_slice(&[0, 1, 0, 0]);
        assert_eq!(first_media_time(&v1), Some(2_112));
        // Version 1 entries are 20 bytes: a delay, then the priming.
        let mut delay = vec![1u8, 0, 0, 0];
        delay.extend_from_slice(&2u32.to_be_bytes());
        for media_time in [-1i64, 1_024] {
            delay.extend_from_slice(&500u64.to_be_bytes());
            delay.extend_from_slice(&media_time.to_be_bytes());
            delay.extend_from_slice(&[0, 1, 0, 0]);
        }
        assert_eq!(first_media_time(&delay), Some(1_024));
        let no_entries = [0u8, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(first_media_time(&no_entries), None);
    }
}

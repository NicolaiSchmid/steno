//! The sizes symphonia allocates from, checked against the file before its
//! demuxer opens it (A17 of `.plans/2026-10-07-stable-promotion.md`).
//! Symphonia 0.5.5 reserves memory straight from some of a file's fields,
//! and a failed allocation aborts the process, which no `catch_unwind`
//! stops: on Linux a 1 TiB `ilst` `data` atom or CAF `kuki` chunk aborts
//! every read, and the sample tables of any MP4 file do on a machine that
//! cannot back 16 to 64 GiB. [`check`] walks an MP4 file's boxes or a CAF
//! file's chunks, reading only headers and the sample tables, and refuses
//! the file as an ordinary decode error ([`CodecError::UnsupportedFormat`]:
//! the meeting fails, retryably, and the file is kept) when:
//!
//! - a box or chunk is larger than the file or than the box that holds it,
//!   or shorter than its own header; except the media data at the top
//!   level (`mdat`, a CAF's `data`, which a CAF may also leave open with a
//!   size of -1): a file cut off ends inside it, and symphonia reads it a
//!   packet at a time, never in one allocation;
//! - a sample table (`stts`, `stsc`, `stsz`, `stco`, `co64`, a fragment's
//!   `trun`) counts more entries than its box holds, or a sample (in
//!   `stsz`, `trun`, or as a `tfhd` or `trex` default) is larger than the
//!   file: symphonia reads each sample into an allocation of its size;
//! - a box symphonia subtracts a fixed header from is shorter than it
//!   (`hdlr` 24 bytes, an `ilst` item's `data` 8, `mean` and `name` 4);
//! - a CAF packet table (`pakt`) counts more packets than its entries could
//!   describe: one byte each at least, two where both the packet's size and
//!   its frames vary, and none for a constant bit rate.
//!
//! The walk follows symphonia's own reading: the same containers, `meta`
//! as a full box, an `ilst` item's children, and the one sample entry it
//! reads in `stsd`, whose child boxes start after the audio sample entry's
//! fields for its version (28, 44 or 64 bytes). WAV and MP3 are not walked:
//! symphonia's RIFF reader checks a chunk against its parent, and MP3 has
//! no sized boxes. What the walk does not bound, and why that is safe:
//!
//! - an `esds` descriptor's length (`atoms/esds.rs` reads a
//!   `DecoderSpecificInfo` of that length): it is encoded in at most four
//!   bytes of seven bits, so at most 256 MiB;
//! - a FLAC metadata block (`atoms/flac.rs`): 24 bits, at most 16 MiB;
//! - a CAF channel layout (`chan`): symphonia collects its descriptions
//!   without reserving their count.
//!
//! Rust only: AVFoundation, which the Swift app used, reported such a file
//! as unreadable.

use std::fs::File;
use std::path::Path;

use super::CodecError;
use super::priming::{BoxError, Boxes, Mp4Box, read_at};

/// How deep the walk follows boxes; symphonia's deepest is about eight.
const MAX_DEPTH: usize = 12;

/// The boxes whose body is boxes, as symphonia reads them.
const CONTAINERS: [&[u8; 4]; 13] = [
    b"moov", b"trak", b"mdia", b"minf", b"stbl", b"udta", b"edts", b"dinf", b"mvex", b"moof",
    b"traf", b"mfra", b"ilst",
];

/// The audio sample entries symphonia reads, whose children it walks.
const AUDIO_ENTRIES: [&[u8; 4]; 16] = [
    b"mp4a", b"alac", b"fLaC", b"Opus", b".mp3", b"lpcm", b"sowt", b"twos", b"in24", b"in32",
    b"fl32", b"fl64", b"raw ", b"ulaw", b"alaw", b"ac-3",
];

/// Refuses the file at `path` when symphonia would allocate from a size the
/// file cannot hold; see the module doc. Anything but an MP4 or CAF file
/// passes, and so does a file too short to tell.
pub(crate) fn check(path: &Path) -> Result<(), CodecError> {
    let refused = |why: String| CodecError::UnsupportedFormat(format!("{}: {why}", path.display()));
    let mut file =
        File::open(path).map_err(|error| CodecError::Io(format!("{}: {error}", path.display())))?;
    let length = file
        .metadata()
        .map_err(|error| CodecError::Io(format!("{}: {error}", path.display())))?
        .len();
    let mut magic = [0u8; 8];
    if read_at(&mut file, 0, &mut magic).is_none() {
        return Ok(());
    }
    let mut walk = Walk { file, length };
    if &magic[..4] == b"caff" {
        walk.caf().map_err(refused)
    } else if [b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide"]
        .iter()
        .any(|kind| magic[4..] == kind[..])
    {
        walk.mp4(0, length, 0, None).map_err(refused)
    } else {
        Ok(())
    }
}

struct Walk {
    file: File,
    length: u64,
}

impl Walk {
    /// The boxes in `start..end`, `depth` deep, inside `parent`.
    fn mp4(
        &mut self,
        start: u64,
        end: u64,
        depth: usize,
        parent: Option<[u8; 4]>,
    ) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Ok(());
        }
        let children = match self.boxes(start, end, depth == 0) {
            Ok(children) => children,
            Err(BoxError::Oversized { kind, at, size }) => {
                return Err(format!(
                    "the `{}` box at byte {at} declares {size} bytes, more than {} holds",
                    name(kind),
                    parent.map_or_else(|| "the file".to_owned(), |p| format!("`{}`", name(p)))
                ));
            }
            Err(BoxError::Unreadable) => {
                return Err(format!("a box at byte {start}.. cannot be read"));
            }
        };
        for found in children {
            self.mp4_box(found, depth, parent)?;
        }
        Ok(())
    }

    /// One box's own sizes, then its children.
    fn mp4_box(
        &mut self,
        found: Mp4Box,
        depth: usize,
        parent: Option<[u8; 4]>,
    ) -> Result<(), String> {
        let body = found.end - found.start;
        let kind = &found.kind;
        let at_least = |bytes: u64| {
            if body < bytes {
                Err(format!(
                    "the `{}` box holds {body} bytes, fewer than its {bytes}",
                    name(*kind)
                ))
            } else {
                Ok(())
            }
        };
        let inside = Some(found.kind);
        match kind {
            _ if CONTAINERS.contains(&kind) || parent.as_ref() == Some(b"ilst") => {
                self.mp4(found.start, found.end, depth + 1, inside)
            }
            b"meta" => {
                at_least(4)?;
                self.mp4(found.start + 4, found.end, depth + 1, inside)
            }
            b"hdlr" => at_least(24),
            b"data" => at_least(8),
            b"mean" | b"name" => at_least(4),
            b"stsd" => {
                at_least(8)?;
                self.sample_entries(found, depth)
            }
            b"stts" | b"stsc" | b"stco" | b"co64" => {
                at_least(8)?;
                let entry = match kind {
                    b"stts" | b"co64" => 8,
                    b"stsc" => 12,
                    _ => 4,
                };
                let count = u64::from(self.u32_at(found.start + 4)?);
                entries_fit(found, found.start + 8, count, entry)
            }
            b"stsz" => {
                at_least(12)?;
                self.sample_sizes(found)
            }
            b"trun" => {
                at_least(8)?;
                self.fragment_run(found)
            }
            // `tfhd`: version and flags, track id, then the optional fields
            // its flags name, the default sample size after the base data
            // offset (8), the description index (4) and the duration (4).
            b"tfhd" => {
                at_least(8)?;
                let flags = self.u32_at(found.start)? & 0x00ff_ffff;
                if flags & 0x10 == 0 {
                    return Ok(());
                }
                let mut at = found.start + 8;
                for (flag, bytes) in [(0x1, 8), (0x2, 4), (0x8, 4)] {
                    if flags & flag != 0 {
                        at += bytes;
                    }
                }
                self.sample_size(found, at)
            }
            // `trex`: version and flags, then track id, description index,
            // duration, and the default sample size.
            b"trex" => self.sample_size(found, found.start + 16),
            _ => Ok(()),
        }
    }

    /// The sample entries of `stsd`, and the child boxes of an audio entry.
    fn sample_entries(&mut self, stsd: Mp4Box, depth: usize) -> Result<(), String> {
        let entries = match self.boxes(stsd.start + 8, stsd.end, false) {
            Ok(entries) => entries,
            Err(BoxError::Oversized { kind, at, size }) => {
                return Err(format!(
                    "the `{}` sample entry at byte {at} declares {size} bytes, more than `stsd` holds",
                    name(kind)
                ));
            }
            Err(BoxError::Unreadable) => return Err("a sample entry cannot be read".to_owned()),
        };
        for entry in entries
            .into_iter()
            .filter(|entry| AUDIO_ENTRIES.contains(&&entry.kind))
        {
            let mut version = [0u8; 2];
            if read_at(&mut self.file, entry.start + 8, &mut version).is_none() {
                continue;
            }
            let fields = match u16::from_be_bytes(version) {
                0 => 28,
                1 => 44,
                2 => 64,
                // Symphonia refuses any other version itself.
                _ => continue,
            };
            if entry.start + fields <= entry.end {
                self.mp4(entry.start + fields, entry.end, depth + 1, Some(entry.kind))?;
            }
        }
        Ok(())
    }

    /// The boxes in `start..end`, in order; at the `top` level, one `mdat`
    /// running past the file ends the list, as a file cut off inside its
    /// media data does.
    fn boxes(&mut self, start: u64, end: u64, top: bool) -> Result<Vec<Mp4Box>, BoxError> {
        let mut found = Vec::new();
        let mut boxes = Boxes::new(&mut self.file, start, end);
        loop {
            match boxes.next_sized() {
                Ok(Some(next)) => found.push(next),
                Ok(None) => return Ok(found),
                Err(BoxError::Oversized { kind, .. }) if top && &kind == b"mdat" => {
                    return Ok(found);
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// `stsz`: a constant size, or one entry a sample; each no larger than
    /// the file.
    fn sample_sizes(&mut self, stsz: Mp4Box) -> Result<(), String> {
        let constant = self.u32_at(stsz.start + 4)?;
        let count = u64::from(self.u32_at(stsz.start + 8)?);
        if constant != 0 {
            return self.no_larger_than_the_file(stsz, u64::from(constant));
        }
        entries_fit(stsz, stsz.start + 12, count, 4)?;
        self.sizes_within_the_file(stsz, stsz.start + 12, count, 4, 0)
    }

    /// `trun`: its samples fit, each no larger than the file.
    fn fragment_run(&mut self, trun: Mp4Box) -> Result<(), String> {
        let flags = self.u32_at(trun.start)? & 0x00ff_ffff;
        let count = u64::from(self.u32_at(trun.start + 4)?);
        let mut first = trun.start + 8;
        for flag in [0x1, 0x4] {
            if flags & flag != 0 {
                first += 4;
            }
        }
        let fields = [0x100, 0x200, 0x400, 0x800];
        let entry = 4 * fields.iter().filter(|flag| flags & **flag != 0).count() as u64;
        entries_fit(trun, first, count, entry)?;
        if flags & 0x200 == 0 {
            return Ok(());
        }
        // The size follows the duration when both are there.
        let offset = if flags & 0x100 != 0 { 4 } else { 0 };
        self.sizes_within_the_file(trun, first, count, entry, offset)
    }

    /// The `u32` at `at` in `found`, a sample's size, is no larger than the file.
    fn sample_size(&mut self, found: Mp4Box, at: u64) -> Result<(), String> {
        if at + 4 > found.end {
            return Ok(());
        }
        let size = u64::from(self.u32_at(at)?);
        self.no_larger_than_the_file(found, size)
    }

    /// The sizes of a table of `count` entries of `stride` bytes from
    /// `from`, each `offset` bytes into its entry.
    fn sizes_within_the_file(
        &mut self,
        found: Mp4Box,
        from: u64,
        count: u64,
        stride: u64,
        offset: usize,
    ) -> Result<(), String> {
        // Read in blocks: a table of an hour's recording is under a MiB.
        const BLOCK: u64 = 4_096;
        let mut index = 0;
        while index < count {
            let entries = (count - index).min(BLOCK);
            let at = from + index * stride;
            let mut bytes = vec![0u8; usize::try_from(entries * stride).unwrap_or(usize::MAX)];
            read_at(&mut self.file, at, &mut bytes)
                .ok_or_else(|| format!("the `{}` table cannot be read", name(found.kind)))?;
            for chunk in bytes.chunks_exact(usize::try_from(stride).unwrap_or(4)) {
                let field = chunk.get(offset..offset + 4).unwrap_or_default();
                let size = u64::from(u32::from_be_bytes(field.try_into().unwrap_or_default()));
                self.no_larger_than_the_file(found, size)?;
            }
            index += entries;
        }
        Ok(())
    }

    fn no_larger_than_the_file(&self, found: Mp4Box, size: u64) -> Result<(), String> {
        if size > self.length {
            return Err(format!(
                "the `{}` box gives a sample of {size} bytes, more than the file's {}",
                name(found.kind),
                self.length
            ));
        }
        Ok(())
    }

    fn u32_at(&mut self, at: u64) -> Result<u32, String> {
        let mut bytes = [0u8; 4];
        read_at(&mut self.file, at, &mut bytes)
            .ok_or_else(|| format!("byte {at} cannot be read"))?;
        Ok(u32::from_be_bytes(bytes))
    }

    /// A CAF's chunks: the header, then chunks of a type and a 64-bit size.
    fn caf(&mut self) -> Result<(), String> {
        let mut at = 8u64;
        // `mBytesPerPacket` and `mFramesPerPacket` of `desc`, once read.
        let mut packets: Option<(u32, u32)> = None;
        while at.saturating_add(12) <= self.length {
            let mut header = [0u8; 12];
            read_at(&mut self.file, at, &mut header)
                .ok_or_else(|| format!("the chunk at byte {at} cannot be read"))?;
            let kind: [u8; 4] = header[..4].try_into().unwrap_or_default();
            let size = i64::from_be_bytes(header[4..].try_into().unwrap_or_default());
            let body = at + 12;
            let data = &kind == b"data";
            let end = match u64::try_from(size) {
                Ok(size) => body.checked_add(size),
                // -1: the data runs to the end of the file.
                Err(_) if data && size == -1 => return Ok(()),
                Err(_) => {
                    return Err(format!(
                        "the `{}` chunk at byte {at} declares {size} bytes",
                        name(kind)
                    ));
                }
            };
            let Some(end) = end.filter(|end| *end <= self.length) else {
                if data {
                    // Cut off inside its audio.
                    return Ok(());
                }
                return Err(format!(
                    "the `{}` chunk at byte {at} declares {size} bytes, more than the file holds",
                    name(kind)
                ));
            };
            match &kind {
                b"desc" if end - body >= 24 => {
                    let mut fields = [0u8; 8];
                    read_at(&mut self.file, body + 16, &mut fields)
                        .ok_or_else(|| "the description cannot be read".to_owned())?;
                    packets = Some((
                        u32::from_be_bytes(fields[..4].try_into().unwrap_or_default()),
                        u32::from_be_bytes(fields[4..].try_into().unwrap_or_default()),
                    ));
                }
                b"pakt" if end - body >= 24 => {
                    let mut count = [0u8; 8];
                    read_at(&mut self.file, body, &mut count)
                        .ok_or_else(|| "the packet table cannot be read".to_owned())?;
                    let count = u64::try_from(i64::from_be_bytes(count)).unwrap_or(0);
                    let entry = match packets {
                        Some((0, 0)) => 2,
                        Some((0, _) | (_, 0)) => 1,
                        // Constant: no entries (symphonia refuses a count),
                        // or no description yet (symphonia refuses that).
                        _ => u64::MAX,
                    };
                    let room = end - body - 24;
                    if count > 0 && count.saturating_mul(entry) > room {
                        return Err(format!(
                            "the packet table counts {count} packets in {room} bytes"
                        ));
                    }
                }
                _ => {}
            }
            at = end;
        }
        Ok(())
    }
}

/// `count` entries of `entry` bytes from `from` fit in `found`.
fn entries_fit(found: Mp4Box, from: u64, count: u64, entry: u64) -> Result<(), String> {
    let room = found.end.saturating_sub(from);
    if count.saturating_mul(entry) > room {
        return Err(format!(
            "the `{}` box counts {count} entries of {entry} bytes in {room} bytes",
            name(found.kind)
        ));
    }
    Ok(())
}

/// A four-character code as text.
fn name(kind: [u8; 4]) -> String {
    String::from_utf8_lossy(&kind).into_owned()
}

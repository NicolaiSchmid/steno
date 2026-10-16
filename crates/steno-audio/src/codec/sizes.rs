//! The sizes symphonia allocates from, checked against the file before its
//! demuxer opens it (A17 of `.plans/2026-10-07-stable-promotion.md`).
//! Symphonia 0.5.5 reserves memory straight from some of a file's fields,
//! and a failed allocation aborts the process, which no `catch_unwind`
//! stops: on Linux a 1 TiB `ilst` `data` atom or CAF `kuki` chunk aborts
//! every read, and the sample tables of any MP4 file do on a machine that
//! cannot back 16 to 64 GiB.
//!
//! [`check`] first tells which reader symphonia opens the file with
//! ([`Start`]), and the probe opens that reader at byte 0, where the walk
//! starts too, so the walk and the read see the same bytes: an MP4 file is
//! a run of well-formed top-level boxes from byte 0 that holds an `ftyp`
//! (a `free` or `wide` box may come first), a CAF file starts with `caff`.
//! Any other file is not walked, and the probe offers it only to
//! symphonia's WAV, MP3 and ADTS readers, which size nothing from the file
//! unchecked. Symphonia's own probe would scan the first MiB for an `ftyp`
//! and read from there, past junk, an ID3 tag or into a `free` box, where
//! the walk never looked; and it fails on a box before the `ftyp`, which
//! the MP4 reader opened at byte 0 seeks past.
//!
//! The walk reads only headers and the sample tables, follows boxes only
//! where symphonia reads them (`isomp4` `atoms/*.rs`, named at each rule),
//! and refuses the file as an ordinary decode error
//! ([`CodecError::UnsupportedFormat`]: the meeting fails, retryably, and
//! the file is kept) when:
//!
//! - a box or chunk is larger than the file or than the box that holds it,
//!   or shorter than its own header. A top-level box symphonia only seeks
//!   past (anything but [`PARSED_AT_TOP`]: `mdat`, `free`, `skip`, `wide`
//!   and the rest) may run past the file's end, as a file cut off inside
//!   it does; so may a CAF's `data`, which may also be left open with a
//!   size of -1. Symphonia never reads such a box in one allocation;
//! - a sample table (`stts`, `stsc`, `stsz`, `stco`, `co64`, a fragment's
//!   `trun`) counts more entries than its box holds, or a sample (in
//!   `stsz`, `trun`, or as a `tfhd` or `trex` default) is larger than the
//!   file: symphonia reads each sample into an allocation of its size;
//! - a box symphonia subtracts a fixed header from is shorter than it
//!   (`hdlr` 24 bytes, `meta` 4, an `ilst` item's `data` 8, `mean` and
//!   `name` 4);
//! - a CAF packet table (`pakt`) counts more packets than its entries could
//!   describe (one byte each at least, two where both the packet's size and
//!   its frames vary, none for a constant bit rate), or gives a packet
//!   larger than the file; or a CAF description gives a packet larger than
//!   the file, or uncompressed frames larger than [`MAX_PCM_FRAME`]
//!   (symphonia reads 1 152 of them into one allocation).
//!
//! What the walk does not bound, and why that is safe:
//!
//! - an audio sample entry's children: an `esds` descriptor's length
//!   (`atoms/esds.rs` reads a `DecoderSpecificInfo` of that length) is
//!   encoded in at most four bytes of seven bits, so at most 256 MiB; a
//!   FLAC metadata block (`atoms/flac.rs`) has 24 bits, at most 16 MiB; an
//!   ALAC cookie is 24 or 48 bytes and an Opus header is capped
//!   (`atoms/alac.rs`, `atoms/opus.rs`);
//! - a CAF channel layout (`chan`): symphonia collects its descriptions
//!   without reserving their count;
//! - the reservations a bounded count still allows: symphonia reserves 32
//!   bytes a `pakt` packet (`chunks.rs:434`), up to 32 times the table's
//!   size at one byte a packet, and 16 bytes a 12-byte `stsc` entry
//!   (`atoms/stsc.rs:71`), 1.3 times; every other table reserves no more
//!   than its own size, so a file reserves at most 32 times its length.
//!
//! Rust only: AVFoundation, which the Swift app used, reported such a file
//! as unreadable.

use std::fs::File;
use std::io::{BufReader, Read as _, Seek as _, SeekFrom};
use std::path::Path;

use super::CodecError;
use super::priming::{BoxError, Boxes, Mp4Box, read_at};

/// Which of symphonia's readers opens a file, as [`check`] found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Start {
    /// MP4: well-formed top-level boxes from byte 0, one of them `ftyp`.
    Mp4,
    /// CAF: `caff` at byte 0.
    Caf,
    /// Anything else, not walked.
    Other,
}

/// The top-level boxes symphonia parses: `ftyp`, `moov`, `sidx` and
/// `meta` as it opens the file (`demuxer.rs:350-395`), `moof` as it reads
/// the fragments (`demuxer.rs:186-198`). It seeks past every other one.
const PARSED_AT_TOP: [&[u8; 4]; 5] = [b"ftyp", b"moov", b"sidx", b"meta", b"moof"];

/// The `ilst` items symphonia reads (`atoms/ilst.rs:625-762`, less the two
/// it skips, `©art` and `hdvd`); it skips every other item unread.
const ITEMS: [&[u8; 4]; 43] = [
    b"rtng", b"aART", b"\xa9alb", b"\xa9ART", b"catg", b"\xa9cmt", b"cpil", b"\xa9wrt", b"cprt",
    b"covr", b"\xa9gen", b"\xa9day", b"desc", b"disk", b"\xa9enc", b"\xa9too", b"pgap", b"gnre",
    b"\xa9grp", b"egid", b"keyw", b"ldes", b"\xa9lyr", b"stik", b"ownr", b"pcst", b"purd", b"rate",
    b"soaa", b"soal", b"soar", b"soco", b"sonm", b"tmpo", b"trkn", b"\xa9nam", b"tven", b"tves",
    b"tvnn", b"tvsn", b"tvsh", b"purl", b"----",
];

/// The largest uncompressed CAF frame the walk lets through: symphonia
/// plays at most 32 channels of 8 bytes (256 bytes), and reads 1 152
/// frames into one allocation (`symphonia-format-caf` `demuxer.rs:83`).
pub(crate) const MAX_PCM_FRAME: u64 = 4_096;

/// Finds where symphonia's reader for the file at `path` starts, and
/// refuses the file when that reader would allocate from a size the file
/// cannot hold; see the module doc. Hands on the file it opened, at byte 0.
pub(crate) fn check(path: &Path) -> Result<(Start, File), CodecError> {
    let io = |error: std::io::Error| CodecError::Io(format!("{}: {error}", path.display()));
    let refused = |why: String| CodecError::UnsupportedFormat(format!("{}: {why}", path.display()));
    let file = File::open(path).map_err(io)?;
    let length = file.metadata().map_err(io)?.len();
    let mut walk = Walk { file, length };
    let start = walk.start();
    match start {
        Start::Mp4 => walk.mp4(0, length, &[]).map_err(refused)?,
        Start::Caf => walk.caf().map_err(refused)?,
        Start::Other => {}
    }
    walk.file.rewind().map_err(io)?;
    Ok((start, walk.file))
}

struct Walk {
    file: File,
    length: u64,
}

impl Walk {
    /// `caff` at byte 0, or an `ftyp` among well-formed top-level boxes
    /// from byte 0.
    fn start(&mut self) -> Start {
        let mut magic = [0u8; 4];
        if read_at(&mut self.file, 0, &mut magic).is_some() && &magic == b"caff" {
            return Start::Caf;
        }
        let mut boxes = Boxes::new(&mut self.file, 0, self.length);
        loop {
            match boxes.next_sized() {
                Ok(Some(found)) if &found.kind == b"ftyp" => return Start::Mp4,
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return Start::Other,
            }
        }
    }

    /// The boxes in `start..end` inside `ancestors`, outermost first.
    fn mp4(&mut self, start: u64, end: u64, ancestors: &[&[u8; 4]]) -> Result<(), String> {
        let children = match self.boxes(start, end, ancestors.is_empty()) {
            Ok(children) => children,
            Err(BoxError::Oversized { kind, at, size }) => {
                return Err(format!(
                    "the `{}` box at byte {at} declares {size} bytes, more than {} holds",
                    name(kind),
                    ancestors
                        .last()
                        .map_or_else(|| "the file".to_owned(), |p| format!("`{}`", name(**p)))
                ));
            }
            Err(BoxError::Unreadable) => {
                return Err(format!("a box at byte {start}.. cannot be read"));
            }
        };
        for found in children {
            self.mp4_box(found, ancestors)?;
        }
        Ok(())
    }

    /// One box's own sizes, then its children, where symphonia reads them.
    fn mp4_box(&mut self, found: Mp4Box, ancestors: &[&[u8; 4]]) -> Result<(), String> {
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
        let inside: Vec<&[u8; 4]> = ancestors.iter().copied().chain([kind]).collect();
        match (ancestors, kind) {
            // demuxer.rs:350-395 and :186-198; moov.rs:61-72; trak.rs:41-48;
            // mdia.rs:36-43; minf.rs:38-42; moof.rs:43-47; meta.rs:57;
            // ilst.rs:625-762.
            ([], b"moov" | b"moof")
            | ([b"moov"], b"trak" | b"mvex" | b"udta")
            | ([b"moov", b"trak"], b"mdia")
            | ([b"moov", b"trak", b"mdia"], b"minf")
            | ([b"moov", b"trak", b"mdia", b"minf"], b"stbl")
            | ([b"moof"], b"traf")
            | ([.., b"meta"], b"ilst") => self.mp4(found.start, found.end, &inside),
            ([.., b"ilst"], item) if ITEMS.contains(&item) => {
                self.mp4(found.start, found.end, &inside)
            }
            // A full box (meta.rs:45), at the top and in `moov/udta`
            // (demuxer.rs:384, udta.rs:44).
            ([] | [b"moov", b"udta"], b"meta") => {
                at_least(4)?;
                self.mp4(found.start + 4, found.end, &inside)
            }
            // hdlr.rs:75, read in `mdia` only.
            ([b"moov", b"trak", b"mdia"], b"hdlr") => at_least(24),
            // ilst.rs:594-601, inside an item.
            ([.., b"ilst", _], b"data") => at_least(8),
            ([.., b"ilst", _], b"mean" | b"name") => at_least(4),
            // stbl.rs:47-71.
            ([.., b"stbl"], b"stts" | b"stsc" | b"stco" | b"co64") => {
                at_least(8)?;
                let entry = match kind {
                    b"stts" | b"co64" => 8,
                    b"stsc" => 12,
                    _ => 4,
                };
                let count = u64::from(self.u32_at(found.start + 4)?);
                entries_fit(found, found.start + 8, count, entry)
            }
            ([.., b"stbl"], b"stsz") => {
                at_least(12)?;
                self.sample_sizes(found)
            }
            // traf.rs:42-46.
            ([b"moof", b"traf"], b"trun") => {
                at_least(8)?;
                self.fragment_run(found)
            }
            // `tfhd`: version and flags, track id, then the optional fields
            // its flags name, the default sample size after the base data
            // offset (8), the description index (4) and the duration (4).
            ([b"moof", b"traf"], b"tfhd") => {
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
            // `trex` (mvex.rs:41): version and flags, then track id,
            // description index, duration, and the default sample size.
            ([b"moov", b"mvex"], b"trex") => self.sample_size(found, found.start + 16),
            _ => Ok(()),
        }
    }

    /// The boxes in `start..end`, in order; at the `top` level, a box
    /// symphonia only seeks past may run past the file, and ends the list.
    fn boxes(&mut self, start: u64, end: u64, top: bool) -> Result<Vec<Mp4Box>, BoxError> {
        let mut found = Vec::new();
        let mut boxes = Boxes::new(&mut self.file, start, end);
        loop {
            match boxes.next_sized() {
                Ok(Some(next)) => found.push(next),
                Ok(None) => return Ok(found),
                Err(BoxError::Oversized { kind, .. }) if top && !PARSED_AT_TOP.contains(&&kind) => {
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
                // chunks.rs:97-124: rate, format, flags, then these.
                b"desc" if end - body >= 32 => {
                    let mut fields = [0u8; 16];
                    read_at(&mut self.file, body + 16, &mut fields)
                        .ok_or_else(|| "the description cannot be read".to_owned())?;
                    let field = |at: usize| {
                        u32::from_be_bytes(fields[at..at + 4].try_into().unwrap_or_default())
                    };
                    let (bytes, frames, bits) = (field(0), field(4), field(12));
                    self.packet_no_larger_than_the_file(u64::from(bytes))?;
                    // Uncompressed (chunks.rs:180): read 1 152 frames at a time.
                    if bits != 0 && u64::from(bytes) > MAX_PCM_FRAME {
                        return Err(format!(
                            "the description gives frames of {bytes} bytes, more than {MAX_PCM_FRAME}"
                        ));
                    }
                    packets = Some((bytes, frames));
                }
                b"pakt" if end - body >= 24 => {
                    let mut count = [0u8; 8];
                    read_at(&mut self.file, body, &mut count)
                        .ok_or_else(|| "the packet table cannot be read".to_owned())?;
                    let count = u64::try_from(i64::from_be_bytes(count)).unwrap_or(0);
                    // The varints of a packet (chunks.rs:436-480): its
                    // size where it varies, its frames where they vary.
                    let (sized, entry) = match packets {
                        Some((0, 0)) => (true, 2),
                        Some((0, _)) => (true, 1),
                        Some((_, 0)) => (false, 1),
                        // Constant: no entries (symphonia refuses a count),
                        // or no description yet (symphonia refuses that).
                        _ => (false, u64::MAX),
                    };
                    let room = end - body - 24;
                    if count > 0 && count.saturating_mul(entry) > room {
                        return Err(format!(
                            "the packet table counts {count} packets in {room} bytes"
                        ));
                    }
                    if sized {
                        self.packet_sizes(body + 24, count, entry == 2)?;
                    }
                }
                _ => {}
            }
            at = end;
        }
        Ok(())
    }

    /// The `count` packets of a CAF packet table from `from`, read as
    /// symphonia reads them (`chunks.rs:535-551`: up to nine bytes of seven
    /// bits, the size first, then the frames if `framed`): each no larger
    /// than the file.
    fn packet_sizes(&mut self, from: u64, count: u64, framed: bool) -> Result<(), String> {
        self.file
            .seek(SeekFrom::Start(from))
            .map_err(|_| "the packet table cannot be read".to_owned())?;
        let length = self.length;
        let mut bytes = BufReader::new(&mut self.file).bytes();
        let mut varint = || {
            let mut value = 0u64;
            for _ in 0..9 {
                let Some(Ok(byte)) = bytes.next() else {
                    return Err("the packet table runs past the file".to_owned());
                };
                value |= u64::from(byte & 0x7f);
                if byte & 0x80 == 0 {
                    return Ok(value);
                }
                value <<= 7;
            }
            Err("the packet table holds an unterminated size".to_owned())
        };
        for _ in 0..count {
            let size = varint()?;
            if size > length {
                return Err(format!(
                    "the packet table gives a packet of {size} bytes, more than the file's {length}"
                ));
            }
            if framed {
                varint()?;
            }
        }
        Ok(())
    }

    fn packet_no_larger_than_the_file(&self, size: u64) -> Result<(), String> {
        if size > self.length {
            return Err(format!(
                "the description gives packets of {size} bytes, more than the file's {}",
                self.length
            ));
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

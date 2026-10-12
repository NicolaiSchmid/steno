//! Salvage of a phone recording whose recorder was killed mid-recording: a
//! crash, a jetsam kill or a force-quit of the iOS app. Swift: none;
//! `AVAudioFile` fails on such a file ("missing moov atom"), as symphonia
//! does.
//!
//! `AVAudioRecorder` writes the `ftyp`, reserves the space the `moov` and
//! the `mdat` header take at the stop, and appends the AAC packets after
//! it; only the stop writes the index. A killed recorder leaves `ftyp`,
//! zeros, then the raw packets, with no `moov`, no `mdat` header and no
//! sample table, so nothing says where a packet ends. Measured on macOS 26
//! (`AVAudioRecorder` with the phone's settings killed after 4 and 30 s,
//! and `AVAudioFile`, the same encoder and file writer, killed mid-encode):
//! the 28-byte `ftyp` (`M4A `, then `M4A `, `mp42`, `isom`), zeros to byte
//! 57 344, where a finished file's `mdat` body starts, then whole packets,
//! byte for byte the finished file's. iOS was not measured.
//!
//! The salvage reads such a file as the phone's recorder wrote it, from its
//! fixed settings (`mobile/src/features/recorder/recording-options.ts`):
//! AAC-LC, mono, 44.1 kHz, 1 024 samples a packet. Each packet's length
//! comes from its own bits ([`packet_length`]), which matched the sample
//! table of every finished file of Apple's encoder at those settings
//! (101 375 packets). Only a file in the recorder's layout is salvaged: a
//! `ftyp` with major brand `M4A ` and `mp42` among its compatible brands
//! (the brands [`super::priming`] checks), no `moov` among the top-level
//! boxes, and packets that parse from the first; any other file stays
//! failed. Where the packets start is found, not assumed: past the zeros
//! (or the body of an `mdat` box, should a writer leave one), at the offset
//! from which [`START_RUN`] packets parse in a row.
//!
//! A packet that does not parse ends the salvage there, and so does the end
//! of the file, inside a packet cut off by the kill or after the last whole
//! one; the file itself is never written. A salvage that reads less than
//! half of the bytes after the start is no salvage
//! ([`SalvagedPackets::too_little`]), as a file with more than half of its
//! packets damaged is none ([`super::MAX_DAMAGED_SHARE`]).

pub mod packet;
mod tables;

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use symphonia::core::audio::Channels;
use symphonia::core::codecs::{CODEC_TYPE_AAC, CodecParameters};
use symphonia::core::formats::Packet;
use symphonia::core::units::TimeBase;

pub use packet::{MAX_PACKET_BYTES, PacketError, packet_length};

use super::priming::recorder_brands;

/// The phone recorder's rate, in hertz.
pub const SAMPLE_RATE: u32 = 44_100;

/// Samples in one AAC-LC packet.
pub const PACKET_FRAMES: u64 = 1_024;

/// The packets that must parse in a row from an offset for it to be where
/// the packets start (or every packet to the end of a shorter file).
pub const START_RUN: usize = 8;

/// The phone's `AudioSpecificConfig`, as its finished files' `esds` holds
/// it: AAC-LC, 44.1 kHz, one channel.
const AUDIO_SPECIFIC_CONFIG: [u8; 2] = [0x12, 0x08];

/// How far past the `ftyp` the zeros may run: the recorder reserves
/// 57 316 bytes; a megabyte leaves room for another writer's reserve.
const MAX_RESERVE: u64 = 1 << 20;

/// The bytes read from the file at a time.
const READ_BLOCK: usize = 64 * 1024;

/// The track id the packets carry.
pub const TRACK_ID: u32 = 0;

/// A killed phone recording's packets, one at a time, read straight from
/// the file.
#[derive(Debug)]
pub struct SalvagedPackets {
    file: File,
    /// Bytes from the file not yet handed out, from `buffer[at..]`.
    buffer: Vec<u8>,
    at: usize,
    /// The file offset of `buffer[0]`.
    buffer_offset: u64,
    /// Where the packets start, and the file's length.
    start: u64,
    end: u64,
    /// The packets handed out, and their bytes.
    packets: u64,
    read: u64,
    /// Set at the end: why the salvage stopped.
    stopped: Option<Stop>,
}

/// Why a salvage stopped reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// At the end of the file, after a last whole packet or inside one the
    /// kill cut off.
    End,
    /// At bytes that are not a packet, at this offset.
    Unreadable(u64),
}

impl SalvagedPackets {
    /// The packets of the file at `path` when it is laid out as the phone's
    /// recorder leaves a recording it was killed in (see the module doc);
    /// `None` for any other file, and for one with no packet at all.
    pub fn open(path: &Path) -> std::io::Result<Option<Self>> {
        let mut file = File::open(path)?;
        let end = file.metadata()?.len();
        let Some(region) = packet_region(&mut file, end)? else {
            return Ok(None);
        };
        let Some(start) = first_packet(&mut file, region, end)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            file,
            buffer: Vec::new(),
            at: 0,
            buffer_offset: start,
            start,
            end,
            packets: 0,
            read: 0,
            stopped: None,
        }))
    }

    /// The codec parameters of the phone's recorder: AAC-LC, mono,
    /// 44.1 kHz, its `AudioSpecificConfig`, timestamps in samples. The
    /// frame count is not known.
    #[must_use]
    pub fn codec_params() -> CodecParameters {
        let mut params = CodecParameters::new();
        params
            .for_codec(CODEC_TYPE_AAC)
            .with_sample_rate(SAMPLE_RATE)
            .with_channels(Channels::FRONT_LEFT)
            .with_time_base(TimeBase::new(1, SAMPLE_RATE))
            .with_max_frames_per_packet(PACKET_FRAMES)
            .with_extra_data(Box::new(AUDIO_SPECIFIC_CONFIG));
        params
    }

    /// The next packet, its timestamp its first sample; `None` once the
    /// salvage has stopped ([`stopped`](Self::stopped) says why).
    pub fn next_packet(&mut self) -> std::io::Result<Option<Packet>> {
        if self.stopped.is_some() {
            return Ok(None);
        }
        self.fill()?;
        let offset = self.buffer_offset + self.at as u64;
        let length = match packet_length(&self.buffer[self.at..]) {
            Ok(length) => length,
            Err(PacketError::Truncated) => {
                self.stopped = Some(Stop::End);
                return Ok(None);
            }
            Err(PacketError::Invalid(why)) => {
                // Zeros to the end are space no packet was written to, so
                // not counted as unread; anything else is a part of the
                // recording not read.
                if self.zeros_to_end()? {
                    self.end = offset;
                    self.stopped = Some(Stop::End);
                } else {
                    tracing::warn!(
                        offset,
                        packets = self.packets,
                        why,
                        "a salvaged recording holds bytes that are not a packet; it is read up to \
                         them"
                    );
                    self.stopped = Some(Stop::Unreadable(offset));
                }
                return Ok(None);
            }
        };
        let packet = Packet::new_from_boxed_slice(
            TRACK_ID,
            self.packets * PACKET_FRAMES,
            PACKET_FRAMES,
            Box::from(&self.buffer[self.at..self.at + length]),
        );
        self.at += length;
        self.packets += 1;
        self.read += length as u64;
        Ok(Some(packet))
    }

    /// Why the salvage stopped, once it has.
    #[must_use]
    pub fn stopped(&self) -> Option<Stop> {
        self.stopped
    }

    /// The packets handed out so far.
    #[must_use]
    pub fn packets(&self) -> u64 {
        self.packets
    }

    /// Whether the packets read hold less than half of the bytes from the
    /// first packet to the end of the file (less any zeros after the last
    /// packet, which no packet was written to): then this is no salvage.
    #[must_use]
    pub fn too_little(&self) -> bool {
        self.read.saturating_mul(2) < self.end - self.start
    }

    /// Keeps at least [`MAX_PACKET_BYTES`] unread in the buffer, or the
    /// rest of the file.
    fn fill(&mut self) -> std::io::Result<()> {
        if self.buffer.len() - self.at >= MAX_PACKET_BYTES {
            return Ok(());
        }
        self.buffer.drain(..self.at);
        self.buffer_offset += self.at as u64;
        self.at = 0;
        let next = self.buffer_offset + self.buffer.len() as u64;
        let want = usize::try_from((self.end - next).min(READ_BLOCK as u64)).unwrap_or(0);
        if want > 0 {
            let have = self.buffer.len();
            self.buffer.resize(have + want, 0);
            self.file.seek(SeekFrom::Start(next))?;
            self.file.read_exact(&mut self.buffer[have..])?;
        }
        Ok(())
    }

    /// Whether every byte from the current one to the end of the file is
    /// zero.
    fn zeros_to_end(&mut self) -> std::io::Result<bool> {
        let offset = self.buffer_offset + self.at as u64;
        Ok(first_non_zero(&mut self.file, offset, self.end)?.is_none())
    }
}

/// Where the packets of a file in the recorder's layout may start, from
/// its top-level boxes: past the `ftyp` and any `free`, `skip` or `wide`,
/// either the body of an `mdat` or, where the boxes stop, the zeros of the
/// reserve. `None` when the `ftyp` is not the recorder's or a `moov` is
/// there.
fn packet_region(file: &mut File, end: u64) -> std::io::Result<Option<Region>> {
    if end < 8 {
        return Ok(None);
    }
    let (ftyp_size, kind) = box_header(file, 0)?;
    if &kind != b"ftyp" || !(16..=256).contains(&ftyp_size) || ftyp_size > end {
        return Ok(None);
    }
    let mut ftyp = vec![0u8; usize::try_from(ftyp_size - 8).unwrap_or(0)];
    read_at(file, 8, &mut ftyp)?;
    if !recorder_brands(&ftyp) {
        return Ok(None);
    }
    let mut at = ftyp_size;
    // A few boxes at most precede the packets.
    for _ in 0..16 {
        if at + 8 > end {
            return Ok(Some(Region::Reserve(at)));
        }
        let (size, kind) = box_header(file, at)?;
        match &kind {
            b"moov" => return Ok(None),
            b"mdat" => {
                let body = if size == 1 { at + 16 } else { at + 8 };
                // A sized `mdat` may have a `moov` after it.
                if size >= 8
                    && at.saturating_add(size).saturating_add(8) <= end
                    && &box_header(file, at + size)?.1 == b"moov"
                {
                    return Ok(None);
                }
                return Ok(Some(Region::Mdat(body.min(end))));
            }
            b"free" | b"skip" | b"wide" if size >= 8 => at = at.saturating_add(size),
            _ => return Ok(Some(Region::Reserve(at))),
        }
    }
    Ok(None)
}

/// Where a file's packets may start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    /// The body of an `mdat`: the packets start there.
    Mdat(u64),
    /// Zeros from here, then the packets.
    Reserve(u64),
}

/// The offset of a file's first packet: an `mdat`'s body if its packets
/// parse from there; else, in the reserve, the first byte that is not zero
/// or one of the three before it (a packet may start with zero bytes:
/// Apple's first one does), whichever [`START_RUN`] packets parse from
/// first, nearest that byte first.
fn first_packet(file: &mut File, region: Region, end: u64) -> std::io::Result<Option<u64>> {
    let reserve = match region {
        Region::Mdat(body) => {
            return Ok(packets_parse_from(file, body, end)?.then_some(body));
        }
        Region::Reserve(at) => at,
    };
    let limit = end.min(reserve.saturating_add(MAX_RESERVE));
    let Some(first) = first_non_zero(file, reserve, limit)? else {
        return Ok(None);
    };
    for back in [1, 0, 2, 3] {
        let Some(start) = first.checked_sub(back).filter(|&start| start >= reserve) else {
            continue;
        };
        if packets_parse_from(file, start, end)? {
            return Ok(Some(start));
        }
    }
    Ok(None)
}

/// Whether [`START_RUN`] packets parse in a row from `start`, or at least
/// one and then every packet to the end of the file.
fn packets_parse_from(file: &mut File, start: u64, end: u64) -> std::io::Result<bool> {
    let length =
        usize::try_from((end - start).min((START_RUN * MAX_PACKET_BYTES) as u64)).unwrap_or(0);
    let mut bytes = vec![0u8; length];
    read_at(file, start, &mut bytes)?;
    let mut at = 0;
    for parsed in 0..START_RUN {
        match packet_length(&bytes[at..]) {
            Ok(length) => at += length,
            // The end of a short file, inside a packet the kill cut off.
            Err(PacketError::Truncated) => {
                return Ok(parsed > 0 && start + bytes.len() as u64 == end);
            }
            Err(PacketError::Invalid(_)) => return Ok(false),
        }
        if at == bytes.len() && start + at as u64 == end {
            return Ok(true);
        }
    }
    Ok(true)
}

/// The offset of the first byte from `from` to `limit` that is not zero,
/// `None` when they all are.
fn first_non_zero(file: &mut File, from: u64, limit: u64) -> std::io::Result<Option<u64>> {
    let mut block = vec![0u8; READ_BLOCK];
    let mut offset = from;
    while offset < limit {
        let length = usize::try_from((limit - offset).min(READ_BLOCK as u64)).unwrap_or(0);
        read_at(file, offset, &mut block[..length])?;
        if let Some(i) = block[..length].iter().position(|&b| b != 0) {
            return Ok(Some(offset + i as u64));
        }
        offset += length as u64;
    }
    Ok(None)
}

/// The size and the type of the box whose header is at `offset`.
fn box_header(file: &mut File, offset: u64) -> std::io::Result<(u64, [u8; 4])> {
    let mut header = [0u8; 8];
    read_at(file, offset, &mut header)?;
    let size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    Ok((
        u64::from(size),
        [header[4], header[5], header[6], header[7]],
    ))
}

fn read_at(file: &mut File, offset: u64, buffer: &mut [u8]) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apple's silent packet: a long window with no bands, then `END`.
    const SILENT: [u8; 4] = [0x00, 0xd0, 0x00, 0x07];

    fn parses_from(bytes: &[u8]) -> bool {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("packets");
        std::fs::write(&path, bytes).unwrap();
        let mut file = File::open(&path).unwrap();
        packets_parse_from(&mut file, 0, bytes.len() as u64).unwrap()
    }

    /// A start needs eight packets in a row ([`START_RUN`]), so an alignment that
    /// parses a few packets by chance is not taken; a short file needs
    /// every packet to its end, a last one cut off included, and at least
    /// one.
    #[test]
    fn a_start_needs_a_run_of_packets_or_the_end_of_the_file() {
        let junk = [0xff; 64];
        for whole in 1..8 {
            assert!(
                !parses_from(&[SILENT.repeat(whole).as_slice(), &junk].concat()),
                "{whole}"
            );
        }
        assert!(parses_from(&[SILENT.repeat(8).as_slice(), &junk].concat()));
        assert!(parses_from(&SILENT.repeat(3)));
        assert!(parses_from(
            &[SILENT.repeat(3).as_slice(), &SILENT[..2]].concat()
        ));
        assert!(!parses_from(&SILENT[..2]));
        assert!(!parses_from(&junk));
    }
}

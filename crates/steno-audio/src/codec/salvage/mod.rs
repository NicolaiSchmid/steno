//! Salvage of a phone recording whose recorder was killed mid-recording.
//! `AVAudioRecorder` leaves `ftyp`, a zeroed reserve, then raw AAC-LC
//! packets with no `moov` or sample table; each packet's length is read
//! from its own bits ([`packet_length`]) at the phone's fixed settings.
//! Any file not in that layout stays failed; the file is never written.

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

/// The phone recorder's rate, in hertz: `sampleRate` in
/// `mobile/src/features/recorder/recording-options.ts` (mono). The packets
/// cannot confirm it; `the_salvage_assumes_the_phones_preset` pins the
/// preset, and a change there must change the salvage too.
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

/// The top-level boxes looked at; a `moov` past them is not found.
const MAX_BOXES: usize = 16;

const READ_BLOCK: usize = 64 * 1024;

/// The track id the packets carry.
pub const TRACK_ID: u32 = 0;

/// A killed phone recording's packets, one at a time, read straight from
/// the file.
#[derive(Debug)]
pub struct SalvagedPackets {
    file: File,
    buffer: Vec<u8>,
    at: usize,
    buffer_offset: u64,
    /// Where the packets start, and where they end: the file's length, or
    /// where zeros that run to its end start.
    start: u64,
    end: u64,
    packets: u64,
    read: u64,
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
    /// The packets of the file at `path` when it is laid out as a killed
    /// phone recording; `None` for any other file or one with no packet.
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

    /// The codec parameters of the phone's recorder; the frame count is not
    /// known.
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
                // Zeros to the end are no part of the recording; anything
                // else is unread.
                if self.zeros_to_end()? {
                    self.end = offset;
                    self.stopped = Some(Stop::End);
                } else {
                    tracing::warn!(
                        offset,
                        packets = self.packets,
                        why,
                        "a salvaged recording holds bytes that are not a packet; it is read up to \
                         where they start"
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

    #[must_use]
    pub fn stopped(&self) -> Option<Stop> {
        self.stopped
    }

    #[must_use]
    pub fn packets(&self) -> u64 {
        self.packets
    }

    /// Whether the packets hold less than half of the bytes from the first
    /// packet to the end (trailing zeros aside): then this is no salvage.
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

    fn zeros_to_end(&mut self) -> std::io::Result<bool> {
        let offset = self.buffer_offset + self.at as u64;
        Ok(first_non_zero(&mut self.file, offset, self.end)?.is_none())
    }
}

/// Where the packets may start: past the `ftyp` and any `free`, `skip` or
/// `wide`, an `mdat` body or the reserve's zeros. `None` when the `ftyp`
/// is not the recorder's or a `moov` is among the boxes.
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
    for _ in 0..MAX_BOXES {
        if at + 8 > end {
            return Ok(Some(Region::Reserve(at)));
        }
        let (size, kind) = box_header(file, at)?;
        match &kind {
            b"moov" => return Ok(None),
            b"mdat" => {
                let Some((body, size)) = box_extent(file, at, size, end)? else {
                    return Ok(Some(Region::Mdat(end.min(at + 8))));
                };
                if moov_after(file, at.saturating_add(size), end)? {
                    return Ok(None);
                }
                return Ok(Some(Region::Mdat(body.min(end))));
            }
            b"free" | b"skip" | b"wide" if size >= 8 => at = at.saturating_add(size),
            _ if moov_after(file, at, end)? => return Ok(None),
            _ => return Ok(Some(Region::Reserve(at))),
        }
    }
    Ok(None)
}

/// The body's offset and whole size of the box at `at` with 32-bit size
/// field `size` (1: a 64-bit size follows, 0: to the end). `None` for a
/// size no box can have.
fn box_extent(
    file: &mut File,
    at: u64,
    size: u64,
    end: u64,
) -> std::io::Result<Option<(u64, u64)>> {
    Ok(match size {
        0 => Some((at + 8, end - at)),
        1 if at + 16 <= end => {
            let mut large = [0u8; 8];
            read_at(file, at + 8, &mut large)?;
            Some(u64::from_be_bytes(large))
                .filter(|&size| size >= 16)
                .map(|size| (at + 16, size))
        }
        1..8 => None,
        size => Some((at + 8, size)),
    })
}

/// Whether a `moov` is among the top-level boxes from `at`, walking at most
/// [`MAX_BOXES`]; an unreadable size ends the walk.
fn moov_after(file: &mut File, mut at: u64, end: u64) -> std::io::Result<bool> {
    for _ in 0..MAX_BOXES {
        if at.saturating_add(8) > end {
            return Ok(false);
        }
        let (size, kind) = box_header(file, at)?;
        if &kind == b"moov" {
            return Ok(true);
        }
        let Some((_, size)) = box_extent(file, at, size, end)? else {
            return Ok(false);
        };
        at = at.saturating_add(size);
    }
    Ok(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    /// The body of an `mdat`: the packets start there.
    Mdat(u64),
    /// Zeros from here, then the packets.
    Reserve(u64),
}

/// The offset of a file's first packet: an `mdat`'s body if its packets
/// parse from there; else, past the zeros, the first offset from which
/// [`START_RUN`] packets parse, trying one byte before the first non-zero
/// byte first (Apple's first packet starts with a zero), then the byte,
/// then two and three before.
fn first_packet(file: &mut File, region: Region, end: u64) -> std::io::Result<Option<u64>> {
    let reserve = match region {
        Region::Mdat(body) if packets_parse_from(file, body, end)? => return Ok(Some(body)),
        Region::Mdat(at) | Region::Reserve(at) => at,
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
/// one and then every packet to the end.
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

    fn region_of(bytes: &[u8]) -> Option<Region> {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("boxes.m4a");
        std::fs::write(&path, bytes).unwrap();
        let mut file = File::open(&path).unwrap();
        packet_region(&mut file, bytes.len() as u64).unwrap()
    }

    /// A box of type `kind` holding `body`, with a 32-bit size.
    fn mp4_box(kind: [u8; 4], body: &[u8]) -> Vec<u8> {
        let size = u32::try_from(body.len() + 8).unwrap();
        [size.to_be_bytes().as_slice(), &kind, body].concat()
    }

    /// An unknown box ends the walk where the reserve would start, but a
    /// `moov` after it still refuses the file.
    #[test]
    fn a_moov_after_an_unknown_box_refuses_the_file() {
        let ftyp = mp4_box(*b"ftyp", b"M4A \0\0\0\0M4A mp42isom");
        let uuid = mp4_box(*b"uuid", &[0x5a; 24]);
        let mdat = mp4_box(*b"mdat", &SILENT.repeat(16));
        let moov = mp4_box(*b"moov", &[0xff; 8]);
        let at = ftyp.len() as u64;
        assert_eq!(
            region_of(&[ftyp.as_slice(), &uuid, &mdat].concat()),
            Some(Region::Reserve(at))
        );
        assert_eq!(
            region_of(&[ftyp.as_slice(), &uuid, &mdat, &moov].concat()),
            None
        );
    }

    fn parses_from(bytes: &[u8]) -> bool {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("packets");
        std::fs::write(&path, bytes).unwrap();
        let mut file = File::open(&path).unwrap();
        packets_parse_from(&mut file, 0, bytes.len() as u64).unwrap()
    }

    /// A start needs [`START_RUN`] packets in a row; a short file needs
    /// every packet to its end, and at least one.
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

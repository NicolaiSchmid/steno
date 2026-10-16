//! The sizes symphonia allocates from, checked before its demuxer opens a
//! file (`codec::sizes`, A17 of `.plans/2026-10-07-stable-promotion.md`).
//! Every case measured or crafted to abort the process on Linux is refused
//! as an ordinary error; every fixture, and every healthy variant a writer
//! makes that symphonia reads, still reads; and a fuzz pass that sets the
//! size and count fields of the fixtures' boxes, chunks and tables to
//! random and boundary values aborts nothing. On Linux the fuzz runs in a
//! child of this binary whose address space is held to 8 GiB, a machine
//! that cannot back the allocations the measurement found. Without the
//! check, the first cases below abort this test binary.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use steno_audio::{CodecError, SymphoniaAudioCodec};
use steno_core::AudioLane;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../Tests/Fixtures/audio")
        .join(name)
}

/// The phone recorder's finished file: `ftyp`, `moov` (one track), `mdat`.
const PHONE: &str = "silence-44k1-avaudiorecorder.m4a";

/// A box found in an MP4 file: its path of types, where its header
/// starts, the header's length, and where it ends.
#[derive(Debug, Clone)]
struct Found {
    path: Vec<[u8; 4]>,
    at: usize,
    header: usize,
    end: usize,
}

fn be32(bytes: &[u8], at: usize) -> u64 {
    u64::from(u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()))
}

/// The box at `at` before `end`: its type, header length and size.
fn header_at(bytes: &[u8], at: usize, end: usize) -> Option<([u8; 4], usize, usize)> {
    if at + 8 > end {
        return None;
    }
    let kind: [u8; 4] = bytes[at + 4..at + 8].try_into().unwrap();
    let (header, size) = match be32(bytes, at) {
        0 => (8, end - at),
        1 if at + 16 <= end => (
            16,
            usize::try_from(u64::from_be_bytes(
                bytes[at + 8..at + 16].try_into().unwrap(),
            ))
            .ok()?,
        ),
        1 => return None,
        size => (8, usize::try_from(size).ok()?),
    };
    (size >= header && at.checked_add(size)? <= end).then_some((kind, header, size))
}

/// `start..end` holds nothing but well-formed boxes of printable types:
/// a body the fuzz reads as boxes, whatever the walk makes of it.
fn holds_boxes(bytes: &[u8], mut at: usize, end: usize) -> bool {
    let printable = |kind: [u8; 4]| {
        kind.iter()
            .all(|b| b.is_ascii_graphic() || *b == b' ' || *b == 0xa9)
    };
    if at >= end {
        return false;
    }
    while at < end {
        match header_at(bytes, at, end) {
            Some((kind, _, size)) if be32(bytes, at) != 0 && printable(kind) => at += size,
            _ => return false,
        }
    }
    true
}

/// Every box of `bytes`, depth first: below any box whose body (from 0, 4
/// or 8 bytes in, or after an audio sample entry's fields of 28, 44 or 64
/// bytes) is a run of well-formed boxes, so the fuzz reaches more than the
/// walk follows.
fn boxes(bytes: &[u8]) -> Vec<Found> {
    fn walk(bytes: &[u8], mut at: usize, end: usize, path: &[[u8; 4]], out: &mut Vec<Found>) {
        while let Some((kind, header, size)) = header_at(bytes, at, end) {
            let mut inner = path.to_vec();
            inner.push(kind);
            out.push(Found {
                path: inner.clone(),
                at,
                header,
                end: at + size,
            });
            let body = at + header;
            if &kind != b"mdat"
                && let Some(skip) = [0, 4, 8, 28, 44, 64]
                    .into_iter()
                    .find(|skip| holds_boxes(bytes, body + skip, at + size))
            {
                walk(bytes, body + skip, at + size, &inner, out);
            }
            at += size;
        }
    }
    let mut out = Vec::new();
    walk(bytes, 0, bytes.len(), &[], &mut out);
    out
}

/// The box whose path ends in `path`.
fn find_at(bytes: &[u8], path: &[&[u8; 4]]) -> Found {
    boxes(bytes)
        .into_iter()
        .find(|found| {
            found.path.len() >= path.len()
                && found.path[found.path.len() - path.len()..]
                    .iter()
                    .zip(path)
                    .all(|(a, b)| a == *b)
        })
        .unwrap_or_else(|| panic!("no {path:?} box"))
}

fn find(bytes: &[u8], kind: [u8; 4]) -> Found {
    find_at(bytes, &[&kind])
}

fn set32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

/// `bytes` with `range` replaced by `new`: the boxes in `holders`, which hold
/// the range, take the difference in size, and so do the chunk offsets past
/// it, as a writer that moved the media data would write them.
fn splice(bytes: &[u8], range: std::ops::Range<usize>, new: &[u8], holders: &[Found]) -> Vec<u8> {
    let delta = i64::try_from(new.len()).unwrap() - i64::try_from(range.len()).unwrap();
    let moved = |value: u64| {
        if value >= range.start as u64 {
            u64::try_from(i64::try_from(value).unwrap() + delta).unwrap()
        } else {
            value
        }
    };
    let mut out = bytes.to_vec();
    for parent in holders {
        assert_eq!(parent.header, 8, "{:?}", parent.path);
        let size = moved(parent.at as u64 + be32(bytes, parent.at)) - parent.at as u64;
        set32(&mut out, parent.at, u32::try_from(size).unwrap());
    }
    out.splice(range.clone(), new.iter().copied());
    // The movie's tables, not one the fuzz's walk finds in a `free` box.
    for table in boxes(&out)
        .into_iter()
        .filter(|table| table.path[0] == *b"moov")
    {
        let width = match table.path.last() {
            Some(b"stco") => 4,
            Some(b"co64") => 8,
            _ => continue,
        };
        let body = table.at + table.header;
        let count = usize::try_from(be32(&out, body + 4)).unwrap();
        for index in 0..count.min((table.end - body - 8) / width) {
            let at = body + 8 + index * width;
            let mut value = [0u8; 8];
            value[8 - width..].copy_from_slice(&out[at..at + width]);
            let shifted = moved(u64::from_be_bytes(value)).to_be_bytes();
            out[at..at + width].copy_from_slice(&shifted[8 - width..]);
        }
    }
    out
}

/// The boxes of `bytes` that hold `found`, outermost first.
fn ancestors(bytes: &[u8], found: &Found) -> Vec<Found> {
    boxes(bytes)
        .into_iter()
        .filter(|outer| {
            outer.path.len() < found.path.len()
                && found.path.starts_with(&outer.path)
                && outer.at < found.at
                && found.end <= outer.end
        })
        .collect()
}

/// `child` appended to `moov`, whose size grows to hold it; the chunk
/// offsets stay, so the file is for the walk, not for a decode.
fn with_moov_child(bytes: &[u8], child: &[u8]) -> Vec<u8> {
    let moov = find(bytes, *b"moov");
    let mut grown = bytes.to_vec();
    let size = u32::try_from(moov.end - moov.at + child.len()).unwrap();
    set32(&mut grown, moov.at, size);
    grown.splice(moov.end..moov.end, child.iter().copied());
    grown
}

/// `child` appended to the box at `path`, as a writer would put it there:
/// the box and its ancestors grow, and the chunk offsets move.
fn with_child_at(bytes: &[u8], path: &[&[u8; 4]], child: &[u8]) -> Vec<u8> {
    let parent = find_at(bytes, path);
    let mut grow = ancestors(bytes, &parent);
    grow.push(parent.clone());
    splice(bytes, parent.end..parent.end, child, &grow)
}

/// The box at `path` replaced by `with`, its ancestors resized.
fn with_box_at(bytes: &[u8], path: &[&[u8; 4]], with: &[u8]) -> Vec<u8> {
    let found = find_at(bytes, path);
    splice(bytes, found.at..found.end, with, &ancestors(bytes, &found))
}

/// `boxes` in front of the file, its chunk offsets moved past them.
fn led_by(bytes: &[u8], lead: &[u8]) -> Vec<u8> {
    splice(bytes, 0..0, lead, &[])
}

fn atom(kind: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(8 + body.len())
        .unwrap()
        .to_be_bytes()
        .to_vec();
    out.extend_from_slice(&kind);
    out.extend_from_slice(body);
    out
}

/// `meta` as iTunes writes it, a full box with a `mdir` handler, around
/// `ilst`.
fn itunes_meta(ilst_body: &[u8]) -> Vec<u8> {
    let mut hdlr_body = vec![0u8; 8];
    hdlr_body.extend_from_slice(b"mdirappl");
    hdlr_body.extend_from_slice(&[0; 9]);
    let mut meta_body = vec![0u8; 4];
    meta_body.extend_from_slice(&atom(*b"hdlr", &hdlr_body));
    meta_body.extend_from_slice(&atom(*b"ilst", ilst_body));
    atom(*b"meta", &meta_body)
}

/// `udta`/`meta`/`ilst`/`©nam` around `data`, as iTunes writes a tag.
fn tag_with(data: &[u8]) -> Vec<u8> {
    atom(*b"udta", &itunes_meta(&atom(*b"\xa9nam", data)))
}

/// `QuickTime`'s `meta`, with no version and flags: a `mdta` handler, its
/// keys, and an `ilst` whose item is a key's index.
fn quicktime_meta() -> Vec<u8> {
    let mut hdlr_body = vec![0u8; 8];
    hdlr_body.extend_from_slice(b"mdta");
    hdlr_body.extend_from_slice(&[0; 13]);
    let key = b"com.apple.quicktime.title";
    let mut keys_body = vec![0, 0, 0, 0, 0, 0, 0, 1];
    keys_body.extend_from_slice(&u32::try_from(8 + key.len()).unwrap().to_be_bytes());
    keys_body.extend_from_slice(b"mdta");
    keys_body.extend_from_slice(key);
    let mut data_body = vec![0, 0, 0, 1, 0, 0, 0, 0];
    data_body.extend_from_slice(b"hello");
    let item = atom([0, 0, 0, 1], &atom(*b"data", &data_body));
    let mut body = atom(*b"hdlr", &hdlr_body);
    body.extend_from_slice(&atom(*b"keys", &keys_body));
    body.extend_from_slice(&atom(*b"ilst", &item));
    atom(*b"meta", &body)
}

/// A `data` atom with a 64-bit size of 2^`exponent` and a few bytes.
fn huge_data(exponent: u32) -> Vec<u8> {
    let mut data = 1u32.to_be_bytes().to_vec();
    data.extend_from_slice(b"data");
    data.extend_from_slice(&(1u64 << exponent).to_be_bytes());
    data.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0, b'x']);
    data
}

/// The phone file with its chunk offsets in a `co64` box, as a writer of
/// a file past 4 GiB lays them out.
fn co64_phone() -> Vec<u8> {
    let phone = std::fs::read(fixture(PHONE)).unwrap();
    let stco = find(&phone, *b"stco");
    let body = stco.at + stco.header;
    let count = usize::try_from(be32(&phone, body + 4)).unwrap();
    let mut co64 = phone[body..body + 8].to_vec();
    for index in 0..count {
        co64.extend_from_slice(&be32(&phone, body + 8 + 4 * index).to_be_bytes());
    }
    with_box_at(&phone, &[b"stco"], &atom(*b"co64", &co64))
}

/// A variable-length integer as a CAF packet table writes it.
fn varint(mut value: u64) -> Vec<u8> {
    let mut out = vec![u8::try_from(value & 0x7f).unwrap()];
    value >>= 7;
    while value > 0 {
        out.push(0x80 | u8::try_from(value & 0x7f).unwrap());
        value >>= 7;
    }
    out.reverse();
    out
}

/// A CAF: the header, then `(type, body, declared size)` chunks.
fn caf(chunks: &[(&[u8; 4], Vec<u8>, Option<i64>)]) -> Vec<u8> {
    let mut out = b"caff".to_vec();
    out.extend_from_slice(&[0, 1, 0, 0]);
    for (kind, body, declared) in chunks {
        out.extend_from_slice(*kind);
        let size = declared.unwrap_or(i64::try_from(body.len()).unwrap());
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(body);
    }
    out
}

/// A CAF description: `rate`, `format`, flags, bytes and frames per
/// packet, channels, bits.
fn desc(format: [u8; 4], flags: u32, bytes: u32, frames: u32, bits: u32) -> Vec<u8> {
    let mut out = 16_000f64.to_be_bytes().to_vec();
    out.extend_from_slice(&format);
    for value in [flags, bytes, frames, 1, bits] {
        out.extend_from_slice(&value.to_be_bytes());
    }
    out
}

/// 16-bit PCM, which symphonia's CAF reader (not the crate's Float32
/// reader) reads: 0.1 s of a ramp.
fn pcm_caf() -> Vec<u8> {
    let mut data = vec![0u8; 4];
    for index in 0..1_600u16 {
        data.extend_from_slice(&(index.wrapping_mul(37)).to_be_bytes());
    }
    caf(&[
        (b"desc", desc(*b"lpcm", 2, 2, 1, 16), None),
        (b"data", data, None),
    ])
}

/// Big-endian Float32 PCM, which symphonia's CAF reader reads too (the
/// crate's own reads only the little-endian Float32 Steno writes): 0.1 s
/// of a ramp.
fn float_caf() -> Vec<u8> {
    let mut data = vec![0u8; 4];
    for index in 0..1_600u16 {
        data.extend_from_slice(&(f32::from(index) / 1_600.0).to_be_bytes());
    }
    caf(&[
        (b"desc", desc(*b"lpcm", 1, 4, 1, 32), None),
        (b"data", data, None),
    ])
}

/// An AAC CAF in shape (its packets are noise): a magic cookie and a
/// packet table, as `AVFoundation` writes AAC in CAF: one-byte sizes, or
/// with `two_varints`, each packet's size and its frames (varying both).
fn aac_caf_with(two_varints: bool) -> Vec<u8> {
    let packets = 20u8;
    let mut pakt = i64::from(packets).to_be_bytes().to_vec();
    pakt.extend_from_slice(&(i64::from(packets) * 1024).to_be_bytes());
    pakt.extend_from_slice(&[0; 8]);
    let mut data = vec![0u8; 4];
    for packet in 0..packets {
        pakt.push(6);
        if two_varints {
            pakt.extend_from_slice(&varint(1024));
        }
        data.extend_from_slice(&[0x21, packet, 0x49, 0x90, 0x02, 0x19]);
    }
    let frames = if two_varints { 0 } else { 1024 };
    caf(&[
        (b"desc", desc(*b"aac ", 0, 0, frames, 0), None),
        (b"kuki", vec![0x12, 0x08], None),
        (b"pakt", pakt, None),
        (b"data", data, None),
    ])
}

fn aac_caf() -> Vec<u8> {
    aac_caf_with(false)
}

/// A CAF packet table of one packet, given as `entry`, before 24 bytes of
/// noise in the `data` (open with `-1` when `open`).
fn one_packet_caf(frames: u32, entry: &[u8], open: bool) -> Vec<u8> {
    let mut pakt = 1i64.to_be_bytes().to_vec();
    pakt.extend_from_slice(&1024i64.to_be_bytes());
    pakt.extend_from_slice(&[0; 8]);
    pakt.extend_from_slice(entry);
    let mut data = vec![0u8; 4];
    data.extend_from_slice(&[0x21, 0x00, 0x49, 0x90, 0x02, 0x19].repeat(4));
    caf(&[
        (b"desc", desc(*b"aac ", 0, 0, frames, 0), None),
        (b"kuki", vec![0x12, 0x08], None),
        (b"pakt", pakt, None),
        (b"data", data, open.then_some(-1)),
    ])
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// The refusal, as every path that opens the file with symphonia sees it:
/// the declared length, the decode and the mixdown.
fn refused(path: &Path) -> String {
    let duration = SymphoniaAudioCodec::declared_duration(path).unwrap_err();
    let decode = SymphoniaAudioCodec::read_channel(path, 0, AudioLane::Mixed).unwrap_err();
    let mixdown =
        SymphoniaAudioCodec::mixdown_path(path, &path.with_extension("mix.wav")).unwrap_err();
    for error in [&decode, &mixdown] {
        assert_eq!(error.to_string(), duration.to_string());
    }
    assert!(
        matches!(duration, CodecError::UnsupportedFormat(_)),
        "{duration}"
    );
    duration.to_string()
}

/// The file reads, or fails as symphonia fails on it, never by the walk.
fn not_refused(path: &Path) {
    for error in [
        SymphoniaAudioCodec::declared_duration(path).err(),
        SymphoniaAudioCodec::read_channel(path, 0, AudioLane::Mixed).err(),
    ]
    .into_iter()
    .flatten()
    {
        let text = error.to_string();
        for walk in [
            "declares",
            "fewer than",
            "entries of",
            "more than the file",
            "packet",
        ] {
            assert!(!text.contains(walk), "{}: {text}", path.display());
        }
    }
}

/// The phone's file with the 1 TiB tag and with an `stco` counting
/// 2^32 - 1 entries: the two cases measured to abort, as the prefixes and
/// wrappers below carry them.
fn measured_bad() -> [(&'static str, Vec<u8>); 2] {
    let phone = std::fs::read(fixture(PHONE)).unwrap();
    let stco = find(&phone, *b"stco");
    let mut table = phone.clone();
    set32(&mut table, stco.at + stco.header + 4, u32::MAX);
    [
        ("ilst", with_moov_child(&phone, &tag_with(&huge_data(40)))),
        ("stco", table),
    ]
}

/// The cases the measurement found aborting on Linux, and those the review
/// crafted, each refused before symphonia allocates: a 1 TiB and a 64 GiB
/// `ilst` `data` atom, the sample tables counting 2^32 - 1 entries, a
/// sample larger than the file, the CAF magic cookie of 1 TiB, packet
/// tables of 2^32 and 2^40 packets or with a packet of 2^40 or 2^50 bytes,
/// and packets or uncompressed frames of 2^28 and 2^31 bytes.
#[test]
#[allow(clippy::too_many_lines)]
fn every_size_measured_to_abort_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let phone = std::fs::read(fixture(PHONE)).unwrap();
    let mut cases: Vec<(String, Vec<u8>, &str)> = Vec::new();
    for exponent in [40, 36] {
        cases.push((
            "ilst-data.m4a".into(),
            with_moov_child(&phone, &tag_with(&huge_data(exponent))),
            "the `data` box",
        ));
    }
    for (kind, offset) in [(b"stts", 4), (b"stsc", 4), (b"stco", 4), (b"stsz", 8)] {
        let found = find(&phone, *kind);
        let mut bytes = phone.clone();
        if kind == b"stsz" {
            set32(&mut bytes, found.at + found.header + 4, 0);
        }
        set32(&mut bytes, found.at + found.header + offset, u32::MAX);
        cases.push(("table.m4a".into(), bytes, "entries"));
    }
    let stsz = find(&phone, *b"stsz");
    let mut big_sample = phone.clone();
    set32(&mut big_sample, stsz.at + stsz.header + 12, u32::MAX);
    cases.push(("sample.m4a".into(), big_sample, "more than the file's"));
    cases.push((
        "kuki.caf".into(),
        caf(&[
            (b"desc", desc(*b"aac ", 0, 0, 1024, 0), None),
            (b"kuki", vec![0; 8], Some(1 << 40)),
            (b"data", vec![0; 8], None),
        ]),
        "the `kuki` chunk",
    ));
    for exponent in [32, 40] {
        let mut pakt = (1i64 << exponent).to_be_bytes().to_vec();
        pakt.extend_from_slice(&[0; 16]);
        cases.push((
            "pakt.caf".into(),
            caf(&[
                (b"desc", desc(*b"aac ", 0, 0, 1024, 0), None),
                (b"pakt", pakt, None),
                (b"data", vec![0; 8], None),
            ]),
            "the packet table counts",
        ));
    }
    // A packet's size in the table: its frames constant, or varying too.
    for exponent in [40, 50] {
        for open in [false, true] {
            cases.push((
                format!("packet-2e{exponent}.caf"),
                one_packet_caf(1024, &varint(1 << exponent), open),
                "gives a packet of",
            ));
            let mut framed = varint(1 << exponent);
            framed.extend_from_slice(&varint(1024));
            cases.push((
                format!("framed-packet-2e{exponent}.caf"),
                one_packet_caf(0, &framed, open),
                "gives a packet of",
            ));
        }
    }
    // Packets of a constant size larger than the file, and uncompressed
    // frames of such a size (symphonia reads 1 152 of them at once), with
    // the audio closed, open (-1) or declared past the file's end.
    let samples = [0u8; 484].to_vec();
    for exponent in [28, 31] {
        let bytes = 1u32 << exponent;
        // Big-endian Float32 and 16-bit PCM; the crate's own reader takes
        // little-endian Float32, and reads no packet size.
        for (flags, bits) in [(1, 32), (2, 16)] {
            for declared in [None, Some(-1), Some(1 << 50)] {
                cases.push((
                    format!("lpcm-{flags}-2e{exponent}.caf"),
                    caf(&[
                        (b"desc", desc(*b"lpcm", flags, bytes, 1, bits), None),
                        (b"data", samples.clone(), declared),
                    ]),
                    "gives packets of",
                ));
            }
        }
    }
    let mut pakt = 1i64.to_be_bytes().to_vec();
    pakt.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
    pakt.extend_from_slice(&[0; 8]);
    pakt.push(1);
    cases.push((
        "alac.caf".into(),
        caf(&[
            (b"desc", desc(*b"alac", 0, u32::MAX, 0, 0), None),
            (b"kuki", vec![0; 24], None),
            (b"pakt", pakt, None),
            (b"data", samples.clone(), None),
        ]),
        "gives packets of",
    ));
    // Within the file, but 8 KiB frames: 9 MiB a read, of a file that
    // could hold none.
    cases.push((
        "wide-frames.caf".into(),
        caf(&[
            (b"desc", desc(*b"lpcm", 1, 8_192, 1, 32), None),
            (b"data", vec![0; 9_000], Some(-1)),
        ]),
        "frames of 8192 bytes",
    ));
    cases.push((
        "short-data.m4a".into(),
        with_moov_child(&phone, &tag_with(&atom(*b"data", &[0; 4]))),
        "fewer than its 8",
    ));
    for (index, (name, bytes, says)) in cases.iter().enumerate() {
        let path = write(dir.path(), &format!("{index}-{name}"), bytes);
        let error = refused(&path);
        assert!(error.contains(says), "{name}: {error}");
    }
}

/// Symphonia's own probe scans the first MiB for an `ftyp` and reads from
/// there, past junk or an ID3 tag, or into a `free` box. The walk starts at
/// byte 0, and so does the MP4 reader; a file that does not start with
/// well-formed boxes never reaches the MP4 reader, and a movie hidden in a
/// leading box is never read.
#[test]
fn a_movie_the_walk_did_not_see_is_never_read() {
    let dir = tempfile::tempdir().unwrap();
    let phone = std::fs::read(fixture(PHONE)).unwrap();
    let id3 = [b"ID3".as_slice(), &[4, 0, 0, 0, 0, 0, 10], &[0; 10]].concat();
    for (label, bad) in measured_bad() {
        let mdat = find(&bad, *b"mdat");
        let hidden = bad[..mdat.at].to_vec();
        for (prefix, lead) in [
            ("junk8", b"JUNKJUNK".to_vec()),
            ("junk3", vec![1, 2, 3]),
            ("id3", id3.clone()),
            ("free", atom(*b"free", &hidden)),
            ("skip", atom(*b"skip", &hidden)),
        ] {
            let alone = [lead.as_slice(), &bad].concat();
            let path = write(dir.path(), &format!("{prefix}-{label}.m4a"), &alone);
            refused(&path);
        }
        // A fake movie in a leading box, then a healthy file: the reader
        // seeks past the box, and reads the healthy one.
        for kind in [*b"free", *b"skip", *b"wide"] {
            let led = led_by(&phone, &atom(kind, &hidden));
            let path = write(dir.path(), &format!("hidden-{label}.m4a"), &led);
            let read = SymphoniaAudioCodec::read_channel(&path, 0, AudioLane::Mixed).unwrap();
            assert_ne!(read.samples.len(), 0);
        }
    }
}

/// A box larger than the one holding it and one shorter than its header
/// are refused too, and so is a chunk running past the file.
#[test]
fn a_box_or_chunk_that_does_not_fit_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let phone = std::fs::read(fixture(PHONE)).unwrap();
    let stbl = find(&phone, *b"stbl");
    let stco = find(&phone, *b"stco");
    let mut past_parent = phone.clone();
    set32(
        &mut past_parent,
        stco.at,
        u32::try_from(stbl.end - stco.at + 8).unwrap(),
    );
    let error = refused(&write(dir.path(), "past-parent.m4a", &past_parent));
    assert!(error.contains("more than `stbl` holds"), "{error}");

    let mut short = phone.clone();
    set32(&mut short, stco.at, 4);
    let error = refused(&write(dir.path(), "short.m4a", &short));
    assert!(error.contains("`stco` box"), "{error}");

    let past_file = caf(&[
        (b"desc", desc(*b"lpcm", 2, 2, 1, 16), None),
        (b"chan", vec![0; 12], Some(1 << 20)),
        (b"data", vec![0; 8], None),
    ]);
    let error = refused(&write(dir.path(), "past-file.caf", &past_file));
    assert!(error.contains("the `chan` chunk"), "{error}");
}

/// Each fixed header symphonia subtracts from a box's length unchecked,
/// where it reads the box: `hdlr` in `mdia`, `meta` at the top level and
/// in `moov/udta`, and an `ilst` item's `mean`, `name` and `data`.
#[test]
fn a_box_shorter_than_the_header_symphonia_takes_off_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let phone = std::fs::read(fixture(PHONE)).unwrap();
    let short_hdlr = with_box_at(&phone, &[b"mdia", b"hdlr"], &atom(*b"hdlr", &[0; 20]));
    let mut cases = vec![(
        "hdlr",
        short_hdlr,
        "`hdlr` box holds 20 bytes, fewer than its 24",
    )];
    for kind in [*b"mean", *b"name"] {
        let item = atom(*b"----", &atom(kind, &[0; 3]));
        cases.push((
            "item",
            with_moov_child(&phone, &atom(*b"udta", &itunes_meta(&item))),
            "holds 3 bytes, fewer than its 4",
        ));
    }
    cases.push((
        "udta-meta",
        with_moov_child(&phone, &atom(*b"udta", &atom(*b"meta", &[0; 3]))),
        "`meta` box holds 3 bytes, fewer than its 4",
    ));
    cases.push((
        "top-meta",
        [phone.as_slice(), &atom(*b"meta", &[0; 2])].concat(),
        "`meta` box holds 2 bytes, fewer than its 4",
    ));
    for (name, bytes, says) in cases {
        let error = refused(&write(dir.path(), &format!("{name}.m4a"), &bytes));
        assert!(error.contains(says), "{name}: {error}");
    }
}

/// The sample sizes a fragmented file gives, in a run (`trun`), as a
/// fragment's default (`tfhd`) or a track's (`trex`), and a constant
/// `stsz` size: each larger than the file is refused, one as large is not.
/// A `co64` table counting one entry more than it holds is refused.
#[test]
fn every_sample_size_and_offset_table_is_checked() {
    let dir = tempfile::tempdir().unwrap();
    let fragmented = std::fs::read(fixture("tone-440-44k1-500ms-fragmented.m4a")).unwrap();
    let length = u32::try_from(fragmented.len()).unwrap();
    let flags = |bytes: &[u8], found: &Found| be32(bytes, found.at + found.header) & 0x00ff_ffff;
    let trun = find(&fragmented, *b"trun");
    let trun_flags = flags(&fragmented, &trun);
    assert_ne!(trun_flags & 0x200, 0, "the fixture's runs give each size");
    let mut first = trun.at + trun.header + 8;
    first += 4 * usize::from(trun_flags & 0x1 != 0) + 4 * usize::from(trun_flags & 0x4 != 0);
    let size_in_entry = 4 * usize::from(trun_flags & 0x100 != 0);
    let tfhd = find(&fragmented, *b"tfhd");
    let tfhd_flags = flags(&fragmented, &tfhd);
    assert_ne!(
        tfhd_flags & 0x10,
        0,
        "the fixture's fragments give a default size"
    );
    let mut default = tfhd.at + tfhd.header + 8;
    for (flag, bytes) in [(0x1, 8), (0x2, 4), (0x8, 4)] {
        if tfhd_flags & flag != 0 {
            default += bytes;
        }
    }
    let trex = find(&fragmented, *b"trex");
    let phone = std::fs::read(fixture(PHONE)).unwrap();
    let stsz = find(&phone, *b"stsz");
    for (name, original, at, says) in [
        (
            "trun",
            &fragmented,
            first + size_in_entry,
            "`trun` box gives a sample",
        ),
        ("tfhd", &fragmented, default, "`tfhd` box gives a sample"),
        (
            "trex",
            &fragmented,
            trex.at + trex.header + 16,
            "`trex` box gives a sample",
        ),
        (
            "stsz",
            &phone,
            stsz.at + stsz.header + 4,
            "`stsz` box gives a sample",
        ),
    ] {
        let length = u32::try_from(original.len()).unwrap();
        let mut bytes = original.clone();
        set32(&mut bytes, at, length + 1);
        let error = refused(&write(dir.path(), &format!("{name}-past.m4a"), &bytes));
        assert!(error.contains(says), "{name}: {error}");
        set32(&mut bytes, at, length);
        not_refused(&write(dir.path(), &format!("{name}-whole.m4a"), &bytes));
    }
    assert!(length > 0);

    let co64 = co64_phone();
    let table = find(&co64, *b"co64");
    let entries = (table.end - table.at - table.header - 8) / 8;
    let mut one_more = co64.clone();
    set32(
        &mut one_more,
        table.at + table.header + 4,
        u32::try_from(entries + 1).unwrap(),
    );
    let error = refused(&write(dir.path(), "co64.m4a", &one_more));
    assert!(error.contains("entries of 8 bytes"), "{error}");
}

/// Media data cut off by the file's end is no allocation, so the check
/// lets it through: a finished file whose `mdat` is cut short, a CAF whose
/// `data` runs to the end (size -1) or past it.
#[test]
fn media_data_cut_off_still_reads() {
    let dir = tempfile::tempdir().unwrap();
    let phone = std::fs::read(fixture(PHONE)).unwrap();
    let mdat = find(&phone, *b"mdat");
    let mut cut = phone.clone();
    set32(
        &mut cut,
        mdat.at,
        u32::try_from(mdat.end - mdat.at + 4_096).unwrap(),
    );
    let pcm = pcm_caf();
    // The `data` chunk's header: its body is the edit count and 1 600
    // samples of two bytes.
    let data = pcm.len() - 3_204 - 12;
    let mut open = pcm.clone();
    open[data + 4..data + 12].copy_from_slice(&(-1i64).to_be_bytes());
    let mut past = pcm;
    past[data + 4..data + 12].copy_from_slice(&(1i64 << 20).to_be_bytes());
    for (name, bytes) in [("cut.m4a", cut), ("open.caf", open), ("past.caf", past)] {
        not_refused(&write(dir.path(), name, &bytes));
    }
}

/// A final top-level box that runs past the file's end is let through when
/// symphonia only seeks past it, and refused when it parses it. Symphonia
/// 0.5.5 itself fails on either ("end of stream", as it reads up to where
/// the box would end), so the walk turns no readable file away.
#[test]
fn a_final_box_cut_off_is_refused_only_where_symphonia_parses_it() {
    let dir = tempfile::tempdir().unwrap();
    let phone = std::fs::read(fixture(PHONE)).unwrap();
    let cut = |kind: &[u8; 4]| {
        let mut bytes = phone.clone();
        bytes.extend_from_slice(&4_096u32.to_be_bytes());
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(&[0; 100]);
        bytes
    };
    for kind in [b"free", b"skip", b"wide", b"mdat", b"uuid", b"mfra"] {
        not_refused(&write(dir.path(), "skipped.m4a", &cut(kind)));
    }
    for kind in [b"ftyp", b"moov", b"sidx", b"meta", b"moof"] {
        let error = refused(&write(dir.path(), "parsed.m4a", &cut(kind)));
        assert!(
            error.contains("declares 4096 bytes, more than the file"),
            "{error}"
        );
    }
}

/// The healthy variants a writer makes that symphonia reads, which the
/// walk read wrongly once: `QuickTime`'s `meta` (no version and flags) in
/// `moov` or a track, a one-letter track or movie name in `udta`, an item
/// symphonia skips with a short `data`, a `free`, `skip` or `wide` box
/// before `ftyp`, and the chunk offsets in `co64`.
fn healthy_variants() -> Vec<(String, Vec<u8>)> {
    let phone = std::fs::read(fixture(PHONE)).unwrap();
    let name = atom(*b"udta", &atom(*b"name", b"A"));
    let skipped_item = atom(*b"xxxx", &atom(*b"data", &[0; 2]));
    let mut out = vec![
        (
            "moov-meta.m4a".to_owned(),
            with_child_at(&phone, &[b"moov"], &quicktime_meta()),
        ),
        (
            "trak-meta.m4a".to_owned(),
            with_child_at(&phone, &[b"trak"], &quicktime_meta()),
        ),
        (
            "trak-name.m4a".to_owned(),
            with_child_at(&phone, &[b"trak"], &name),
        ),
        (
            "moov-name.m4a".to_owned(),
            with_child_at(&phone, &[b"moov"], &name),
        ),
        (
            "skipped-item.m4a".to_owned(),
            with_child_at(
                &phone,
                &[b"moov"],
                &atom(*b"udta", &itunes_meta(&skipped_item)),
            ),
        ),
        ("co64.m4a".to_owned(), co64_phone()),
    ];
    for kind in [*b"free", *b"skip", *b"wide"] {
        out.push((
            format!("{}-first.m4a", String::from_utf8_lossy(&kind)),
            led_by(&phone, &atom(kind, &[0; 8])),
        ));
    }
    out
}

/// Every fixture still reads as before: the check reads, never changes, so
/// a file it lets through takes the same path as without it. So do the
/// healthy variants, the CAFs symphonia reads, and the phone file's audio
/// comes out the same from every variant that keeps its layout.
#[test]
fn every_fixture_still_reads() {
    let dir = tempfile::tempdir().unwrap();
    let mut files: Vec<PathBuf> = std::fs::read_dir(fixture(""))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    files.push(write(dir.path(), "pcm.caf", &pcm_caf()));
    files.push(write(dir.path(), "float.caf", &float_caf()));
    for (name, bytes) in healthy_variants() {
        files.push(write(dir.path(), &name, &bytes));
    }
    for path in &files {
        SymphoniaAudioCodec::read_channel(path, 0, AudioLane::Mixed)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }
    let phone = SymphoniaAudioCodec::read_channel(&fixture(PHONE), 0, AudioLane::Mixed).unwrap();
    for name in [
        "moov-meta.m4a",
        "trak-meta.m4a",
        "trak-name.m4a",
        "co64.m4a",
    ] {
        let read =
            SymphoniaAudioCodec::read_channel(&dir.path().join(name), 0, AudioLane::Mixed).unwrap();
        assert_eq!(read.samples, phone.samples, "{name}");
    }
    // Their packets are noise, so they open but do not decode.
    for (name, bytes) in [("aac.caf", aac_caf()), ("framed.caf", aac_caf_with(true))] {
        SymphoniaAudioCodec::declared_duration(&write(dir.path(), name, &bytes)).unwrap();
    }
}

// The fuzz pass.

/// A field of a fixture the fuzz sets: where it is, its width in bytes,
/// and whether it holds a CAF variable-length integer (written in exactly
/// `width` bytes of seven bits) rather than a big-endian one.
#[derive(Debug, Clone, Copy)]
struct Field {
    at: usize,
    width: usize,
    varint: bool,
}

impl Field {
    fn new(at: usize, width: usize) -> Self {
        Self {
            at,
            width,
            varint: false,
        }
    }

    fn set(self, bytes: &mut [u8], value: u64) {
        let target = &mut bytes[self.at..self.at + self.width];
        if self.varint {
            for (index, byte) in target.iter_mut().enumerate() {
                let shift = 7 * (self.width - 1 - index);
                let group = u8::try_from((value >> shift.min(63)) & 0x7f).unwrap();
                let more = if index + 1 < self.width { 0x80 } else { 0 };
                *byte = group | more;
            }
        } else {
            target.copy_from_slice(&value.to_be_bytes()[8 - self.width..]);
        }
    }
}

/// The size and count fields of `bytes`' boxes, chunks and tables, and
/// the sample sizes they give.
fn fields(bytes: &[u8]) -> Vec<Field> {
    let mut out = Vec::new();
    if bytes.starts_with(b"caff") {
        let mut at = 8;
        while at + 12 <= bytes.len() {
            let kind = &bytes[at..at + 4];
            let size = i64::from_be_bytes(bytes[at + 4..at + 12].try_into().unwrap());
            out.push(Field::new(at + 4, 8));
            if kind == b"pakt" {
                out.push(Field::new(at + 12, 8));
                if at + 36 + 6 <= bytes.len() {
                    out.push(Field {
                        at: at + 36,
                        width: 6,
                        varint: true,
                    });
                }
            }
            if kind == b"desc" {
                // Bytes and frames a packet, bits a channel.
                out.extend([28, 32, 40].map(|offset| Field::new(at + offset, 4)));
            }
            let Ok(size) = usize::try_from(size) else {
                break;
            };
            at += 12 + size;
        }
        return out;
    }
    for found in boxes(bytes) {
        out.push(Field::new(found.at, 4));
        if found.header == 16 {
            out.push(Field::new(found.at + 8, 8));
        }
        let body = found.at + found.header;
        let room = |at: usize| at + 4 <= found.end;
        match found.path.last().unwrap() {
            b"stts" | b"stsc" | b"stco" | b"co64" | b"trun" => out.push(Field::new(body + 4, 4)),
            b"stsz" => out.extend([4, 8, 12].map(|offset| Field::new(body + offset, 4))),
            // The default sample duration and size, wherever the flags put
            // them, and a run's first entries.
            b"tfhd" => out.extend(
                [12, 16, 20, 24]
                    .into_iter()
                    .filter(|offset| room(body + offset))
                    .map(|offset| Field::new(body + offset, 4)),
            ),
            b"trex" => out.push(Field::new(body + 16, 4)),
            _ => {}
        }
        if found.path.last() == Some(b"trun") {
            out.extend(
                (12..32)
                    .step_by(4)
                    .filter(|offset| room(body + offset))
                    .map(|offset| Field::new(body + offset, 4)),
            );
        }
    }
    out
}

/// A small deterministic generator (xorshift64*), so a failing case can be
/// run again from its number.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }
}

/// A value for a field of `width` bytes in a file of `length`: the edges
/// the measurement found, the file's own length about, or anything.
fn value(random: &mut Random, width: usize, length: u64) -> u64 {
    let max = if width == 4 {
        u64::from(u32::MAX)
    } else {
        u64::MAX
    };
    let picks = [
        0,
        1,
        7,
        8,
        9,
        16,
        24,
        length.saturating_sub(1),
        length,
        length + 1,
        (1 << 31) - 1,
        1 << 31,
        u64::from(u32::MAX),
        1 << 36,
        1 << 40,
        1 << 62,
        u64::MAX,
        random.next(),
    ];
    picks[random.below(picks.len())] & max
}

/// The fixtures the fuzz mutates: every MP4 file (the phone recorder's,
/// killed and not, Apple's encoder with its `ilst`, ffmpeg's with an edit
/// list, a fragmented one, PCM in a `QuickTime` movie), the healthy variants,
/// and the CAFs symphonia reads.
fn fuzz_fixtures() -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(fixture(""))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == "m4a" || ext == "mp4" || ext == "mov")
        })
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read(&path).unwrap())
        })
        .collect();
    out.sort();
    out.extend(healthy_variants());
    out.push(("pcm.caf".to_owned(), pcm_caf()));
    out.push(("float.caf".to_owned(), float_caf()));
    out.push(("aac.caf".to_owned(), aac_caf()));
    out.push(("framed.caf".to_owned(), aac_caf_with(true)));
    out
}

/// Runs `cases` mutations from `seed`, each through the declared length and
/// the decode; a panic is as good as an error here (the pipeline catches
/// it), an abort ends the process and fails the parent.
fn fuzz(cases: u64, seed: u64) {
    let dir = tempfile::tempdir().unwrap();
    let fixtures: Vec<(String, Vec<u8>, Vec<Field>)> = fuzz_fixtures()
        .into_iter()
        .map(|(name, bytes)| {
            let fields = fields(&bytes);
            (name, bytes, fields)
        })
        .collect();
    let mut random = Random(seed | 1);
    let mut stderr = std::io::stderr();
    for case in 0..cases {
        let (name, original, fields) = &fixtures[random.below(fixtures.len())];
        let mut bytes = original.clone();
        let mut changed = Vec::new();
        for _ in 0..=random.below(3) {
            let field = fields[random.below(fields.len())];
            let width = if field.varint { 8 } else { field.width };
            let value = value(&mut random, width, bytes.len() as u64);
            field.set(&mut bytes, value);
            changed.push((field.at, value));
        }
        let extension = Path::new(name).extension().unwrap().to_string_lossy();
        let path = dir.path().join(format!("case.{extension}"));
        std::fs::write(&path, &bytes).unwrap();
        // The case, before it runs: an abort leaves it last.
        let _ = writeln!(stderr, "case {case}: {name} {changed:?}");
        let _ = std::panic::catch_unwind(|| {
            let _ = SymphoniaAudioCodec::declared_duration(&path);
            let _ = SymphoniaAudioCodec::read_channel(&path, 0, AudioLane::Mixed);
        });
    }
}

/// Cases from `STENO_SIZES_FUZZ_CASES` (400 by default, CI's bound), seed
/// from `STENO_SIZES_FUZZ_SEED`.
fn fuzz_settings() -> (u64, u64) {
    let read = |name: &str, default: u64| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    (
        read("STENO_SIZES_FUZZ_CASES", 400),
        read("STENO_SIZES_FUZZ_SEED", 0x5eed),
    )
}

/// The child: the fuzz pass, run by the parent under the limit. Passes at
/// once outside it.
#[test]
#[ignore = "the fuzz test runs it in a child process"]
fn fuzz_child() {
    if std::env::var_os("STENO_SIZES_FUZZ_CHILD").is_none() {
        return;
    }
    let (cases, seed) = fuzz_settings();
    fuzz(cases, seed);
}

/// No size or count field of the fixtures, set to a boundary or a random
/// value, aborts a read. On Linux the pass runs in a child held to 8 GiB of
/// address space; elsewhere (no such limit on the Mac or Windows) in this
/// process.
#[test]
fn random_sizes_and_counts_abort_nothing() {
    if !cfg!(target_os = "linux") {
        let (cases, seed) = fuzz_settings();
        fuzz(cases, seed);
        return;
    }
    let output = Command::new("sh")
        .arg("-c")
        .arg("ulimit -v 8388608 && exec \"$0\" \"$@\"")
        .arg(std::env::current_exe().unwrap())
        .args([
            "fuzz_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("STENO_SIZES_FUZZ_CHILD", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let ran: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("case "))
        .collect();
    assert!(
        output.status.success(),
        "the fuzz child ended {}, last {:?}:\n{}",
        output.status,
        ran.last(),
        stderr.lines().rev().take(5).collect::<Vec<_>>().join("\n")
    );
    let (cases, _) = fuzz_settings();
    assert_eq!(ran.len() as u64, cases, "every case ran");
}

//! The sizes symphonia allocates from, checked before its demuxer opens a
//! file (`codec::sizes`, A17 of `.plans/2026-10-07-stable-promotion.md`).
//! Every case measured to abort the process on Linux is refused as an
//! ordinary error; every fixture still reads; and a fuzz pass that sets
//! the size and count fields of the fixtures' boxes, chunks and tables to
//! random and boundary values aborts nothing. On Linux the fuzz runs in a
//! child of this binary whose address space is held to 8 GiB, a machine
//! that cannot back the allocations the measurement found. Without the
//! check, the first four cases below abort this test binary.

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

/// A box found in an MP4 file: its type, where its header starts, the
/// header's length, and where it ends.
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

/// Every box of `bytes` the walk follows, depth first.
fn boxes(bytes: &[u8]) -> Vec<Found> {
    fn walk(bytes: &[u8], mut at: usize, end: usize, path: &[[u8; 4]], out: &mut Vec<Found>) {
        while at + 8 <= end {
            let mut size = usize::try_from(be32(bytes, at)).unwrap();
            let kind: [u8; 4] = bytes[at + 4..at + 8].try_into().unwrap();
            let mut header = 8;
            if size == 1 {
                size = usize::try_from(u64::from_be_bytes(
                    bytes[at + 8..at + 16].try_into().unwrap(),
                ))
                .unwrap();
                header = 16;
            } else if size == 0 {
                size = end - at;
            }
            if size < header || at + size > end {
                return;
            }
            let mut inner = path.to_vec();
            inner.push(kind);
            out.push(Found {
                path: inner.clone(),
                at,
                header,
                end: at + size,
            });
            let body = at + header;
            let containers: [&[u8; 4]; 12] = [
                b"moov", b"trak", b"mdia", b"minf", b"stbl", b"udta", b"edts", b"dinf", b"mvex",
                b"moof", b"traf", b"ilst",
            ];
            if containers.contains(&&kind) || path.last() == Some(b"ilst") {
                walk(bytes, body, at + size, &inner, out);
            } else if &kind == b"meta" {
                walk(bytes, body + 4, at + size, &inner, out);
            } else if &kind == b"stsd" {
                walk(bytes, body + 8, at + size, &inner, out);
            } else if path.last() == Some(b"stsd") && &kind == b"mp4a" && body + 28 <= at + size {
                walk(bytes, body + 28, at + size, &inner, out);
            }
            at += size;
        }
    }
    let mut out = Vec::new();
    walk(bytes, 0, bytes.len(), &[], &mut out);
    out
}

fn find(bytes: &[u8], kind: [u8; 4]) -> Found {
    boxes(bytes)
        .into_iter()
        .find(|found| found.path.last() == Some(&kind))
        .unwrap_or_else(|| panic!("no {} box", String::from_utf8_lossy(&kind)))
}

fn set32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

/// `child` appended to `moov`, whose size grows to hold it.
fn with_moov_child(bytes: &[u8], child: &[u8]) -> Vec<u8> {
    let moov = find(bytes, *b"moov");
    let mut grown = bytes.to_vec();
    let size = u32::try_from(moov.end - moov.at + child.len()).unwrap();
    set32(&mut grown, moov.at, size);
    grown.splice(moov.end..moov.end, child.iter().copied());
    grown
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

/// `udta`/`meta`/`ilst`/`©nam` around `data`, as iTunes writes a tag.
fn tag_with(data: &[u8]) -> Vec<u8> {
    let name = atom(*b"\xa9nam", data);
    let ilst = atom(*b"ilst", &name);
    let mut hdlr_body = vec![0u8; 8];
    hdlr_body.extend_from_slice(b"mdirappl");
    hdlr_body.extend_from_slice(&[0; 9]);
    let hdlr = atom(*b"hdlr", &hdlr_body);
    let mut meta_body = vec![0u8; 4];
    meta_body.extend_from_slice(&hdlr);
    meta_body.extend_from_slice(&ilst);
    atom(*b"udta", &atom(*b"meta", &meta_body))
}

/// A `data` atom with a 64-bit size of 2^`exponent` and a few bytes.
fn huge_data(exponent: u32) -> Vec<u8> {
    let mut data = 1u32.to_be_bytes().to_vec();
    data.extend_from_slice(b"data");
    data.extend_from_slice(&(1u64 << exponent).to_be_bytes());
    data.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0, b'x']);
    data
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

/// 16-bit big-endian PCM, which symphonia's CAF reader (not the crate's
/// Float32 reader) reads: 0.1 s of a ramp.
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

/// An AAC CAF in shape (its packets are noise): a magic cookie and a
/// packet table of one-byte sizes, as `AVFoundation` writes AAC in CAF.
fn aac_caf() -> Vec<u8> {
    let packets = 20u8;
    let mut pakt = i64::from(packets).to_be_bytes().to_vec();
    pakt.extend_from_slice(&(i64::from(packets) * 1024).to_be_bytes());
    pakt.extend_from_slice(&[0; 8]);
    let mut data = vec![0u8; 4];
    for packet in 0..packets {
        pakt.push(6);
        data.extend_from_slice(&[0x21, packet, 0x49, 0x90, 0x02, 0x19]);
    }
    caf(&[
        (b"desc", desc(*b"aac ", 0, 0, 1024, 0), None),
        (b"kuki", vec![0x12, 0x08], None),
        (b"pakt", pakt, None),
        (b"data", data, None),
    ])
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// The check's refusal, as every path that opens the file with symphonia
/// sees it: the declared length, the decode and the mixdown.
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

/// The cases the measurement found aborting on Linux, each refused before
/// symphonia allocates: a 1 TiB and a 64 GiB `ilst` `data` atom, the
/// sample tables counting 2^32 - 1 entries, a sample larger than the file,
/// and the CAF magic cookie of 1 TiB and packet tables of 2^32 and 2^40
/// packets.
#[test]
fn every_size_measured_to_abort_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let phone = std::fs::read(fixture(PHONE)).unwrap();
    let mut cases: Vec<(&str, Vec<u8>, &str)> = Vec::new();
    for exponent in [40, 36] {
        cases.push((
            "ilst-data.m4a",
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
        cases.push(("table.m4a", bytes, "entries"));
    }
    let stsz = find(&phone, *b"stsz");
    let mut big_sample = phone.clone();
    if be32(&phone, stsz.at + stsz.header + 4) == 0 {
        set32(&mut big_sample, stsz.at + stsz.header + 12, u32::MAX);
    } else {
        set32(&mut big_sample, stsz.at + stsz.header + 4, u32::MAX);
    }
    cases.push(("sample.m4a", big_sample, "more than the file's"));
    cases.push((
        "kuki.caf",
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
            "pakt.caf",
            caf(&[
                (b"desc", desc(*b"aac ", 0, 0, 1024, 0), None),
                (b"pakt", pakt, None),
                (b"data", vec![0; 8], None),
            ]),
            "the packet table counts",
        ));
    }
    cases.push((
        "short-data.m4a",
        with_moov_child(&phone, &tag_with(&atom(*b"data", &[0; 4]))),
        "fewer than its 8",
    ));
    for (index, (name, bytes, says)) in cases.iter().enumerate() {
        let path = write(dir.path(), &format!("{index}-{name}"), bytes);
        let error = refused(&path);
        assert!(error.contains(says), "{name}: {error}");
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
        let path = write(dir.path(), name, &bytes);
        // Symphonia itself may stop where the data was to end.
        if let Err(error) = SymphoniaAudioCodec::read_channel(&path, 0, AudioLane::Mixed) {
            assert!(!error.to_string().contains("declares"), "{name}: {error}");
        }
    }
}

/// Every fixture still reads as before: the check reads, never changes, so
/// a file it lets through takes the same path as without it.
#[test]
fn every_fixture_still_reads() {
    let dir = tempfile::tempdir().unwrap();
    let mut files: Vec<PathBuf> = std::fs::read_dir(fixture(""))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    files.push(write(dir.path(), "pcm.caf", &pcm_caf()));
    for path in files {
        SymphoniaAudioCodec::read_channel(&path, 0, AudioLane::Mixed)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }
    // Its packets are noise, so it opens but does not decode.
    SymphoniaAudioCodec::declared_duration(&write(dir.path(), "aac.caf", &aac_caf())).unwrap();
}

// The fuzz pass.

/// A field of a fixture the fuzz sets: where it is and its width in bytes.
#[derive(Debug, Clone, Copy)]
struct Field {
    at: usize,
    width: usize,
}

/// The size and count fields of `bytes`' boxes, chunks and tables.
fn fields(bytes: &[u8]) -> Vec<Field> {
    let mut out = Vec::new();
    if bytes.starts_with(b"caff") {
        let mut at = 8;
        while at + 12 <= bytes.len() {
            let kind = &bytes[at..at + 4];
            let size = i64::from_be_bytes(bytes[at + 4..at + 12].try_into().unwrap());
            out.push(Field {
                at: at + 4,
                width: 8,
            });
            if kind == b"pakt" {
                out.push(Field {
                    at: at + 12,
                    width: 8,
                });
            }
            if kind == b"desc" {
                out.extend([28, 32].map(|offset| Field {
                    at: at + offset,
                    width: 4,
                }));
            }
            let Ok(size) = usize::try_from(size) else {
                break;
            };
            at += 12 + size;
        }
        return out;
    }
    for found in boxes(bytes) {
        out.push(Field {
            at: found.at,
            width: 4,
        });
        if found.header == 16 {
            out.push(Field {
                at: found.at + 8,
                width: 8,
            });
        }
        let body = found.at + found.header;
        match found.path.last().unwrap() {
            b"stts" | b"stsc" | b"stco" | b"co64" | b"trun" => {
                out.push(Field {
                    at: body + 4,
                    width: 4,
                });
            }
            b"stsz" => out.extend([4, 8].map(|offset| Field {
                at: body + offset,
                width: 4,
            })),
            _ => {}
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
/// list) and the two CAFs symphonia reads.
fn fuzz_fixtures() -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(fixture(""))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == "m4a" || ext == "mp4")
        })
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read(&path).unwrap())
        })
        .collect();
    out.sort();
    out.push(("pcm.caf".to_owned(), pcm_caf()));
    out.push(("aac.caf".to_owned(), aac_caf()));
    out
}

/// Runs `cases` mutations from `seed`, each through the declared length and
/// the decode; a panic is as good as an error here (the pipeline catches
/// it), an abort ends the process and fails the parent.
fn fuzz(cases: u64, seed: u64) {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = fuzz_fixtures();
    let mut random = Random(seed | 1);
    let mut stderr = std::io::stderr();
    for case in 0..cases {
        let (name, original) = &fixtures[random.below(fixtures.len())];
        let fields = fields(original);
        let mut bytes = original.clone();
        let mut changed = Vec::new();
        for _ in 0..=random.below(3) {
            let field = fields[random.below(fields.len())];
            let value = value(&mut random, field.width, bytes.len() as u64);
            let encoded = value.to_be_bytes();
            bytes[field.at..field.at + field.width].copy_from_slice(&encoded[8 - field.width..]);
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

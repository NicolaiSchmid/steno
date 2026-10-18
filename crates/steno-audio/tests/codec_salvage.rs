//! The salvage of a phone recording whose recorder was killed (no `moov`),
//! on Apple-encoded fixtures (`Tests/Fixtures/README.md`): a finished file,
//! the same killed after 48 packets, and `AVAudioRecorder` killed over
//! silence. Variants (cut, moved, overwritten, other brands) are built from
//! them.

#![allow(clippy::cast_precision_loss, clippy::float_cmp)]

use std::path::{Path, PathBuf};

use steno_audio::codec::salvage::{self, PacketError, packet_length};
use steno_audio::codec::{CodecError, SymphoniaAudioCodec};
use steno_core::paths::file_url;
use steno_core::{AudioAsset, AudioDamage, AudioDecoder, AudioFormat, AudioLane, AudioRetention};
use uuid::Uuid;

/// Where the packets of the recorder's files start: the reserve after the
/// `ftyp` ends there.
const PACKETS_AT: usize = 57_344;

/// The killed file's packets, and the priming the decode drops.
const KILLED_PACKETS: usize = 48;
const PRIMING: usize = 2_112;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../Tests/Fixtures/audio")
        .join(name)
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(fixture(name)).unwrap()
}

fn killed() -> Vec<u8> {
    read("speech-44k1-avaudiofile-killed.m4a")
}

fn finished() -> Vec<u8> {
    read("speech-44k1-avaudiofile.m4a")
}

fn decode(path: &Path) -> Result<steno_audio::codec::DecodedChannel, CodecError> {
    SymphoniaAudioCodec::read_channel(path, 0, AudioLane::Mixed)
}

fn written(directory: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn be32(bytes: &[u8], at: usize) -> usize {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize
}

/// The offset of the first box of type `kind` (its size field).
fn find_box(bytes: &[u8], kind: [u8; 4]) -> usize {
    bytes.windows(4).position(|w| w == kind).unwrap() - 4
}

/// The packet sizes a finished file's sample table lists.
fn sample_sizes(bytes: &[u8]) -> Vec<usize> {
    let stsz = find_box(bytes, *b"stsz");
    assert_eq!(be32(bytes, stsz + 12), 0, "one size per sample");
    let count = be32(bytes, stsz + 16);
    (0..count).map(|i| be32(bytes, stsz + 20 + 4 * i)).collect()
}

/// Where the `index`th packet of the finished file starts, counted from its
/// first packet.
fn packet_offset(index: usize) -> usize {
    sample_sizes(&finished())[..index].iter().sum()
}

/// The killed recording decodes as the finished file does, sample for
/// sample up to the last packet, less the same priming, and says it was
/// salvaged.
#[tokio::test]
async fn a_killed_recording_decodes_as_its_finished_twin_up_to_the_kill() {
    let killed_bytes = killed();
    assert!(
        !killed_bytes.windows(4).any(|w| w == b"moov"),
        "the killed file has no index"
    );
    let finished_bytes = finished();
    let sizes = sample_sizes(&finished_bytes);
    let data: usize = sizes[..KILLED_PACKETS].iter().sum();
    assert_eq!(killed_bytes.len(), PACKETS_AT + data);
    assert_eq!(
        killed_bytes[PACKETS_AT..],
        finished_bytes[PACKETS_AT..PACKETS_AT + data]
    );

    let salvaged = decode(&fixture("speech-44k1-avaudiofile-killed.m4a")).unwrap();
    let clean = decode(&fixture("speech-44k1-avaudiofile.m4a")).unwrap();
    assert_eq!((salvaged.sample_rate, salvaged.channels), (44_100, 1));
    assert_eq!(salvaged.samples.len(), KILLED_PACKETS * 1_024 - PRIMING);
    assert_eq!(clean.samples.len(), sizes.len() * 1_024 - PRIMING);
    assert!(same(
        &salvaged.samples,
        &clean.samples[..salvaged.samples.len()]
    ));
    assert!(salvaged.samples.iter().any(|&s| s.abs() > 0.1), "speech");
    assert_eq!(
        salvaged.damage,
        AudioDamage {
            parts: 0,
            seconds: 0.0,
            salvaged: true,
        }
    );
    assert_eq!(clean.damage, AudioDamage::default());

    let asset = AudioAsset {
        id: Uuid::new_v4(),
        meeting_id: Uuid::new_v4(),
        url: file_url(&fixture("speech-44k1-avaudiofile-killed.m4a"), false),
        format: AudioFormat::M4aAac,
        lanes: vec![AudioLane::Mixed],
        sidecars_16k: std::collections::BTreeMap::new(),
        mixdown_url: None,
        retention: AudioRetention::KeepForever,
        expires_at: None,
    };
    let lane = SymphoniaAudioCodec::new()
        .decode(&asset, AudioLane::Mixed)
        .await
        .unwrap();
    assert!(lane.damage.salvaged);
    assert_eq!(
        lane.samples,
        SymphoniaAudioCodec::to_16k(&salvaged.samples, 44_100)
    );
    assert_eq!(
        SymphoniaAudioCodec::declared_duration(&fixture("speech-44k1-avaudiofile-killed.m4a"))
            .unwrap(),
        None,
        "nothing declares its length"
    );
}

/// `AVAudioRecorder` killed over silence: its 126 packets read as zeros.
#[test]
fn the_recorders_own_killed_file_reads_as_its_silence() {
    let bytes = read("silence-44k1-avaudiorecorder-killed.m4a");
    assert!(bytes[28..PACKETS_AT].iter().all(|&b| b == 0));
    assert_eq!(bytes.len() - PACKETS_AT, 126 * 4);
    let decoded = decode(&fixture("silence-44k1-avaudiorecorder-killed.m4a")).unwrap();
    assert_eq!(decoded.samples.len(), 126 * 1_024 - PRIMING);
    assert!(decoded.samples.iter().all(|&s| s == 0.0));
    assert!(decoded.damage.salvaged);
}

/// Every packet of the finished files ends where `stsz` says.
#[test]
fn every_packet_ends_where_the_sample_table_says() {
    for name in [
        "speech-44k1-avaudiofile.m4a",
        "silence-44k1-avaudiorecorder.m4a",
        "tone-440-44k1-onset-200ms-apple.m4a",
    ] {
        let bytes = read(name);
        let mdat = find_box(&bytes, *b"mdat") + 8;
        let mut at = mdat;
        let sizes = sample_sizes(&bytes);
        for (index, &size) in sizes.iter().enumerate() {
            assert_eq!(
                packet_length(&bytes[at..]),
                Ok(size),
                "{name}, packet {index}"
            );
            assert_eq!(
                packet_length(&bytes[at..at + size - 1]),
                Err(PacketError::Truncated),
                "{name}, packet {index} less its last byte"
            );
            at += size;
        }
        assert_eq!(
            at,
            mdat + be32(&bytes, mdat - 8) - 8,
            "{name}: the whole mdat"
        );
    }
}

/// Only the recorder's layout is salvaged: other brands, a `moov` anywhere,
/// non-MP4 bytes, or a file with no packets stay failed with symphonia's
/// error, and none is written to.
#[test]
fn only_the_recorders_layout_is_salvaged() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = killed();
    assert_eq!(&bytes[4..12], b"ftypM4A ");
    assert_eq!(&bytes[16..28], b"M4A mp42isom");
    let mut variants: Vec<(&str, Vec<u8>)> = Vec::new();
    let mut major = bytes.clone();
    major[8..12].copy_from_slice(b"isom");
    variants.push(("isom", major));
    let mut compatible = bytes.clone();
    compatible[20..24].copy_from_slice(b"iso2");
    variants.push(("no mp42", compatible));
    let mut moov = bytes.clone();
    moov[28..32].copy_from_slice(&16u32.to_be_bytes());
    moov[32..36].copy_from_slice(b"moov");
    variants.push(("a moov", moov));
    let mut no_ftyp = bytes.clone();
    no_ftyp[4..8].copy_from_slice(b"free");
    variants.push(("no ftyp", no_ftyp));
    // A sized `mdat` with a `moov` after it that symphonia cannot read: a
    // file with an index, if a broken one, is not salvaged.
    let packets = &bytes[PACKETS_AT..];
    let mdat_size = u32::try_from(packets.len() + 8).unwrap().to_be_bytes();
    let broken_moov = [16u32.to_be_bytes().as_slice(), b"moov", &[0xff; 8]].concat();
    variants.push((
        "a moov after the mdat",
        [&bytes[..28], &mdat_size, b"mdat", packets, &broken_moov].concat(),
    ));
    let free = [16u32.to_be_bytes().as_slice(), b"free", &[0; 8]].concat();
    variants.push((
        "a moov after the mdat and a free box",
        [
            &bytes[..28],
            &mdat_size,
            b"mdat",
            packets,
            &free,
            &broken_moov,
        ]
        .concat(),
    ));
    variants.push((
        "a moov after a 64-bit mdat",
        [&bytes[..28], &mdat_64(packets), &broken_moov].concat(),
    ));
    let uuid = [32u32.to_be_bytes().as_slice(), b"uuid", &[0x5a; 24]].concat();
    variants.push((
        "a moov after a uuid box and the mdat",
        [
            &bytes[..28],
            &uuid,
            &mdat_size,
            b"mdat",
            packets,
            &broken_moov,
        ]
        .concat(),
    ));
    variants.push(("only the ftyp", bytes[..28].to_vec()));
    variants.push(("only the reserve", bytes[..PACKETS_AT].to_vec()));
    for (name, variant) in variants {
        let path = written(directory.path(), &format!("{name}.m4a"), &variant);
        assert!(
            matches!(
                decode(&path),
                Err(CodecError::UnsupportedFormat(_) | CodecError::UnknownFormat)
            ),
            "{name}: {:?}",
            decode(&path).map(|d| d.samples.len())
        );
        assert_eq!(std::fs::read(&path).unwrap(), variant, "{name}");
    }
    assert!(decode(&written(directory.path(), "kept.m4a", &bytes)).is_ok());
}

/// `packets` in an `mdat` box of a 64-bit size: a size field of 1, the type,
/// then the size in 64 bits.
fn mdat_64(packets: &[u8]) -> Vec<u8> {
    let size = u64::try_from(packets.len() + 16).unwrap();
    [
        1u32.to_be_bytes().as_slice(),
        b"mdat",
        &size.to_be_bytes(),
        packets,
    ]
    .concat()
}

/// A stream of more than one channel is refused and not written to.
#[test]
fn a_stream_of_two_channels_is_not_salvaged() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = killed();
    // Apple's silent packet less its `END`: a single channel element with a
    // long window and no bands.
    let channel = |id: &str, tag: &str| format!("{id}{tag}0110100000000000000000");
    assert_eq!(
        bits(&[&channel("000", "0000"), "111"]),
        [0x00, 0xd0, 0x00, 0x07]
    );
    let pair = bits(&[&channel("001", "0000"), "111"]);
    let two = bits(&[&channel("000", "0000"), &channel("000", "0001"), "111"]);
    assert_eq!(
        packet_length(&two),
        Err(PacketError::Invalid("a second channel"))
    );
    assert!(matches!(packet_length(&pair), Err(PacketError::Invalid(_))));
    for (name, packet) in [("pair", pair), ("two", two)] {
        let variant = [&bytes[..PACKETS_AT], &packet.repeat(64)].concat();
        let path = written(directory.path(), &format!("{name}.m4a"), &variant);
        assert!(
            matches!(decode(&path), Err(CodecError::UnsupportedFormat(_))),
            "{name}: {:?}",
            decode(&path).map(|d| d.samples.len())
        );
        assert_eq!(std::fs::read(&path).unwrap(), variant, "{name}");
    }
}

/// A recording cut inside a packet decodes up to the cut.
#[test]
fn a_recording_cut_mid_packet_decodes_up_to_the_cut() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = killed();
    let clean = decode(&fixture("speech-44k1-avaudiofile.m4a")).unwrap();
    let sizes = sample_sizes(&finished());
    for (cut, packets) in [
        (PACKETS_AT + packet_offset(40) + sizes[40] / 2, 40),
        (PACKETS_AT + packet_offset(40) + 1, 40),
        (PACKETS_AT + packet_offset(41), 41),
        (PACKETS_AT + packet_offset(5) + 3, 5),
    ] {
        let path = written(directory.path(), &format!("cut-{cut}.m4a"), &bytes[..cut]);
        let decoded = decode(&path).unwrap();
        assert_eq!(
            decoded.samples.len(),
            packets * 1_024 - PRIMING,
            "cut at {cut}"
        );
        assert!(same(
            &decoded.samples,
            &clean.samples[..decoded.samples.len()]
        ));
        assert!(decoded.damage.salvaged);
    }
}

/// Where the packets start is found, not assumed: other reserve sizes, a
/// longer `ftyp` and leftover `mdat` headers decode the same.
#[test]
fn the_packets_are_found_wherever_the_reserve_ends() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = killed();
    let expected = decode(&fixture("speech-44k1-avaudiofile-killed.m4a"))
        .unwrap()
        .samples;
    let (ftyp, packets) = (&bytes[..28], &bytes[PACKETS_AT..]);
    let with_reserve = |reserve: usize| [ftyp, &vec![0; reserve], packets].concat();
    let mdat = [ftyp, &[0, 0, 0, 0], b"mdat", packets].concat();
    let placeholder = |size: u32| {
        [
            ftyp,
            &size.to_be_bytes(),
            b"mdat",
            &vec![0; PACKETS_AT - 36],
            packets,
        ]
        .concat()
    };
    let mut longer_ftyp = [ftyp, b"iso2", &bytes[28..]].concat();
    longer_ftyp[..4].copy_from_slice(&32u32.to_be_bytes());
    for (name, variant) in [
        ("short", with_reserve(1_000)),
        ("long", with_reserve(70_000)),
        ("none", with_reserve(0)),
        ("mdat", mdat),
        ("64-bit mdat", [ftyp, &mdat_64(packets)].concat()),
        ("placeholder of 0", placeholder(0)),
        ("placeholder of 8", placeholder(8)),
        ("longer ftyp", longer_ftyp),
    ] {
        let path = written(directory.path(), &format!("{name}.m4a"), &variant);
        assert_eq!(decode(&path).unwrap().samples, expected, "{name}");
    }
}

/// When the first packet starts with a non-zero byte, the packets start
/// there, not at an earlier alignment that parses by chance.
#[test]
fn a_first_packet_that_starts_with_a_set_byte_is_found() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = killed();
    let first = sample_sizes(&finished())[0];
    assert_ne!(bytes[PACKETS_AT + first], 0);
    let variant = [&bytes[..PACKETS_AT], &bytes[PACKETS_AT + first..]].concat();
    let path = written(directory.path(), "no-priming-packet.m4a", &variant);
    let decoded = decode(&path).unwrap();
    assert_eq!(
        decoded.samples.len(),
        (KILLED_PACKETS - 1) * 1_024 - PRIMING
    );
    assert!(decoded.damage.salvaged);
    assert_eq!(decoded.damage.parts, 0);
}

/// A finished file whose `moov` was overwritten reads every packet from
/// the `mdat` body.
#[test]
fn a_finished_file_that_lost_its_moov_is_read_from_its_mdat() {
    let directory = tempfile::tempdir().unwrap();
    let mut bytes = finished();
    let moov = find_box(&bytes, *b"moov");
    bytes[moov + 4..moov + 8].copy_from_slice(b"free");
    let path = written(directory.path(), "lost-moov.m4a", &bytes);
    let decoded = decode(&path).unwrap();
    let clean = decode(&fixture("speech-44k1-avaudiofile.m4a")).unwrap();
    assert!(same(&decoded.samples, &clean.samples));
    assert!(decoded.damage.salvaged);
}

/// Bytes that are not a packet end the salvage there; less than half
/// readable fails the file. Trailing zeros are not part of the recording.
#[test]
fn bytes_that_are_not_packets_end_the_salvage() {
    let directory = tempfile::tempdir().unwrap();
    let clean = decode(&fixture("speech-44k1-avaudiofile.m4a")).unwrap();
    let bytes = killed();
    let overwritten = |packet: usize| {
        let mut variant = bytes.clone();
        variant[PACKETS_AT + packet_offset(packet)..].fill(0xAA);
        variant
    };
    let late = written(directory.path(), "late.m4a", &overwritten(40));
    let decoded = decode(&late).unwrap();
    assert_eq!(decoded.samples.len(), 40 * 1_024 - PRIMING);
    assert!(same(
        &decoded.samples,
        &clean.samples[..decoded.samples.len()]
    ));
    let early = written(directory.path(), "early.m4a", &overwritten(8));
    assert!(
        matches!(decode(&early), Err(CodecError::ConversionFailed(ref e)) if e.contains("index")),
        "{:?}",
        decode(&early).map(|d| d.samples.len())
    );

    let padded = [bytes.as_slice(), &vec![0; 4 * PACKETS_AT]].concat();
    let path = written(directory.path(), "padded.m4a", &padded);
    assert_eq!(
        decode(&path).unwrap().samples.len(),
        KILLED_PACKETS * 1_024 - PRIMING
    );
}

/// A recording longer than one 64 KiB read is salvaged across refills.
#[test]
fn a_salvage_reads_the_file_to_its_end_across_refills() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = killed();
    let copies = 24;
    let long = [&bytes[..PACKETS_AT], &bytes[PACKETS_AT..].repeat(copies)].concat();
    assert!(long.len() - PACKETS_AT > 3 * 64 * 1_024);
    let decoded = decode(&written(directory.path(), "long.m4a", &long)).unwrap();
    assert_eq!(
        decoded.samples.len(),
        copies * KILLED_PACKETS * 1_024 - PRIMING
    );
    assert_eq!(
        decoded.damage,
        AudioDamage {
            parts: 0,
            seconds: 0.0,
            salvaged: true,
        }
    );
    let single = decode(&fixture("speech-44k1-avaudiofile-killed.m4a"))
        .unwrap()
        .samples;
    assert!(same(&decoded.samples[..single.len()], &single));
}

/// The half rule ([`salvage::SalvagedPackets::too_little`]): exactly half
/// readable still salvages, less fails.
#[test]
fn a_salvage_reads_at_least_half_of_the_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = killed();
    let data = bytes.len() - PACKETS_AT;
    for (percent, reads) in [(45, false), (55, true)] {
        let target = data * percent / 100;
        let packet = (0..=KILLED_PACKETS)
            .min_by_key(|&k| packet_offset(k).abs_diff(target))
            .unwrap();
        let share = packet_offset(packet) * 100 / data;
        assert!(share.abs_diff(percent) <= 2, "{percent} %: {share} %");
        let mut variant = bytes.clone();
        variant[PACKETS_AT + packet_offset(packet)..].fill(0xAA);
        let path = written(directory.path(), &format!("{percent}.m4a"), &variant);
        match decode(&path) {
            Ok(decoded) if reads => {
                assert_eq!(decoded.samples.len(), packet * 1_024 - PRIMING);
            }
            Err(CodecError::ConversionFailed(e)) if !reads => assert!(e.contains("half"), "{e}"),
            other => panic!("{percent} %: {:?}", other.map(|d| d.samples.len())),
        }
    }
    let packet = 24;
    let read = packet_offset(packet);
    let half = [&bytes[..PACKETS_AT + read], &vec![0xAA; read]].concat();
    let decoded = decode(&written(directory.path(), "half.m4a", &half)).unwrap();
    assert_eq!(decoded.samples.len(), packet * 1_024 - PRIMING);
}

/// The salvage's rate and channels match the phone's preset: exactly one
/// `sampleRate:` and one `numberOfChannels:` in `recording-options.ts`,
/// asking for 44.1 kHz mono.
#[test]
fn the_salvage_assumes_the_phones_preset() {
    let preset = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../mobile/src/features/recorder/recording-options.ts");
    let preset = std::fs::read_to_string(&preset).unwrap();
    let values = |key: &str| -> Vec<String> {
        preset
            .split(key)
            .skip(1)
            .map(|rest| rest.split(',').next().unwrap().trim().to_owned())
            .collect()
    };
    assert_eq!(
        values("sampleRate:"),
        ["44_100"],
        "the phone's rate changed"
    );
    assert_eq!(
        values("numberOfChannels:"),
        ["1"],
        "the phone's channels changed"
    );
    assert_eq!(salvage::SAMPLE_RATE, 44_100);
}

/// A packet that parses but does not decode becomes counted silence inside
/// a salvage too, and later packets keep their time.
#[test]
fn a_packet_the_decoder_refuses_becomes_silence_in_a_salvage() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = killed();
    let refused = bits(&[
        "000",
        "0000",
        "00000000", // SCE, tag 0, global gain 0
        "0",
        "00",
        "0",
        "000001",
        "0", // ics_info: long, one band, no predictor
        "1101",
        "00001",     // one section of the noise codebook
        "000000000", // the first noise energy
        "0",
        "0",
        "0",   // no pulse, TNS or gain control
        "111", // END
    ]);
    assert_eq!(packet_length(&refused), Ok(refused.len()));
    let at = PACKETS_AT + packet_offset(20);
    let spliced = [&bytes[..at], &refused, &bytes[at..]].concat();
    let path = written(directory.path(), "refused.m4a", &spliced);
    let decoded = decode(&path).unwrap();
    assert_eq!(
        decoded.samples.len(),
        (KILLED_PACKETS + 1) * 1_024 - PRIMING
    );
    let silence = 20 * 1_024 - PRIMING..21 * 1_024 - PRIMING;
    assert!(decoded.samples[silence.clone()].iter().all(|&s| s == 0.0));
    let clean = decode(&fixture("speech-44k1-avaudiofile.m4a")).unwrap();
    assert!(same(
        &decoded.samples[..silence.start],
        &clean.samples[..silence.start]
    ));
    assert_eq!(decoded.damage.parts, 1);
    assert!((decoded.damage.seconds - 1_024.0 / 44_100.0).abs() < 1e-9);
    assert!(decoded.damage.salvaged);
}

/// A file that makes symphonia's demuxer panic (a box size of 2^64 - 1
/// overflows in a debug build) fails as one it refuses, unwritten, and the
/// caught panic writes no crash log.
#[test]
fn a_file_the_demuxer_panics_on_fails_and_is_kept() {
    thread_local! {
        static PANICS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    let support = tempfile::tempdir().unwrap();
    let earlier = support.path().join("crash-2026-10-01T12-00-00.000Z.log");
    std::fs::write(&earlier, "an earlier crash").unwrap();
    steno_core::crash_log::install_crash_log_hook(support.path().to_path_buf(), None);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        PANICS.with(|count| count.set(count.get() + 1));
        previous(panic);
    }));
    let directory = tempfile::tempdir().unwrap();
    let bytes = killed();
    let huge = [
        1u32.to_be_bytes().as_slice(),
        b"free",
        &u64::MAX.to_be_bytes(),
    ]
    .concat();
    let mut other = bytes[..28].to_vec();
    other[8..12].copy_from_slice(b"isom");
    for (name, ftyp) in [("phone", &bytes[..28]), ("isom", other.as_slice())] {
        let variant = [ftyp, &huge, &bytes[PACKETS_AT..]].concat();
        let path = written(directory.path(), &format!("{name}.m4a"), &variant);
        assert!(
            matches!(decode(&path), Err(CodecError::UnsupportedFormat(_))),
            "{name}: {:?}",
            decode(&path).map(|d| d.samples.len())
        );
        assert_eq!(std::fs::read(&path).unwrap(), variant, "{name}");
    }
    // The default hook again, for the assertions below and the other tests.
    drop(std::panic::take_hook());
    // A release build's demuxer returns an error for these sizes instead.
    if cfg!(debug_assertions) {
        assert_eq!(PANICS.with(std::cell::Cell::get), 2, "the demuxer panicked");
    }
    let logs: Vec<_> = std::fs::read_dir(support.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(logs, [earlier], "no crash log for a caught panic");
}

/// The phone's settings, as the decode is built from them.
#[test]
fn the_salvage_decodes_with_the_phones_settings() {
    let params = salvage::SalvagedPackets::codec_params();
    assert_eq!(params.sample_rate, Some(44_100));
    assert_eq!(
        params.channels.map(symphonia::core::audio::Channels::count),
        Some(1)
    );
    assert_eq!(params.extra_data.as_deref(), Some(&[0x12, 0x08][..]));
}

/// Whether two runs of samples are equal, without printing them.
fn same(a: &[f32], b: &[f32]) -> bool {
    a == b
}

/// `parts` of '0' and '1' as bytes, zero-padded to the next byte.
fn bits(parts: &[&str]) -> Vec<u8> {
    let text: String = parts.concat();
    text.as_bytes()
        .chunks(8)
        .map(|chunk| {
            chunk
                .iter()
                .enumerate()
                .fold(0u8, |byte, (i, &c)| byte | (u8::from(c == b'1') << (7 - i)))
        })
        .collect()
}

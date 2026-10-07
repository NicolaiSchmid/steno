//! The model store against a local HTTP server that serves a temporary
//! directory: resumable `.partial` downloads (a cut connection, a partial
//! a killed run left, a corrupt prefix, a complete partial, a host that
//! ignores `Range`, a `206` from the wrong offset or without
//! `Content-Range`), the mirror layout and the checksum gate. No network
//! beyond 127.0.0.1.

#![allow(clippy::cast_possible_truncation)]

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use steno_speech::{ModelAsset, ModelFile, ModelSource, ModelStore, SpeechError};

/// What the server does besides serving files.
#[derive(Debug, Clone, Copy, Default)]
struct Behaviour {
    /// Closes the first response's connection after this many body bytes.
    cut_first_after: Option<usize>,
    /// Answers every request with the whole file, as some hosts do.
    ignore_range: bool,
    /// Answers a range request with the whole file under `206`, labelled
    /// as starting at 0 or, with `false`, without a `Content-Range`.
    whole_file_as_206: Option<bool>,
}

/// One request the server saw: the path and the `Range` and
/// `Accept-Encoding` headers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    path: String,
    range: Option<String>,
    accept_encoding: Option<String>,
}

/// Serves the files under `root` at `http://127.0.0.1:<port>/<path>` until
/// the test ends.
struct FileServer {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl FileServer {
    fn start(root: &Path, behaviour: Behaviour) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let root = root.to_path_buf();
        let log = Arc::clone(&seen);
        let cut_done = Arc::new(AtomicBool::new(false));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (root, log, cut_done) = (root.clone(), Arc::clone(&log), Arc::clone(&cut_done));
                std::thread::spawn(move || {
                    let _ = answer(&stream, &root, behaviour, &log, &cut_done);
                });
            }
        });
        FileServer { base, seen }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn answer(
    stream: &TcpStream,
    root: &Path,
    behaviour: Behaviour,
    log: &Mutex<Vec<Seen>>,
    cut_done: &AtomicBool,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let path = line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .trim_start_matches('/')
        .to_owned();
    let (mut range, mut accept_encoding) = (None, None);
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("range") {
                range = Some(value.trim().to_owned());
            } else if name.eq_ignore_ascii_case("accept-encoding") {
                accept_encoding = Some(value.trim().to_owned());
            }
        }
    }
    log.lock().unwrap().push(Seen {
        path: path.clone(),
        range: range.clone(),
        accept_encoding,
    });
    let mut out = stream;
    let Ok(body) = fs::read(root.join(&path)) else {
        return out.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    };
    let start = range.filter(|_| !behaviour.ignore_range).and_then(|r| {
        r.strip_prefix("bytes=")?
            .strip_suffix('-')?
            .parse::<usize>()
            .ok()
    });
    let (head, slice) = match start {
        Some(_) if behaviour.whole_file_as_206.is_some() => {
            let range = if behaviour.whole_file_as_206 == Some(true) {
                format!(
                    "Content-Range: bytes 0-{}/{}\r\n",
                    body.len() - 1,
                    body.len()
                )
            } else {
                String::new()
            };
            (
                format!(
                    "HTTP/1.1 206 Partial Content\r\n{range}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                ),
                &body[..],
            )
        }
        Some(start) if start >= body.len() => {
            let head = format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                body.len()
            );
            (head, &body[..0])
        }
        Some(start) => (
            format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len() - 1,
                body.len(),
                body.len() - start
            ),
            &body[start..],
        ),
        None => (
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            ),
            &body[..],
        ),
    };
    out.write_all(head.as_bytes())?;
    match behaviour.cut_first_after {
        Some(cut) if !cut_done.swap(true, Ordering::SeqCst) => {
            out.write_all(&slice[..cut.min(slice.len())])?;
            out.flush()?;
            stream.shutdown(Shutdown::Both)
        }
        _ => out.write_all(slice),
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// A body that is not periodic in any short stride, so a misplaced offset
/// changes the checksum.
fn body(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 7919) % 251) as u8).collect()
}

const ID: &str = "test-asset";
const NAME: &str = "model.onnx";
const PARTIAL: &str = "model.onnx.partial";
const LOCK: &str = "model.onnx.lock";

/// A server over a directory holding a file at `<ID>/<NAME>`, an empty
/// store and the asset of that one file.
struct Fixture {
    server: FileServer,
    store: ModelStore,
    asset: ModelAsset,
    _served: tempfile::TempDir,
    _root: tempfile::TempDir,
}

fn fixture(contents: &[u8], behaviour: Behaviour) -> Fixture {
    let files = tempfile::tempdir().unwrap();
    fs::create_dir_all(files.path().join(ID)).unwrap();
    fs::write(files.path().join(ID).join(NAME), contents).unwrap();
    let server = FileServer::start(files.path(), behaviour);
    let root = tempfile::tempdir().unwrap();
    let asset = ModelAsset {
        id: ID.to_owned(),
        display_name: "Test".to_owned(),
        licence: "MIT".to_owned(),
        attribution: String::new(),
        files: vec![ModelFile {
            name: NAME.to_owned(),
            source: Some(ModelSource::Url(format!("{}/{ID}/{NAME}", server.base))),
            sha256: digest(contents),
            size: contents.len() as u64,
        }],
    };
    Fixture {
        server,
        store: ModelStore::new(root.path()),
        asset,
        _served: files,
        _root: root,
    }
}

impl Fixture {
    fn directory(&self) -> PathBuf {
        self.store.directory(&self.asset)
    }

    /// Leaves `<NAME>.partial` holding `bytes`, as an earlier run would.
    fn leave_partial(&self, bytes: &[u8]) -> PathBuf {
        let partial = self.directory().join(PARTIAL);
        fs::create_dir_all(self.directory()).unwrap();
        fs::write(&partial, bytes).unwrap();
        partial
    }

    /// Installs the asset and checks it.
    fn install(&self) {
        self.store.ensure(&self.asset, &mut |_| {}).unwrap();
        self.store.verify(&self.asset).unwrap();
    }
}

fn names(directory: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_cut_connection_resumes_with_a_range_request() {
    let contents = body(300_000);
    let f = fixture(
        &contents,
        Behaviour {
            cut_first_after: Some(100_000),
            ..Behaviour::default()
        },
    );
    let mut starts = Vec::new();
    let mut last = 0;
    f.store
        .ensure(&f.asset, &mut |p| {
            if p.received <= last || starts.is_empty() {
                starts.push(p.received);
            }
            last = p.received;
            assert_eq!(p.total, contents.len() as u64);
        })
        .unwrap();
    f.store.verify(&f.asset).unwrap();
    let seen = f.server.seen();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0].range, None);
    // Whatever the first attempt wrote before the cut is kept: the second
    // asks for the rest, and its progress starts there.
    let resumed_at: u64 = seen[1]
        .range
        .as_deref()
        .and_then(|r| r.strip_prefix("bytes=")?.strip_suffix('-')?.parse().ok())
        .expect("a Range request");
    assert!(resumed_at > 0 && resumed_at <= 100_000, "{resumed_at}");
    assert_eq!(starts, [0, resumed_at]);
    // Uncompressed, so a byte range is a range of the file itself.
    assert!(
        seen.iter()
            .all(|s| s.accept_encoding.as_deref() == Some("identity")),
        "{seen:?}"
    );
    assert_eq!(names(&f.directory()), [NAME, LOCK]);
}

#[test]
fn a_partial_a_killed_run_left_is_resumed_not_fetched_again() {
    let contents = body(200_000);
    let f = fixture(&contents, Behaviour::default());
    f.leave_partial(&contents[..123_456]);
    let mut first = None;
    f.store
        .ensure(&f.asset, &mut |p| {
            first.get_or_insert(p.received);
        })
        .unwrap();
    f.store.verify(&f.asset).unwrap();
    assert_eq!(first, Some(123_456));
    assert_eq!(
        f.server.seen(),
        [Seen {
            path: format!("{ID}/{NAME}"),
            range: Some("bytes=123456-".to_owned()),
            accept_encoding: Some("identity".to_owned()),
        }]
    );
    assert_eq!(names(&f.directory()), [NAME, LOCK]);
}

#[test]
fn a_corrupt_prefix_fails_its_checksum_and_is_fetched_again_from_zero() {
    let contents = body(50_000);
    let f = fixture(&contents, Behaviour::default());
    f.leave_partial(&vec![0xAA; 20_000]);
    f.install();
    let ranges: Vec<_> = f.server.seen().into_iter().map(|s| s.range).collect();
    assert_eq!(ranges, [Some("bytes=20000-".to_owned()), None]);
    assert_eq!(names(&f.directory()), [NAME, LOCK]);
}

#[test]
fn a_host_that_ignores_the_range_gets_the_file_written_from_the_start() {
    let contents = body(40_000);
    let f = fixture(
        &contents,
        Behaviour {
            ignore_range: true,
            ..Behaviour::default()
        },
    );
    f.leave_partial(&contents[..10_000]);
    f.install();
    assert_eq!(f.server.seen().len(), 1);
}

#[test]
fn a_partial_longer_than_the_file_or_already_complete_is_handled() {
    let contents = body(10_000);
    let f = fixture(&contents, Behaviour::default());
    // Longer than the manifest size: cannot be a prefix, so it is emptied
    // and the whole file is requested.
    f.leave_partial(&vec![1; 20_000]);
    f.install();
    assert_eq!(f.server.seen()[0].range, None);
    // Complete but never renamed (killed between the sync and the rename,
    // or a rename Windows refused): verified and installed without a
    // request.
    f.store.remove(&f.asset).unwrap();
    f.leave_partial(&contents);
    f.install();
    assert_eq!(f.server.seen().len(), 1);
    // Complete but wrong: checked, then fetched once more from zero.
    f.store.remove(&f.asset).unwrap();
    f.leave_partial(&vec![7; contents.len()]);
    f.install();
    let ranges: Vec<_> = f
        .server
        .seen()
        .into_iter()
        .skip(1)
        .map(|s| s.range)
        .collect();
    assert_eq!(ranges, [None]);
}

#[test]
fn a_wrong_checksum_is_rejected_and_nothing_is_kept() {
    let contents = body(30_000);
    let mut f = fixture(&contents, Behaviour::default());
    f.asset.files[0].sha256 = digest(b"something else");
    let error = f.store.ensure(&f.asset, &mut |_| {}).unwrap_err();
    assert!(matches!(error, SpeechError::Checksum { .. }), "{error}");
    assert_eq!(names(&f.directory()), [LOCK]);
    assert!(!f.store.is_installed(&f.asset));
}

#[test]
fn a_mirror_serves_every_file_from_asset_id_and_file_name() {
    let contents = body(5_000);
    let mut f = fixture(&contents, Behaviour::default());
    // The source is unreachable, or absent altogether: the mirror wins.
    f.asset.files[0].source = Some(ModelSource::Url("http://127.0.0.1:9/never".to_owned()));
    f.store = f.store.with_mirror(Some(format!("{}/", f.server.base)));
    f.install();
    f.store.remove(&f.asset).unwrap();
    f.asset.files[0].source = None;
    f.install();
    assert!(
        f.server
            .seen()
            .iter()
            .all(|s| s.path == format!("{ID}/{NAME}"))
    );
    assert_eq!(f.server.seen().len(), 2);
}

#[test]
fn a_download_waits_for_the_one_holding_its_partial_and_resumes_it() {
    // Another download holds the lock and stops; this one waits for the
    // lock instead of fetching a copy of its own, then continues the bytes
    // the other left.
    let contents = body(20_000);
    let f = fixture(&contents, Behaviour::default());
    f.leave_partial(&contents[..7_000]);
    let mut held = Some(File::create(f.directory().join(LOCK)).unwrap());
    held.as_ref().unwrap().lock().unwrap();
    // While the lock is held, a report can only come from the wait; the
    // first one lets go of it.
    let mut reports = Vec::new();
    f.store
        .ensure(&f.asset, &mut |p| {
            reports.push((p.received, held.is_some()));
            held = None;
        })
        .unwrap();
    f.store.verify(&f.asset).unwrap();
    assert_eq!(reports[0], (7_000, true));
    let ranges: Vec<_> = f.server.seen().into_iter().map(|s| s.range).collect();
    assert_eq!(ranges, [Some("bytes=7000-".to_owned())]);
    assert_eq!(names(&f.directory()), [NAME, LOCK]);
}

#[test]
fn a_206_from_the_wrong_offset_or_without_a_range_is_not_appended() {
    // Both answers carry the whole file; appended to the partial they
    // would overrun the manifest size. The download starts over instead.
    for labelled in [true, false] {
        let contents = body(30_000);
        let f = fixture(
            &contents,
            Behaviour {
                whole_file_as_206: Some(labelled),
                ..Behaviour::default()
            },
        );
        f.leave_partial(&contents[..12_000]);
        f.install();
        let ranges: Vec<_> = f.server.seen().into_iter().map(|s| s.range).collect();
        assert_eq!(
            ranges,
            [Some("bytes=12000-".to_owned()), None],
            "labelled {labelled}"
        );
        assert_eq!(names(&f.directory()), [NAME, LOCK]);
    }
}

/// Every file under `directory`, relative and with `/`, sorted.
fn tree(directory: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(directory).unwrap();
                let parts: Vec<_> = relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                files.push(parts.join("/"));
            }
        }
    }
    files.sort();
    files
}

/// A bundle laid out in folders, as a `CoreML` `.mlmodelc` is: each file
/// lands in its folder, with its lock and partial beside it there, from
/// the mirror's `<asset id>/<folder>/<file>`; a cut connection on a file in
/// a folder resumes there.
#[test]
fn files_in_folders_of_the_asset_install_into_those_folders() {
    let hosted = tempfile::tempdir().unwrap();
    let files = [
        ("Encoder.mlmodelc/weights/weight.bin", body(300_000)),
        ("Encoder.mlmodelc/coremldata.bin", body(500)),
        ("vocab.json", body(40)),
    ];
    for (name, contents) in &files {
        let path = hosted.path().join(ID).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    let server = FileServer::start(
        hosted.path(),
        Behaviour {
            cut_first_after: Some(100_000),
            ..Behaviour::default()
        },
    );
    let root = tempfile::tempdir().unwrap();
    let store = ModelStore::new(root.path()).with_mirror(Some(server.base.clone()));
    let asset = ModelAsset {
        id: ID.to_owned(),
        display_name: "Test".to_owned(),
        licence: "MIT".to_owned(),
        attribution: String::new(),
        files: files
            .iter()
            .map(|(name, contents)| ModelFile {
                name: (*name).to_owned(),
                source: None,
                sha256: digest(contents),
                size: contents.len() as u64,
            })
            .collect(),
    };
    asset.validate().unwrap();
    assert_eq!(store.missing_files(&asset).len(), 3);
    let mut partials = Vec::new();
    store
        .ensure(&asset, &mut |_| {
            partials.extend(tree(&store.directory(&asset)));
        })
        .unwrap();
    store.verify(&asset).unwrap();
    assert!(
        partials.contains(&"Encoder.mlmodelc/weights/weight.bin.partial".to_owned()),
        "{partials:?}"
    );
    let directory = store.directory(&asset);
    for (name, contents) in &files {
        assert_eq!(&fs::read(directory.join(name)).unwrap(), contents, "{name}");
    }
    assert_eq!(
        tree(&directory),
        [
            "Encoder.mlmodelc/coremldata.bin",
            "Encoder.mlmodelc/coremldata.bin.lock",
            "Encoder.mlmodelc/weights/weight.bin",
            "Encoder.mlmodelc/weights/weight.bin.lock",
            "vocab.json",
            "vocab.json.lock",
        ]
    );
    let seen = server.seen();
    let weights: Vec<_> = seen
        .iter()
        .filter(|s| s.path == format!("{ID}/Encoder.mlmodelc/weights/weight.bin"))
        .collect();
    assert_eq!(weights.len(), 2, "cut once, then resumed: {seen:?}");
    assert!(weights[1].range.is_some(), "{seen:?}");
}

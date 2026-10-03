//! The model store against a local HTTP server that serves a temporary
//! directory: resumable `.partial` downloads (a cut connection, a partial
//! a killed run left, a corrupt prefix, a host that ignores `Range`), the
//! mirror layout and the checksum gate. No network beyond 127.0.0.1.

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
}

/// One request the server saw: the path and the `Range` header.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    path: String,
    range: Option<String>,
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
    let mut range = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("range")
        {
            range = Some(value.trim().to_owned());
        }
    }
    log.lock().unwrap().push(Seen {
        path: path.clone(),
        range: range.clone(),
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
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// A body that is not periodic in any short stride, so a misplaced offset
/// changes the checksum.
fn body(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 7919) % 251) as u8).collect()
}

const ID: &str = "test-asset";
const NAME: &str = "model.onnx";

/// An asset of one file, served by `server` from `<root>/<ID>/<NAME>`.
fn asset(server: &FileServer, contents: &[u8]) -> ModelAsset {
    ModelAsset {
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
    }
}

/// A served directory holding `contents` at `<ID>/<NAME>`, and an empty
/// store root.
fn fixture(contents: &[u8]) -> (tempfile::TempDir, PathBuf, tempfile::TempDir) {
    let served = tempfile::tempdir().unwrap();
    let directory = served.path().join(ID);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(NAME), contents).unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = served.path().to_path_buf();
    (served, path, root)
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
    let (_served, path, root) = fixture(&contents);
    let server = FileServer::start(
        &path,
        Behaviour {
            cut_first_after: Some(100_000),
            ..Behaviour::default()
        },
    );
    let store = ModelStore::new(root.path());
    let asset = asset(&server, &contents);
    let mut starts = Vec::new();
    let mut last = 0;
    store
        .ensure(&asset, &mut |p| {
            if p.received <= last || starts.is_empty() {
                starts.push(p.received);
            }
            last = p.received;
            assert_eq!(p.total, contents.len() as u64);
        })
        .unwrap();
    store.verify(&asset).unwrap();
    let seen = server.seen();
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
    assert_eq!(names(&store.directory(&asset)), [NAME]);
}

#[test]
fn a_partial_a_killed_run_left_is_resumed_not_fetched_again() {
    let contents = body(200_000);
    let (_served, path, root) = fixture(&contents);
    let server = FileServer::start(&path, Behaviour::default());
    let store = ModelStore::new(root.path());
    let asset = asset(&server, &contents);
    let directory = store.directory(&asset);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("model.onnx.partial"), &contents[..123_456]).unwrap();
    let mut first = None;
    store
        .ensure(&asset, &mut |p| {
            first.get_or_insert(p.received);
        })
        .unwrap();
    store.verify(&asset).unwrap();
    assert_eq!(first, Some(123_456));
    assert_eq!(
        server.seen(),
        [Seen {
            path: format!("{ID}/{NAME}"),
            range: Some("bytes=123456-".to_owned()),
        }]
    );
    assert_eq!(names(&directory), [NAME]);
}

#[test]
fn a_corrupt_prefix_fails_its_checksum_and_is_fetched_again_from_zero() {
    let contents = body(50_000);
    let (_served, path, root) = fixture(&contents);
    let server = FileServer::start(&path, Behaviour::default());
    let store = ModelStore::new(root.path());
    let asset = asset(&server, &contents);
    let directory = store.directory(&asset);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("model.onnx.partial"), vec![0xAA; 20_000]).unwrap();
    store.ensure(&asset, &mut |_| {}).unwrap();
    store.verify(&asset).unwrap();
    let ranges: Vec<_> = server.seen().into_iter().map(|s| s.range).collect();
    assert_eq!(ranges, [Some("bytes=20000-".to_owned()), None]);
    assert_eq!(names(&directory), [NAME]);
}

#[test]
fn a_host_that_ignores_the_range_gets_the_file_written_from_the_start() {
    let contents = body(40_000);
    let (_served, path, root) = fixture(&contents);
    let server = FileServer::start(
        &path,
        Behaviour {
            ignore_range: true,
            ..Behaviour::default()
        },
    );
    let store = ModelStore::new(root.path());
    let asset = asset(&server, &contents);
    let directory = store.directory(&asset);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("model.onnx.partial"), &contents[..10_000]).unwrap();
    store.ensure(&asset, &mut |_| {}).unwrap();
    store.verify(&asset).unwrap();
    assert_eq!(server.seen().len(), 1);
}

#[test]
fn a_partial_longer_than_the_file_or_already_complete_is_handled() {
    let contents = body(10_000);
    let (_served, path, root) = fixture(&contents);
    let server = FileServer::start(&path, Behaviour::default());
    let store = ModelStore::new(root.path());
    let asset = asset(&server, &contents);
    let directory = store.directory(&asset);
    fs::create_dir_all(&directory).unwrap();
    // Longer than the manifest size: cannot be a prefix, so it is emptied
    // and the whole file is requested.
    fs::write(directory.join("model.onnx.partial"), vec![1; 20_000]).unwrap();
    store.ensure(&asset, &mut |_| {}).unwrap();
    store.verify(&asset).unwrap();
    assert_eq!(server.seen()[0].range, None);
    // Complete but never renamed (killed between the sync and the rename):
    // the range is past the end, the host answers 416, and the file is
    // fetched whole once more.
    store.remove(&asset).unwrap();
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("model.onnx.partial"), &contents).unwrap();
    store.ensure(&asset, &mut |_| {}).unwrap();
    store.verify(&asset).unwrap();
    let ranges: Vec<_> = server.seen().into_iter().skip(1).map(|s| s.range).collect();
    assert_eq!(ranges, [Some("bytes=10000-".to_owned()), None]);
}

#[test]
fn a_wrong_checksum_is_rejected_and_nothing_is_kept() {
    let contents = body(30_000);
    let (_served, path, root) = fixture(&contents);
    let server = FileServer::start(&path, Behaviour::default());
    let store = ModelStore::new(root.path());
    let mut asset = asset(&server, &contents);
    asset.files[0].sha256 = digest(b"something else");
    let error = store.ensure(&asset, &mut |_| {}).unwrap_err();
    assert!(matches!(error, SpeechError::Checksum { .. }), "{error}");
    assert_eq!(names(&store.directory(&asset)), Vec::<String>::new());
    assert!(!store.is_installed(&asset));
}

#[test]
fn a_mirror_serves_every_file_from_asset_id_and_file_name() {
    let contents = body(5_000);
    let (_served, path, root) = fixture(&contents);
    let server = FileServer::start(&path, Behaviour::default());
    let mut asset = asset(&server, &contents);
    // The source is unreachable, or absent altogether: the mirror wins.
    asset.files[0].source = Some(ModelSource::Url("http://127.0.0.1:9/never".to_owned()));
    let store = ModelStore::new(root.path()).with_mirror(Some(format!("{}/", server.base)));
    store.ensure(&asset, &mut |_| {}).unwrap();
    store.verify(&asset).unwrap();
    store.remove(&asset).unwrap();
    asset.files[0].source = None;
    store.ensure(&asset, &mut |_| {}).unwrap();
    store.verify(&asset).unwrap();
    assert!(
        server
            .seen()
            .iter()
            .all(|s| s.path == format!("{ID}/{NAME}"))
    );
    assert_eq!(server.seen().len(), 2);
}

#[test]
fn a_partial_another_download_holds_is_left_alone() {
    let contents = body(20_000);
    let (_served, path, root) = fixture(&contents);
    let server = FileServer::start(&path, Behaviour::default());
    let store = ModelStore::new(root.path());
    let asset = asset(&server, &contents);
    let directory = store.directory(&asset);
    fs::create_dir_all(&directory).unwrap();
    let partial = directory.join("model.onnx.partial");
    fs::write(&partial, &contents[..7_000]).unwrap();
    let held = File::options().write(true).open(&partial).unwrap();
    held.lock().unwrap();
    store.ensure(&asset, &mut |_| {}).unwrap();
    store.verify(&asset).unwrap();
    assert_eq!(server.seen()[0].range, None);
    drop(held);
    assert_eq!(fs::read(&partial).unwrap(), &contents[..7_000]);
    assert_eq!(names(&directory), [NAME, "model.onnx.partial"]);
}

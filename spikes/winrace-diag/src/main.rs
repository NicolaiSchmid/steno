//! Diagnostic: which step of the durable replace fails under parallel
//! writers on Windows, and with which code.
#![allow(clippy::all)]

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    imp::main();
}

#[cfg(windows)]
mod imp {
    use std::collections::{BTreeMap, BTreeSet, HashMap};
    use std::ffi::c_void;
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write as _};
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::{Duration, Instant};

    unsafe extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
        fn SetFileInformationByHandle(h: *mut c_void, class: i32, info: *const c_void, size: u32) -> i32;
    }
    const REPLACE: u32 = 1;
    const WT: u32 = 8;
    const BACKUP: u32 = 0x0200_0000;
    const APPEND: u32 = 4;
    const DELETE: u32 = 0x0001_0000;
    const SHARE_RW: u32 = 3;

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain([0]).collect()
    }
    fn move_ex(from: &Path, to: &Path, flags: u32) -> io::Result<()> {
        let (f, t) = (wide(from), wide(to));
        if unsafe { MoveFileExW(f.as_ptr(), t.as_ptr(), flags) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    fn c(e: &io::Error) -> String {
        format!("{:?}", e.raw_os_error())
    }

    type Log = Mutex<BTreeMap<String, usize>>;
    fn note(log: &Log, key: String) {
        *log.lock().unwrap().entry(key).or_default() += 1;
    }

    fn retried<T>(
        log: &Log,
        label: &str,
        busy: impl Fn(&io::Error) -> bool,
        mut a: impl FnMut() -> io::Result<T>,
    ) -> io::Result<T> {
        for _ in 0..49 {
            match a() {
                Err(e) if busy(&e) => {
                    note(log, format!("  {label} retried code={}", c(&e)));
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    note(log, format!("  {label} gave up code={}", c(&e)));
                    return Err(e);
                }
                o => return o,
            }
        }
        a().inspect_err(|e| note(log, format!("  {label} exhausted code={}", c(e))))
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum V {
        Current,
        NoWriteThrough,
        WriteThroughNoReopen,
        Locked,
        RetryOnly,
        Fixed,
    }

    #[derive(Default)]
    struct Locks(Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>);
    impl Locks {
        fn get(&self, p: &Path) -> Arc<Mutex<()>> {
            self.0.lock().unwrap().entry(p.to_path_buf()).or_default().clone()
        }
    }

    fn rename_over(v: V, log: &Log, from: &Path, to: &Path) -> Result<(), (&'static str, io::Error)> {
        let mut wt_ok = false;
        if v != V::NoWriteThrough {
            match move_ex(from, to, REPLACE | WT) {
                Ok(()) => {
                    wt_ok = true;
                    note(log, "  wt ok".into())
                }
                Err(e) => note(
                    log,
                    format!("  wt failed code={} from_exists={}", c(&e), from.exists()),
                ),
            }
        }
        if !wt_ok {
            let busy = |e: &io::Error| if matches!(v, V::RetryOnly | V::Fixed) { matches!(e.raw_os_error(), Some(5 | 32 | 33)) } else { e.kind() == io::ErrorKind::PermissionDenied };
            retried(log, "std rename", busy, || {
                std::fs::rename(from, to)
            })
            .map_err(|e| ("std rename", e))?;
        }
        if v == V::WriteThroughNoReopen && wt_ok {
            return Ok(());
        }
        let busy = |e: &io::Error| if matches!(v, V::RetryOnly | V::Fixed) { matches!(e.raw_os_error(), Some(5 | 32 | 33)) } else { e.raw_os_error() == Some(32) };
        let f = retried(log, "reopen", busy, || {
            OpenOptions::new().write(true).open(to)
        })
        .map_err(|e| ("reopen", e))?;
        f.sync_all().map_err(|e| ("flush file", e))?;
        if !wt_ok { note(log, "  std renamed and flushed".into()); }
        Ok(())
    }

    fn flush_dir(d: &Path) -> io::Result<()> {
        let f = OpenOptions::new().access_mode(APPEND).custom_flags(BACKUP).open(d)?;
        match f.sync_all() {
            Err(e) if matches!(e.raw_os_error(), Some(1 | 50 | 5 | 87 | 6)) => Ok(()),
            o => o,
        }
    }

    fn replace(v: V, log: &Log, locks: &Locks, path: &Path, data: &[u8]) -> io::Result<()> {
        let dir = path.parent().unwrap();
        let prefix = format!(".{}.", path.file_name().unwrap().to_string_lossy());
        let mut t = tempfile::Builder::new()
            .prefix(&prefix)
            .suffix(".partial")
            .tempfile_in(dir)
            .inspect_err(|e| note(log, format!("FINAL create code={}", c(e))))?;
        t.write_all(data).inspect_err(|e| note(log, format!("FINAL write code={}", c(e))))?;
        t.as_file().sync_all().inspect_err(|e| note(log, format!("FINAL sync temp code={}", c(e))))?;
        let (f, tmp) = t.keep().map_err(|e| e.error).inspect_err(|e| note(log, format!("FINAL keep code={}", c(e))))?;
        drop(f);
        let lock = locks.get(path);
        let _g = matches!(v, V::Locked | V::Fixed).then(|| lock.lock().unwrap());
        if let Err((stage, e)) = rename_over(v, log, &tmp, path) {
            note(
                log,
                format!(
                    "FINAL {stage} code={} from_exists={} to_exists={}",
                    c(&e),
                    tmp.exists(),
                    path.exists()
                ),
            );
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        flush_dir(dir).inspect_err(|e| note(log, format!("FINAL flush dir code={}", c(e))))
    }

    fn round(v: V, log: &Arc<Log>, locks: &Arc<Locks>) -> (usize, bool, usize) {
        const WRITERS: usize = 8;
        const WRITES: usize = 25;
        let dir = tempfile::tempdir().unwrap();
        let files = [dir.path().join("a.json"), dir.path().join("b.json")];
        let payload = |file: usize, writer: usize, write: usize| {
            format!("{{\"f\":{file},\"w\":{writer},\"n\":{write},\"p\":\"{}\"}}", "x".repeat(4096 * (writer + 1)))
        };
        let start = Arc::new(Barrier::new(WRITERS * 2));
        let mut threads = Vec::new();
        for (file, path) in files.iter().enumerate() {
            for writer in 0..WRITERS {
                let (path, start, log, locks) = (path.clone(), start.clone(), log.clone(), locks.clone());
                threads.push(std::thread::spawn(move || {
                    start.wait();
                    let mut failed = 0;
                    for write in 0..WRITES {
                        if replace(v, &log, &locks, &path, payload(file, writer, write).as_bytes()).is_err() {
                            failed += 1;
                        }
                    }
                    failed
                }));
            }
        }
        let failed: usize = threads.into_iter().map(|t| t.join().unwrap()).sum();
        let mut whole = true;
        for (file, path) in files.iter().enumerate() {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let all: BTreeSet<String> = (0..WRITERS)
                .flat_map(|w| (0..WRITES).map(move |n| payload(file, w, n)))
                .collect();
            whole &= all.contains(&text);
        }
        let left = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".partial"))
            .count();
        (failed, whole, left)
    }

    fn probe(label: &str, f: impl FnOnce() -> String) {
        println!("PROBE {label}: {}", f());
    }

    fn probes() {
        let d = tempfile::tempdir().unwrap();
        let to = d.path().join("t.json");
        let from = d.path().join("f.partial");
        let fresh = |name: &str, bytes: &[u8]| {
            std::fs::write(&to, b"old").unwrap();
            std::fs::write(&from, bytes).unwrap();
            name.to_owned()
        };
        let r = |x: io::Result<()>| match x {
            Ok(()) => "ok".to_owned(),
            Err(e) => format!("err {}", c(&e)),
        };
        let ro = |x: io::Result<File>| match x {
            Ok(_) => "ok".to_owned(),
            Err(e) => format!("err {}", c(&e)),
        };
        // Target held without share-delete.
        fresh("", b"new");
        let h = OpenOptions::new().read(true).share_mode(SHARE_RW).open(&to).unwrap();
        probe("target open no-share-delete: wt", || r(move_ex(&from, &to, REPLACE | WT)));
        probe("target open no-share-delete: std", || r(std::fs::rename(&from, &to)));
        probe("target open no-share-delete: reopen write", || ro(OpenOptions::new().write(true).open(&to)));
        drop(h);
        // Target held share-all.
        fresh("", b"new");
        let h = OpenOptions::new().read(true).open(&to).unwrap();
        probe("target open share-all: wt", || r(move_ex(&from, &to, REPLACE | WT)));
        probe("target open share-all: std", || r(std::fs::rename(&from, &to)));
        drop(h);
        // Target held share-all for write (a flush handle).
        fresh("", b"new");
        let h = OpenOptions::new().write(true).open(&to).unwrap();
        probe("target open share-all write: wt", || r(move_ex(&from, &to, REPLACE | WT)));
        probe("target open share-all write: std", || r(std::fs::rename(&from, &to)));
        drop(h);
        // Source held share-all.
        fresh("", b"new");
        let h = OpenOptions::new().read(true).open(&from).unwrap();
        probe("source open share-all: wt", || r(move_ex(&from, &to, REPLACE | WT)));
        probe("source open share-all: std", || r(std::fs::rename(&from, &to)));
        drop(h);
        // Source held no-share-delete.
        fresh("", b"new");
        let h = OpenOptions::new().read(true).share_mode(SHARE_RW).open(&from).unwrap();
        probe("source open no-share-delete: wt", || r(move_ex(&from, &to, REPLACE | WT)));
        probe("source open no-share-delete: std", || r(std::fs::rename(&from, &to)));
        drop(h);
        // Target delete-pending (legacy disposition).
        fresh("", b"new");
        let h = OpenOptions::new().access_mode(DELETE).open(&to).unwrap();
        let one = 1u8;
        let set = unsafe { SetFileInformationByHandle(h.as_raw_handle(), 4, (&one as *const u8).cast(), 1) };
        probe("delete-pending set", || format!("{set} {}", c(&io::Error::last_os_error())));
        probe("delete-pending: exists", || format!("{}", to.exists()));
        probe("delete-pending: reopen write", || ro(OpenOptions::new().write(true).open(&to)));
        probe("delete-pending: wt", || r(move_ex(&from, &to, REPLACE | WT)));
        probe("delete-pending: std", || r(std::fs::rename(&from, &to)));
        drop(h);
        probe("delete-pending after close: exists, content", || {
            format!("{} {:?}", to.exists(), std::fs::read(&to).ok().map(|b| String::from_utf8_lossy(&b).into_owned()))
        });
    }

    pub fn main() {
        if std::env::var("PROBES").is_ok() { probes(); }
        let rounds: usize = std::env::var("ROUNDS").ok().and_then(|s| s.parse().ok()).unwrap_or(15);
        let par: usize = std::env::var("PAR").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
        let variants: Vec<V> = match std::env::var("VARIANTS").as_deref() {
            Ok("diag2") => vec![V::Current, V::RetryOnly, V::Fixed],
            _ => vec![V::Current, V::NoWriteThrough, V::WriteThroughNoReopen, V::Locked],
        };
        for v in variants {
            let log = Arc::new(Log::default());
            let locks = Arc::new(Locks::default());
            let (mut failed, mut torn, mut left) = (0, 0, 0);
            let t = Instant::now();
            for _ in 0..rounds.div_ceil(par) {
                let handles: Vec<_> = (0..par).map(|_| { let (log, locks) = (log.clone(), locks.clone()); std::thread::spawn(move || round(v, &log, &locks)) }).collect();
                for h in handles {
                    let (f, whole, l) = h.join().unwrap();
                    failed += f;
                    torn += usize::from(!whole);
                    left += l;
                }
            }
            println!("VARIANT {v:?}: rounds={rounds} failed_writes={failed} torn_or_lost_files={torn} temporaries_left={left} secs={:.1}", t.elapsed().as_secs_f64());
            for (k, n) in log.lock().unwrap().iter() {
                println!("    {n:>6} {k}");
            }
        }
    }
}

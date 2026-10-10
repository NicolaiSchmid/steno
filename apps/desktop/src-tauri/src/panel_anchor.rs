//! Where the panels' anchor is kept: `panel-anchor.json` in the support
//! directory (`steno_core::StenoPaths`), which no identifier names (D5 of
//! `.plans/2026-10-07-stable-promotion.md`: decouple, move nothing).
//!
//! A build under the earlier identifier (`identifier::EARLIER`) kept it
//! in that identifier's app config directory: on macOS
//! `~/Library/Application Support/uno.schmid.steno.desktop`, on Linux
//! `$XDG_CONFIG_HOME` (else `~/.config`) and the identifier, on Windows
//! `%APPDATA%\uno.schmid.steno.desktop`. While the new file is missing,
//! that one is read once and written to the new place, so from then on
//! only the new file counts; the earlier file stays where it is, for a
//! rollback.
//!
//! Every write replaces the whole file (`files::write_json`, atomic and
//! durable). A drag's saves run off the main thread, the latest anchor of
//! a drag winning ([`AnchorFile::save_in_background`]), and the exit waits
//! for the last one ([`AnchorFile::flush`]); the one-time copy is written
//! when the anchor loads ([`AnchorFile::load`]). A new file that does not parse is set aside
//! (`files::set_aside`, `panel-anchor.json.corrupt-<time>`) before
//! anything is written, and the panels open at the default place in that
//! run; until a drag saves a new file, the next launch reads the earlier
//! build's file again, as when the new one was missing. One that cannot
//! be read, or set aside, is never written in this run.
//!
//! Swift: `FloatingPanelModel.anchorKey` (`steno.floatingPanel.anchor` in
//! the Swift app's defaults, which nothing imports).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use steno_services::files;

use crate::identifier;
use crate::panel_geometry::PanelAnchor;

/// The file's name, in the support directory and in the earlier config
/// directory alike.
const FILE_NAME: &str = "panel-anchor.json";

/// The anchor's file, and the earlier one to read once.
pub struct AnchorFile {
    path: PathBuf,
    earlier: Option<PathBuf>,
    /// False once the file turned out unreadable, or could not be set
    /// aside: the anchor then lives in memory for this run.
    writable: AtomicBool,
    /// The background writer ([`Self::save_in_background`]), started with
    /// the first save.
    writer: OnceLock<Sender<Write>>,
}

/// What the background writer is asked.
enum Write {
    Save(PanelAnchor),
    /// Answer once every anchor queued before is on disk.
    Flush(Sender<()>),
}

/// How long the exit waits for the writer ([`AnchorFile::flush`]): far
/// above one save (about 150 ms on a slow disk), and short beside the
/// time the system gives a quitting app.
const FLUSH_PATIENCE: Duration = Duration::from_secs(2);

// A patience under a second would let the exit go before a slow disk's
// save, and a zero one would make the flush wait for nothing.
const _: () = assert!(FLUSH_PATIENCE.as_millis() >= 1000);

impl AnchorFile {
    /// The file in `support_directory`, and the earlier build's under
    /// `config_directory`, the platform's base config directory (Tauri's
    /// `PathResolver::config_dir`, which the earlier build's app config
    /// directory hung from), when there is one.
    pub fn new(support_directory: &Path, config_directory: Option<&Path>) -> Self {
        AnchorFile {
            path: support_directory.join(FILE_NAME),
            earlier: config_directory.map(|dir| dir.join(identifier::EARLIER).join(FILE_NAME)),
            writable: AtomicBool::new(true),
            writer: OnceLock::new(),
        }
    }

    /// The saved anchor. The new file when it is there: its anchor, or none
    /// when it does not parse (it is set aside first) or cannot be read.
    /// Without it, the earlier build's anchor, written to the new place at
    /// once; none when neither holds one.
    pub fn load(&self) -> Option<PanelAnchor> {
        match self.path.symlink_metadata() {
            Ok(_) => {
                let (anchor, writable) = files::read_json::<Option<PanelAnchor>>(&self.path);
                self.writable.store(writable, Ordering::Relaxed);
                anchor
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let earlier = self.earlier.as_deref()?;
                let anchor = read_earlier(earlier)?;
                tracing::warn!(
                    "the panels' anchor read from {}, saved to {}",
                    earlier.display(),
                    self.path.display()
                );
                self.save(anchor);
                Some(anchor)
            }
            Err(error) => {
                tracing::warn!(
                    "{} could not be read ({error}); it stays and is not written",
                    self.path.display()
                );
                self.writable.store(false, Ordering::Relaxed);
                None
            }
        }
    }

    /// Replaces the file with `anchor`, unless it may not be written.
    pub fn save(&self, anchor: PanelAnchor) {
        if !self.writable.load(Ordering::Relaxed) {
            tracing::debug!("the panels' anchor is kept in memory only");
            return;
        }
        if let Err(error) = files::write_json(&self.path, &anchor) {
            tracing::debug!(%error, "the panels' anchor could not be saved");
        }
    }

    /// [`Self::save`] on a thread of its own, so a drag's moves never wait
    /// for the disk: each write takes the latest anchor queued by then. If
    /// the thread cannot start, the save runs here.
    pub fn save_in_background(&'static self, anchor: PanelAnchor) {
        let writer = self.writer.get_or_init(|| {
            let (writer, requests) = channel();
            let started = std::thread::Builder::new()
                .name("panel-anchor".to_owned())
                .spawn(move || self.write_all(&requests));
            if let Err(error) = started {
                tracing::debug!(%error, "the panels' anchor is saved on the calling thread");
            }
            writer
        });
        if writer.send(Write::Save(anchor)).is_err() {
            self.save(anchor);
        }
    }

    /// Waits until every anchor queued so far is on disk, at most
    /// [`FLUSH_PATIENCE`]; at once when nothing was ever queued. The exit
    /// calls it after the shutdown, so a drag just before a quit or a
    /// logout is not lost.
    pub fn flush(&self) {
        self.flush_within(FLUSH_PATIENCE);
    }

    fn flush_within(&self, patience: Duration) {
        let Some(writer) = self.writer.get() else {
            return;
        };
        let (done, written) = channel();
        if writer.send(Write::Flush(done)).is_ok() && written.recv_timeout(patience).is_err() {
            tracing::debug!("the panels' anchor was not saved before the exit");
        }
    }

    /// The writer thread: each write takes the latest anchor queued by then
    /// ([`batch`]), and a flush is answered once the anchors before it are
    /// written.
    fn write_all(&self, requests: &Receiver<Write>) {
        while let Ok(first) = requests.recv() {
            let (anchor, flushes) = batch(first, requests);
            if let Some(anchor) = anchor {
                self.save(anchor);
            }
            for done in flushes {
                let _ = done.send(());
            }
        }
    }
}

/// The anchor in the earlier build's file at `path`; none when it is
/// missing, unreadable or does not parse. That file is never changed.
fn read_earlier(path: &Path) -> Option<PanelAnchor> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// `first` and the requests queued after it: the last anchor among them,
/// none when only flushes came, and the flushes, which wait for that
/// anchor's write.
fn batch(first: Write, queued: &Receiver<Write>) -> (Option<PanelAnchor>, Vec<Sender<()>>) {
    let mut anchor = None;
    let mut flushes = Vec::new();
    for request in std::iter::once(first).chain(queued.try_iter()) {
        match request {
            Write::Save(later) => anchor = Some(later),
            Write::Flush(done) => flushes.push(done),
        }
    }
    (anchor, flushes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel_geometry::Rect;

    fn anchor(x: f64) -> PanelAnchor {
        PanelAnchor {
            top_center: (x, 30.0),
            screen: Rect::new(0.0, 0.0, 1440.0, 900.0),
        }
    }

    struct Dirs {
        _root: tempfile::TempDir,
        support: PathBuf,
        config: PathBuf,
    }

    impl Dirs {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let (support, config) = (root.path().join("Steno"), root.path().join("config"));
            Dirs {
                _root: root,
                support,
                config,
            }
        }

        fn file(&self) -> AnchorFile {
            AnchorFile::new(&self.support, Some(&self.config))
        }

        fn new_path(&self) -> PathBuf {
            self.support.join(FILE_NAME)
        }

        fn earlier_path(&self) -> PathBuf {
            self.config.join("uno.schmid.steno.desktop").join(FILE_NAME)
        }

        fn write(path: &Path, bytes: &[u8]) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }

        fn write_earlier(&self, anchor: PanelAnchor) -> Vec<u8> {
            let bytes = serde_json::to_vec(&anchor).unwrap();
            Self::write(&self.earlier_path(), &bytes);
            bytes
        }

        fn set_aside(&self) -> Vec<PathBuf> {
            std::fs::read_dir(&self.support)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.to_string_lossy().contains(".corrupt-"))
                .collect()
        }
    }

    /// The new file wins over the earlier one, which is not read.
    #[test]
    fn the_new_file_is_read_when_it_is_there() {
        let dirs = Dirs::new();
        dirs.write_earlier(anchor(100.0));
        dirs.file().save(anchor(200.0));
        assert_eq!(dirs.file().load(), Some(anchor(200.0)));
    }

    /// Only the earlier file: read once, written to the new place, and left
    /// where it was; the next launch reads the new file.
    #[test]
    fn the_earlier_file_is_read_once_and_saved_to_the_new_place() {
        let dirs = Dirs::new();
        let earlier = dirs.write_earlier(anchor(100.0));
        assert_eq!(dirs.file().load(), Some(anchor(100.0)));
        let saved: PanelAnchor =
            serde_json::from_slice(&std::fs::read(dirs.new_path()).unwrap()).unwrap();
        assert_eq!(saved, anchor(100.0));
        assert_eq!(std::fs::read(dirs.earlier_path()).unwrap(), earlier);

        let file = dirs.file();
        file.save(anchor(300.0));
        dirs.write_earlier(anchor(400.0));
        assert_eq!(file.load(), Some(anchor(300.0)));
    }

    #[test]
    fn neither_file_is_no_anchor() {
        let dirs = Dirs::new();
        assert_eq!(dirs.file().load(), None);
        assert!(!dirs.new_path().exists());
        assert_eq!(AnchorFile::new(&dirs.support, None).load(), None);
    }

    /// An earlier file that does not parse is no anchor, and stays as it
    /// is.
    #[test]
    fn a_damaged_earlier_file_is_no_anchor_and_stays() {
        let dirs = Dirs::new();
        Dirs::write(&dirs.earlier_path(), b"{\"top_center\":");
        assert_eq!(dirs.file().load(), None);
        assert_eq!(
            std::fs::read(dirs.earlier_path()).unwrap(),
            b"{\"top_center\":"
        );
        assert!(!dirs.new_path().exists());
    }

    /// A new file that does not parse is set aside, byte for byte, before
    /// anything is written; the panels open at the default place, not at
    /// the earlier build's anchor, and the next save writes a new file
    /// beside the copy.
    #[test]
    fn a_corrupt_new_file_is_set_aside_and_the_default_place_used() {
        let dirs = Dirs::new();
        dirs.write_earlier(anchor(100.0));
        Dirs::write(&dirs.new_path(), b"not json");
        let file = dirs.file();
        assert_eq!(file.load(), None);
        let aside = dirs.set_aside();
        assert_eq!(aside.len(), 1, "{aside:?}");
        assert_eq!(std::fs::read(&aside[0]).unwrap(), b"not json");
        assert!(!dirs.new_path().exists());

        file.save(anchor(200.0));
        assert_eq!(dirs.file().load(), Some(anchor(200.0)));
        assert_eq!(std::fs::read(&aside[0]).unwrap(), b"not json");
    }

    /// A new file that cannot be read (here a folder in its place) is never
    /// written in this run.
    #[test]
    fn an_unreadable_new_file_is_never_written() {
        let dirs = Dirs::new();
        std::fs::create_dir_all(dirs.new_path()).unwrap();
        let file = dirs.file();
        assert_eq!(file.load(), None);
        file.save(anchor(200.0));
        assert!(dirs.new_path().is_dir());
        assert_eq!(dirs.set_aside(), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_write_takes_the_latest_anchor_queued() {
        let (queue, queued) = channel();
        assert_eq!(
            batch(Write::Save(anchor(1.0)), &queued).0,
            Some(anchor(1.0))
        );
        for x in [2.0, 3.0] {
            queue.send(Write::Save(anchor(x))).unwrap();
        }
        let (done, _written) = channel();
        queue.send(Write::Flush(done)).unwrap();
        queue.send(Write::Save(anchor(4.0))).unwrap();
        let (latest_anchor, flushes) = batch(Write::Save(anchor(1.0)), &queued);
        assert_eq!(latest_anchor, Some(anchor(4.0)));
        assert_eq!(flushes.len(), 1);
        assert!(queued.try_recv().is_err());

        let (done, _written) = channel();
        let (no_anchor, flushes) = batch(Write::Flush(done), &queued);
        assert_eq!((no_anchor, flushes.len()), (None, 1));
    }

    /// A drag of 60 moves saved in the background, then a flush: once it
    /// returns the file holds the last move, never an earlier one. The drag's
    /// wait has a minute's patience, so a slow runner's disk cannot fail the
    /// test; the last move's wait is the exit's own [`AnchorFile::flush`],
    /// whose patience one write fits.
    #[test]
    fn the_flush_returns_once_the_last_anchor_of_a_drag_is_on_disk() {
        let patience = Duration::from_secs(60);
        let dirs = Dirs::new();
        let file: &'static AnchorFile = Box::leak(Box::new(dirs.file()));
        file.flush();
        assert!(!dirs.new_path().exists());
        for x in 1..=60 {
            file.save_in_background(anchor(f64::from(x)));
        }
        file.flush_within(patience);
        assert_eq!(dirs.file().load(), Some(anchor(60.0)));
        file.save_in_background(anchor(61.0));
        file.flush();
        assert_eq!(dirs.file().load(), Some(anchor(61.0)));
    }
}

//! The speakers' sample clips on the disk: each run writes its own files,
//! the merge names them, and only then are the files no speaker row names
//! removed.
//!
//! A run writes each clip to `speakers/<SPEAKER-UUID>-<RUN-UUID>.wav`
//! ([`RecordingLayout::run_sample_clip`]), a name no other run uses, and
//! syncs it and the folder before the merge (`write`). The merge's
//! `Store::replace_transcript` switches the speaker rows' `sampleClipURL`s
//! to the new files in the transaction that keeps their confirmations, and
//! commits durably. Only after that commit does `sweep` remove the clip
//! files no speaker row names: the clips the earlier rows named, and the
//! files of a run that ended before its commit (a crash, a failed write, a
//! failed merge). So at every point each speaker row names a whole clip of
//! the run that wrote the row, and a confirmed speaker's earlier clip is
//! removed only once a durable commit names the new one:
//!
//! | The run ends | The rows name | Left over, for a later sweep |
//! |--------------|---------------|------------------------------|
//! | while writing the clips | the earlier clips | the new files written so far |
//! | before the merge commits | the earlier clips | every new file |
//! | after the commit, before the sweep | the new clips | the earlier clips |
//! | during the sweep | the new clips | the earlier clips not yet removed |
//!
//! The sweep runs only inside a run, after its merge, on the meeting's own
//! folder ([`RecordingLayout::own_folder`]), while the run holds the
//! meeting in the pipeline's in-flight set: no other run writes into that
//! folder then, so no clip is removed between its write and its commit. A
//! master in a folder of other files is never swept. The clips Swift wrote
//! (`speakers/<SPEAKER-UUID>.wav`) stay while a row names them. Rust only:
//! Swift writes each clip in place, over the file a row may name.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use steno_core::{
    AudioBuffer16k, RecordingLayout, Store, StoreError, busy_file, paths::file_url_path,
};

use crate::files::{self, Access};
use crate::fixtures::{int16, wav_data};

/// A point in a run's clip handling where a test fails the step or ends
/// the run as a crash would ([`ClipProbe`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipStep {
    /// The clip at this index is on the disk under its run's name.
    Written(usize),
    /// Every clip is written; the merge is about to commit.
    Merging,
    /// The merge committed; the sweep is about to remove files.
    Sweeping,
    /// The sweep removed its file at this index.
    Removed(usize),
}

/// Called at each [`ClipStep`] (`PipelineDependencies::with_clip_probe`).
/// An error is the step's failure: the write of that clip fails, the merge
/// fails, the sweep is skipped, or it stops after that removal. A panic
/// ends the run there, as a crash would.
pub type ClipProbe = Arc<dyn Fn(ClipStep) -> std::io::Result<()> + Send + Sync>;

/// Calls `probe`, when there is one, at `step`.
pub(crate) fn reach(probe: Option<&ClipProbe>, step: ClipStep) -> std::io::Result<()> {
    probe.map_or(Ok(()), |probe| probe(step))
}

/// Writes each of `clips` to its path, a run's own name under `layout`'s
/// `speakers/` that no file has yet, durably: the folder is created with
/// its entry synced, and each file is synced and its folder entry with it
/// ([`files::replace_file`]), so a commit that names them names files on
/// the disk. A failure removes the files this call wrote and leaves every
/// other file alone, so the clips the speaker rows name stay playable.
pub(crate) fn write(
    layout: &RecordingLayout,
    clips: &[(PathBuf, AudioBuffer16k)],
    probe: Option<&ClipProbe>,
) -> std::io::Result<()> {
    files::create_dir_all_durably(&layout.speakers_directory())?;
    for (index, (path, clip)) in clips.iter().enumerate() {
        let data = wav_data(&int16(&clip.samples), 16_000, 1);
        let written = files::replace_file(path, &data, Access::Default)
            .and_then(|()| reach(probe, ClipStep::Written(index)));
        if let Err(error) = written {
            for (written, _) in &clips[..=index] {
                let _ = std::fs::remove_file(written);
            }
            return Err(error);
        }
    }
    Ok(())
}

/// Removes every clip file in `directory` that no speaker row of any
/// meeting names, by file name, and returns the files removed: WAV files
/// and the temporaries of an unfinished write. On Windows a file another
/// handle holds is tried again for a moment ([`busy_file::retried`]); a
/// file that cannot be removed is skipped and left for the next sweep. Reads the rows first and
/// removes nothing when that read fails. The caller holds the meeting the
/// folder belongs to in the in-flight set (see the module doc).
pub(crate) fn sweep(
    store: &Store,
    directory: &Path,
    probe: Option<&ClipProbe>,
) -> Result<Vec<PathBuf>, StoreError> {
    let named: BTreeSet<OsString> = store
        .sample_clip_urls()?
        .iter()
        .filter_map(|url| file_url_path(url)?.file_name().map(ToOwned::to_owned))
        .collect();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(Vec::new());
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if named.contains(&name)
            || !is_clip_file(&name)
            || !entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            continue;
        }
        let path = entry.path();
        match busy_file::retried(|| std::fs::remove_file(&path)) {
            Ok(()) => {
                let index = removed.len();
                removed.push(path);
                if reach(probe, ClipStep::Removed(index)).is_err() {
                    break;
                }
            }
            Err(error) => tracing::warn!(%error, "a sample clip no speaker names stays"),
        }
    }
    Ok(removed)
}

/// A clip (`.wav`) or the temporary of an unfinished clip write:
/// `.<name>.<random>.partial` from [`files::replace_file`], which [`write`]
/// uses, and `.wav.partial` and `.wav.part` from the writers before it.
fn is_clip_file(name: &OsString) -> bool {
    let name = name.to_string_lossy();
    [".wav", ".partial", ".part"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use steno_core::paths::file_url;
    use steno_core::testing::sample_data;
    use steno_core::{Speaker, SpeakerAssignment};
    use uuid::Uuid;

    use super::*;

    fn speaker(meeting_id: Uuid, label: &str, clip: Option<&Path>) -> Speaker {
        Speaker {
            id: Uuid::new_v4(),
            meeting_id,
            cluster_label: label.to_owned(),
            assignment: SpeakerAssignment::Unknown,
            embedding: None,
            sample_clip_range: None,
            sample_clip_url: clip.map(|path| file_url(path, false)),
            cluster_confidence: 1.0,
        }
    }

    /// The sweep keeps every file a row names, a run's new clip and an old
    /// fixed-name clip alike, also one a row of another meeting names, and
    /// removes the unnamed clips and temporaries only; other files stay.
    #[test]
    fn the_sweep_removes_only_the_clip_files_no_row_names() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("steno.sqlite")).unwrap();
        let meeting = sample_data::meeting();
        store.save_meeting(&meeting).unwrap();
        let mut other = sample_data::meeting();
        other.id = Uuid::new_v4();
        store.save_meeting(&other).unwrap();
        let layout = RecordingLayout::new(&dir.path().join("audio"), meeting.id);
        layout.create_directories(true).unwrap();
        let (run, earlier) = (Uuid::new_v4(), Uuid::new_v4());
        let fixed = layout.sample_clip(Uuid::new_v4());
        let current = layout.run_sample_clip(Uuid::new_v4(), run);
        let elsewhere = layout.run_sample_clip(Uuid::new_v4(), run);
        let replaced = layout.run_sample_clip(Uuid::new_v4(), earlier);
        // As `files::replace_file` names its temporary.
        let temporary = layout
            .speakers_directory()
            .join(format!(".{}-{run}.wav.a1b2c3.partial", Uuid::new_v4()));
        let unrelated = layout.speakers_directory().join("notes.txt");
        for path in [
            &fixed, &current, &elsewhere, &replaced, &temporary, &unrelated,
        ] {
            std::fs::write(path, b"clip").unwrap();
        }
        store
            .save_speaker(&speaker(meeting.id, "Speaker 1", Some(&fixed)))
            .unwrap();
        store
            .save_speaker(&speaker(meeting.id, "Speaker 2", Some(&current)))
            .unwrap();
        store
            .save_speaker(&speaker(other.id, "Speaker 1", Some(&elsewhere)))
            .unwrap();
        store
            .save_speaker(&speaker(meeting.id, "Speaker 3", None))
            .unwrap();

        let mut removed = sweep(&store, &layout.speakers_directory(), None).unwrap();
        removed.sort();
        let mut expected = vec![replaced.clone(), temporary.clone()];
        expected.sort();
        assert_eq!(removed, expected);
        for kept in [&fixed, &current, &elsewhere, &unrelated] {
            assert!(kept.exists(), "{}", kept.display());
        }
        assert!(
            sweep(&store, &dir.path().join("missing"), None)
                .unwrap()
                .is_empty(),
            "a folder that is not there has nothing to sweep"
        );
    }

    /// A write that fails partway removes the files it wrote and leaves
    /// the clip a row names as it was.
    #[test]
    fn a_failed_write_removes_its_own_files_and_keeps_the_earlier_clip() {
        let dir = tempfile::tempdir().unwrap();
        let layout = RecordingLayout::new(dir.path(), Uuid::new_v4());
        layout.create_directories(true).unwrap();
        let earlier = layout.sample_clip(Uuid::new_v4());
        std::fs::write(&earlier, b"earlier clip").unwrap();
        let run = Uuid::new_v4();
        let first = layout.run_sample_clip(Uuid::new_v4(), run);
        // A folder where the second clip goes: its write fails.
        let blocked = layout.run_sample_clip(Uuid::new_v4(), run);
        std::fs::create_dir(&blocked).unwrap();
        let clip = AudioBuffer16k::new(vec![0.25; 1600]);
        let clips = vec![(first.clone(), clip.clone()), (blocked.clone(), clip)];

        assert!(write(&layout, &clips, None).is_err());
        assert!(
            !first.exists(),
            "the clip written before the failure is removed"
        );
        assert_eq!(std::fs::read(&earlier).unwrap(), b"earlier clip");
    }
}

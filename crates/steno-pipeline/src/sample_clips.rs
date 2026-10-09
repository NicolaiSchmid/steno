//! The speakers' sample clips on the disk: each run writes its own files,
//! the merge names them, and only then are the files of the meeting's
//! speakers that no speaker row names removed.
//!
//! A run writes each clip to `speakers/<SPEAKER-UUID>-<RUN-UUID>.wav`
//! ([`RecordingLayout::run_sample_clip`]), a name no other run uses, and
//! syncs it and the folder before the merge (`write`). The merge's
//! `Store::replace_transcript` switches the speaker rows' `sampleClipURL`s
//! to the new files in the transaction that keeps their confirmations,
//! commits durably and returns the rows it replaced. Only after that commit
//! does `sweep_after_merge` remove the clip files of the meeting's speakers
//! that no speaker row names: the clips the earlier rows named, and the
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
//! A sweep removes a clip file only when all of these hold:
//! - The file belongs to one of the meeting's speakers, by the speaker id
//!   its name starts with: one this run wrote or one the merge replaced.
//!   Speaker ids derive from their meeting's id, so the clips of another
//!   meeting whose master lies in this folder are never the sweep's.
//! - No speaker row of any meeting names it, matched by file name, so a URL
//!   that spells the folder another way still keeps its clip.
//! - Its speaker is not a confirmed one this run gave no clip, and it is
//!   not the clip such a speaker's earlier row named: a confirmed speaker
//!   the re-run drops, or whose new row has no clip, keeps every file it
//!   had. Its row is gone or names no clip, as before per-run names, so the
//!   file stays on the disk, unnamed and not played; retention removes it
//!   with the audio while the meeting still has that speaker, and deleting
//!   the meeting removes it.
//!
//! The sweep runs only inside a run, after its merge, on the meeting's own
//! folder ([`RecordingLayout::own_folder`]), while the run holds the meeting
//! in the in-flight set, so no other run of the meeting has uncommitted
//! clips there. A meeting whose master is not in its own folder writes no
//! clip and sweeps nothing. When retention removes a meeting's audio, it
//! also removes the clip files of the meeting's speakers that no row names
//! ([`crate::retention`]). The clips Swift wrote
//! (`speakers/<SPEAKER-UUID>.wav`) stay while a row names them. Rust only:
//! Swift writes each clip in place, over the file a row may name.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use steno_core::{
    AudioBuffer16k, RecordingLayout, Speaker, Store, StoreError, busy_file, paths::file_url_path,
};
use uuid::Uuid;

use crate::files::{self, Access};
use crate::fixtures::{int16, wav_data};
use crate::pipeline::BACKGROUND_RUN_LOG;

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

/// The sweep of a run, once its merge committed `committed`, the rows this
/// run wrote, in place of `replaced`, the rows `Store::replace_transcript`
/// returned: [`sweep`] over the speakers of either, keeping every clip a
/// row now names. A confirmed speaker this run gave no clip keeps every
/// file of its own and the clip its earlier row named, which a merge into a
/// row without a clip may have left under another speaker's id (see the
/// module doc). Reads the rows first and removes nothing when that read
/// fails. The caller holds the meeting in the in-flight set.
pub(crate) fn sweep_after_merge(
    store: &Store,
    directory: &Path,
    replaced: &[Speaker],
    committed: &[Speaker],
    probe: Option<&ClipProbe>,
) -> Result<Vec<PathBuf>, StoreError> {
    let mut named = store.sample_clip_urls()?;
    let given_a_clip: BTreeSet<Uuid> = committed
        .iter()
        .filter(|speaker| speaker.sample_clip_url.is_some())
        .map(|speaker| speaker.id)
        .collect();
    let mut owners: BTreeSet<Uuid> = replaced
        .iter()
        .chain(committed)
        .map(|speaker| speaker.id)
        .collect();
    for kept in replaced
        .iter()
        .filter(|speaker| speaker.assignment.is_confirmed() && !given_a_clip.contains(&speaker.id))
    {
        owners.remove(&kept.id);
        named.extend(kept.sample_clip_url.clone());
    }
    Ok(sweep(directory, &owners, &named, probe))
}

/// Removes every clip file in `directory` of one of `owners`, by the
/// speaker id its name starts with, that none of the `named` URLs names, by
/// file name, and returns the files removed: WAV files and the temporaries
/// of an unfinished write. On Windows a file another handle holds is tried
/// again for a moment ([`busy_file::retried`]); a file that cannot be
/// removed is skipped and left for the next sweep.
pub(crate) fn sweep(
    directory: &Path,
    owners: &BTreeSet<Uuid>,
    named: &[String],
    probe: Option<&ClipProbe>,
) -> Vec<PathBuf> {
    let named: BTreeSet<OsString> = named
        .iter()
        .filter_map(|url| file_url_path(url)?.file_name().map(ToOwned::to_owned))
        .collect();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !clip_speaker(&name).is_some_and(|id| owners.contains(&id))
            || named.contains(&name)
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
            Err(error) => tracing::warn!(
                target: BACKGROUND_RUN_LOG,
                path = %path.display(),
                %error,
                "a sample clip no speaker row names stays"
            ),
        }
    }
    removed
}

/// The speaker a clip file belongs to, by the id its name starts with,
/// for a clip (`<SPEAKER-UUID>.wav`, `<SPEAKER-UUID>-<RUN-UUID>.wav`) or
/// the temporary of an unfinished clip write: `.<name>.<random>.partial`
/// from [`files::replace_file`], which [`write`] uses, and `.wav.partial`
/// and `.wav.part` from the writers before it. `None` for any other file.
fn clip_speaker(name: &OsStr) -> Option<Uuid> {
    let name = name.to_str()?;
    if ![".wav", ".partial", ".part"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
    {
        return None;
    }
    let id = name.strip_prefix('.').unwrap_or(name).get(..36)?;
    Uuid::try_parse(id).ok()
}

#[cfg(test)]
mod tests {
    use steno_core::paths::file_url;
    use steno_core::testing::sample_data;
    use steno_core::{SpeakerAssignment, StoreError};

    use super::*;

    fn speaker(meeting_id: Uuid, id: Uuid, confirmed: bool, clip: Option<&Path>) -> Speaker {
        Speaker {
            id,
            meeting_id,
            cluster_label: format!("SPEAKER_{id}"),
            assignment: if confirmed {
                SpeakerAssignment::Confirmed {
                    person_id: sample_data::person(0, "Anna").id,
                }
            } else {
                SpeakerAssignment::Unknown
            },
            embedding: None,
            sample_clip_range: None,
            sample_clip_url: clip.map(|path| file_url(path, false)),
            cluster_confidence: 1.0,
        }
    }

    fn url(path: &Path) -> String {
        file_url(path, false)
    }

    fn file_name(path: &Path) -> String {
        path.file_name().unwrap().to_string_lossy().into_owned()
    }

    /// Writes `b"clip"` to each of `paths`.
    fn put(paths: &[&PathBuf]) {
        for path in paths {
            std::fs::write(path, b"clip").unwrap();
        }
    }

    /// The sweep keeps every file a URL names, a run's new clip and an old
    /// fixed-name clip alike, and every file of a speaker it was not given
    /// (another meeting's whose master lies here), and removes the unnamed
    /// clips and temporaries of its speakers only; other files stay.
    #[test]
    fn the_sweep_removes_only_this_meetings_clips_no_row_names() {
        let dir = tempfile::tempdir().unwrap();
        let layout = RecordingLayout::new(dir.path(), Uuid::new_v4());
        layout.create_directories(true).unwrap();
        let (first, second, guest) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (run, earlier) = (Uuid::new_v4(), Uuid::new_v4());
        let fixed = layout.sample_clip(first);
        let current = layout.run_sample_clip(second, run);
        let replaced = layout.run_sample_clip(first, earlier);
        // As `files::replace_file` names its temporary.
        let temporary = layout.speakers_directory().join(format!(
            ".{}.a1b2c3.partial",
            file_name(&layout.run_sample_clip(second, earlier))
        ));
        let guests = layout.sample_clip(guest);
        let unrelated = layout.speakers_directory().join("notes.txt");
        put(&[&fixed, &current, &replaced, &temporary, &guests, &unrelated]);

        let owners = BTreeSet::from([first, second]);
        let named = [url(&fixed), url(&current)];
        let mut removed = sweep(&layout.speakers_directory(), &owners, &named, None);
        removed.sort();
        let mut expected = vec![replaced.clone(), temporary.clone()];
        expected.sort();
        assert_eq!(removed, expected);
        for kept in [&fixed, &current, &unrelated] {
            assert!(kept.exists(), "{}", kept.display());
        }
        assert!(
            guests.exists(),
            "another meeting's clip is never this sweep's"
        );
        assert!(
            sweep(&dir.path().join("missing"), &owners, &named, None).is_empty(),
            "a folder that is not there has nothing to sweep"
        );
    }

    /// A URL that spells the folder another way, through a link, still
    /// keeps the clip it names: the sweep matches by file name.
    #[cfg(unix)]
    #[test]
    fn a_url_through_another_spelling_of_the_folder_keeps_its_clip() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        let meeting = Uuid::new_v4();
        let layout = RecordingLayout::new(&audio, meeting);
        layout.create_directories(true).unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&audio, &alias).unwrap();
        let (id, run) = (Uuid::new_v4(), Uuid::new_v4());
        let clip = layout.run_sample_clip(id, run);
        put(&[&clip]);
        let spelled = RecordingLayout::new(&alias, meeting).run_sample_clip(id, run);
        assert_ne!(spelled, clip);
        assert!(spelled.exists());

        let removed = sweep(
            &layout.speakers_directory(),
            &BTreeSet::from([id]),
            &[url(&spelled)],
            None,
        );
        assert!(removed.is_empty(), "{removed:?}");
        assert!(clip.exists());
    }

    /// After the merge a confirmed speaker that the re-run dropped, or
    /// whose new row has no clip, keeps every file it had and the clip its
    /// row named under another speaker's id; a confirmed speaker given a new
    /// clip and an unconfirmed one lose their earlier clips, and every clip
    /// a row names stays.
    #[test]
    fn after_the_merge_a_confirmed_speaker_given_no_clip_keeps_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("steno.sqlite")).unwrap();
        let meeting = sample_data::meeting();
        store.save_meeting(&meeting).unwrap();
        let layout = RecordingLayout::new(&dir.path().join("audio"), meeting.id);
        layout.create_directories(true).unwrap();
        let (earlier, run) = (Uuid::new_v4(), Uuid::new_v4());
        let [clipless, dropped, renewed, unconfirmed, merged_into] =
            [(); 5].map(|()| Uuid::new_v4());
        let before = |id| layout.run_sample_clip(id, earlier);
        let now = |id| layout.run_sample_clip(id, run);
        let dropped_leftover = layout.run_sample_clip(dropped, Uuid::new_v4());
        // The clip a merge moved from the unconfirmed speaker to a
        // confirmed one without a clip.
        let moved = layout.run_sample_clip(unconfirmed, Uuid::new_v4());
        put(&[
            &before(clipless),
            &before(dropped),
            &dropped_leftover,
            &before(renewed),
            &before(unconfirmed),
            &now(renewed),
            &now(unconfirmed),
            &moved,
        ]);
        let replaced = [
            speaker(meeting.id, clipless, true, Some(&before(clipless))),
            speaker(meeting.id, dropped, true, Some(&before(dropped))),
            speaker(meeting.id, renewed, true, Some(&before(renewed))),
            speaker(meeting.id, unconfirmed, false, Some(&before(unconfirmed))),
            speaker(meeting.id, merged_into, true, Some(&moved)),
        ];
        // The rows the merge committed, as diarize wrote them.
        let committed = [
            speaker(meeting.id, clipless, false, None),
            speaker(meeting.id, renewed, false, Some(&now(renewed))),
            speaker(meeting.id, unconfirmed, false, Some(&now(unconfirmed))),
            speaker(meeting.id, merged_into, false, None),
        ];
        for row in &committed {
            store.save_speaker(row).unwrap();
        }

        let mut removed = sweep_after_merge(
            &store,
            &layout.speakers_directory(),
            &replaced,
            &committed,
            None,
        )
        .unwrap();
        removed.sort();
        let mut expected = vec![before(renewed), before(unconfirmed)];
        expected.sort();
        assert_eq!(removed, expected);
        for kept in [
            before(clipless),
            before(dropped),
            dropped_leftover,
            now(renewed),
            now(unconfirmed),
            moved,
        ] {
            assert!(kept.exists(), "{}", kept.display());
        }
    }

    /// A sweep whose read of the rows fails removes nothing.
    #[test]
    fn a_sweep_whose_store_read_fails_removes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("steno.sqlite")).unwrap();
        let meeting = sample_data::meeting();
        store.save_meeting(&meeting).unwrap();
        let layout = RecordingLayout::new(&dir.path().join("audio"), meeting.id);
        layout.create_directories(true).unwrap();
        let id = Uuid::new_v4();
        let unnamed = layout.run_sample_clip(id, Uuid::new_v4());
        put(&[&unnamed]);
        let replaced = [speaker(meeting.id, id, false, Some(&unnamed))];
        store
            .write(|transaction| {
                transaction
                    .execute_batch("ALTER TABLE speaker RENAME COLUMN sampleClipURL TO gone")?;
                Ok::<_, StoreError>(())
            })
            .unwrap();

        assert!(
            sweep_after_merge(&store, &layout.speakers_directory(), &replaced, &[], None).is_err()
        );
        assert!(unnamed.exists());
    }

    /// A write that fails partway removes the files it wrote and leaves
    /// the clip a row names as it was, the same speaker's included.
    #[test]
    fn a_failed_write_removes_its_own_files_and_keeps_the_earlier_clip() {
        let dir = tempfile::tempdir().unwrap();
        let layout = RecordingLayout::new(dir.path(), Uuid::new_v4());
        layout.create_directories(true).unwrap();
        let (id, run) = (Uuid::new_v4(), Uuid::new_v4());
        let earlier = layout.sample_clip(id);
        std::fs::write(&earlier, b"earlier clip").unwrap();
        let first = layout.run_sample_clip(id, run);
        // A folder where the second clip goes: its write fails.
        let blocked = layout.run_sample_clip(Uuid::new_v4(), run);
        std::fs::create_dir(&blocked).unwrap();
        let clip = AudioBuffer16k::new(vec![0.25; 1600]);
        let clips = vec![
            (first.clone(), clip.clone()),
            (blocked.clone(), clip.clone()),
        ];

        assert!(write(&layout, &clips, None).is_err());
        assert!(
            !first.exists(),
            "the clip written before the failure is removed"
        );
        assert_eq!(std::fs::read(&earlier).unwrap(), b"earlier clip");

        // A failure once its clip is on the disk removes that clip too.
        let other = Uuid::new_v4();
        let second = layout.run_sample_clip(other, run);
        let other_earlier = layout.sample_clip(other);
        std::fs::write(&other_earlier, b"earlier clip").unwrap();
        let clips = vec![(first.clone(), clip.clone()), (second.clone(), clip)];
        let failing: ClipProbe = Arc::new(|step| match step {
            ClipStep::Written(1) => Err(std::io::Error::other("the disk is full")),
            _ => Ok(()),
        });
        assert!(write(&layout, &clips, Some(&failing)).is_err());
        assert!(!first.exists() && !second.exists());
        for earlier in [&earlier, &other_earlier] {
            assert_eq!(std::fs::read(earlier).unwrap(), b"earlier clip");
        }
    }
}

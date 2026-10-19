//! The speakers' sample clips on the disk. A run writes each clip under a
//! name of its own (`speakers/<SPEAKER-UUID>-<RUN-UUID>.wav`), the merge's
//! commit points the speaker rows at the new files, and only then does the
//! sweep remove the meeting's speakers' clips that no row names, sparing a
//! confirmed speaker without a new clip, so every row names a whole clip or none.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use steno_core::{
    AudioBuffer16k, RecordingLayout, Speaker, Store, StoreError, paths::file_url_path,
};
use uuid::Uuid;

use crate::files::{self, Access};
use crate::fixtures::{int16, wav_data};
use crate::pipeline::BACKGROUND_RUN_LOG;

/// A point in a run's clip handling where a test fails the step or ends
/// the run as a crash would.
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

/// Called at each [`ClipStep`]. An error is the step's failure; a panic
/// ends the run there, as a crash would.
pub type ClipProbe = Arc<dyn Fn(ClipStep) -> std::io::Result<()> + Send + Sync>;

pub(crate) fn reach(probe: Option<&ClipProbe>, step: ClipStep) -> std::io::Result<()> {
    probe.map_or(Ok(()), |probe| probe(step))
}

/// Writes each of `clips` durably to its run's own path under `layout`'s
/// `speakers/`, so a commit that names them names files on the disk. A
/// failure removes the files this call wrote and leaves every other file
/// alone.
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

/// The sweep of a run once its merge committed `committed` (the rows as
/// `diarize` built them) in place of `replaced`: [`sweep`] over the
/// speakers the merge replaced and those this run gave a clip, keeping every
/// clip a row names and a confirmed clip-less speaker's files. Removes
/// nothing when reading the rows fails. The caller holds the meeting in the
/// in-flight set.
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
    let mut owners = given_a_clip.clone();
    for speaker in replaced {
        if speaker.assignment.is_confirmed() && !given_a_clip.contains(&speaker.id) {
            named.extend(speaker.sample_clip_url.clone());
        } else {
            owners.insert(speaker.id);
        }
    }
    Ok(sweep(directory, &owners, &named, probe))
}

/// Removes every clip file in `directory` of one of `owners` that none of
/// the `named` URLs names (by file name in any ASCII case), WAV files and
/// unfinished temporaries alike, and returns the files removed. A file that
/// cannot be removed is left for the next sweep.
pub(crate) fn sweep(
    directory: &Path,
    owners: &BTreeSet<Uuid>,
    named: &[String],
    probe: Option<&ClipProbe>,
) -> Vec<PathBuf> {
    // A file system that ignores case plays a URL in any case, so such a URL
    // keeps its file.
    let named: BTreeSet<OsString> = named
        .iter()
        .filter_map(|url| Some(file_url_path(url)?.file_name()?.to_ascii_uppercase()))
        .collect();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !clip_speaker(&name).is_some_and(|id| owners.contains(&id))
            || named.contains(&name.to_ascii_uppercase())
            || !entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            continue;
        }
        let path = entry.path();
        match std::fs::remove_file(&path) {
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

/// The speaker a clip file belongs to, by the id its name starts with, for
/// a clip or the temporary of an unfinished clip write (`.partial`,
/// `.wav.partial`, `.wav.part`). `None` for any other file.
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
    use steno_core::SpeakerAssignment;
    use steno_core::paths::file_url;
    use steno_core::testing::sample_data;

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

    fn put(paths: &[&PathBuf]) {
        for path in paths {
            std::fs::write(path, b"clip").unwrap();
        }
    }

    /// The sweep removes only its speakers' unnamed clips and temporaries;
    /// named files, other speakers' files and other files stay.
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

    /// After the merge only the unnamed files of the sweep's owners go;
    /// confirmed speakers without a new clip keep theirs, and every clip a
    /// row names stays.
    #[test]
    fn after_the_merge_only_the_owners_unnamed_clips_go() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("steno.sqlite")).unwrap();
        let meeting = sample_data::meeting();
        store.save_meeting(&meeting).unwrap();
        let layout = RecordingLayout::new(&dir.path().join("audio"), meeting.id);
        layout.create_directories(true).unwrap();
        let (earlier, run) = (Uuid::new_v4(), Uuid::new_v4());
        let [
            clipless,
            dropped,
            renewed,
            unconfirmed,
            merged_into,
            returned,
            gone,
            fresh,
        ] = [(); 8].map(|()| Uuid::new_v4());
        let before = |id| layout.run_sample_clip(id, earlier);
        let now = |id| layout.run_sample_clip(id, run);
        let dropped_leftover = layout.run_sample_clip(dropped, Uuid::new_v4());
        // The clip a merge moved from the unconfirmed speaker to a
        // confirmed one without a clip.
        let moved = layout.run_sample_clip(unconfirmed, Uuid::new_v4());
        let fresh_leftover = layout.run_sample_clip(fresh, Uuid::new_v4());
        put(&[
            &before(clipless),
            &before(dropped),
            &dropped_leftover,
            &before(renewed),
            &before(unconfirmed),
            &now(renewed),
            &now(unconfirmed),
            &moved,
            &before(returned),
            &before(gone),
            &fresh_leftover,
            &now(fresh),
        ]);
        let replaced = [
            speaker(meeting.id, clipless, true, Some(&before(clipless))),
            speaker(meeting.id, dropped, true, Some(&before(dropped))),
            speaker(meeting.id, renewed, true, Some(&before(renewed))),
            speaker(meeting.id, unconfirmed, false, Some(&before(unconfirmed))),
            speaker(meeting.id, merged_into, true, Some(&moved)),
            // An unconfirmed speaker the re-run drops.
            speaker(meeting.id, gone, false, Some(&before(gone))),
        ];
        // The rows the merge committed, as diarize wrote them.
        let committed = [
            speaker(meeting.id, clipless, false, None),
            speaker(meeting.id, renewed, false, Some(&now(renewed))),
            speaker(meeting.id, unconfirmed, false, Some(&now(unconfirmed))),
            speaker(meeting.id, merged_into, false, None),
            // A confirmed speaker an earlier re-run dropped, back unconfirmed.
            speaker(meeting.id, returned, false, None),
            // A new speaker with a clip, and a crashed run's leftover.
            speaker(meeting.id, fresh, false, Some(&now(fresh))),
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
        let mut expected = vec![
            before(renewed),
            before(unconfirmed),
            before(gone),
            fresh_leftover,
        ];
        expected.sort();
        assert_eq!(removed, expected);
        for kept in [
            before(clipless),
            before(dropped),
            dropped_leftover,
            now(renewed),
            now(unconfirmed),
            moved,
            before(returned),
        ] {
            assert!(kept.exists(), "{}", kept.display());
        }
    }

    /// A write that fails partway removes the files it wrote and leaves the
    /// named clips alone.
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
    }
}

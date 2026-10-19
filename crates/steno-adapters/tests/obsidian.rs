//! The Obsidian destination against a real temp vault: the layout users
//! see, re-export, folder claims, person pages, the audio copy, and the
//! user's edits, which are never overwritten.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use steno_adapters::obsidian::{ManagedBlock, ObsidianError, ObsidianFolderDestination};
use steno_adapters::rendering::ArtifactRenderer;
use steno_adapters::runtime::DeliveryLedger;
use steno_core::content_hash::sha256;
use steno_core::paths::file_url;
use steno_core::{
    AudioFormat, AudioLane, DeliveryReceipt, FileOwnership, MeetingExport, ObsidianSettings,
    Platform,
};

struct Vault {
    directory: tempfile::TempDir,
    root: PathBuf,
}

impl Vault {
    fn new() -> Self {
        let directory = temp_dir();
        let root = directory.path().join("vault");
        fs::create_dir_all(&root).unwrap();
        Vault { directory, root }
    }

    /// Audio on, people in `People`, copies dated by [`copy_clock`].
    fn destination(&self) -> ObsidianFolderDestination {
        self.destination_with(true, Some("People"))
    }

    fn destination_with(&self, audio: bool, people: Option<&str>) -> ObsidianFolderDestination {
        destination_at(&self.root, audio, people)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }
    fn read(&self, relative: &str) -> Vec<u8> {
        fs::read(self.path(relative)).unwrap_or_else(|error| panic!("{relative}: {error}"))
    }
    fn text(&self, relative: &str) -> String {
        String::from_utf8(self.read(relative)).unwrap()
    }
    fn list(&self, relative: &str) -> Vec<String> {
        list(&self.path(relative))
    }
    fn write(&self, relative: &str, data: impl AsRef<[u8]>) {
        fs::write(self.path(relative), data).unwrap();
    }
    fn export_with_audio(&self) -> MeetingExport {
        export_with_audio_in(self.directory.path())
    }
    fn sweep_the_mixdown(&self) {
        fs::remove_file(self.directory.path().join("audio.m4a")).unwrap();
    }
    /// Each file's bytes, by name, in `folder`.
    fn contents(&self, folder: &str) -> Vec<(String, Vec<u8>)> {
        self.list(folder)
            .into_iter()
            .map(|name| {
                let bytes = self.read(&format!("{folder}/{name}"));
                (name, bytes)
            })
            .collect()
    }
}

/// 23:30 UTC on the 7th, the 8th in Berlin: copies are dated 2026-10-08.
fn copy_clock() -> chrono::DateTime<chrono::Utc> {
    steno_core::json::parse_date("2026-10-07T23:30:00.000Z").unwrap()
}

/// A destination in Berlin time with the `task` tag, on the Mac.
fn destination_at(vault: &Path, audio: bool, people: Option<&str>) -> ObsidianFolderDestination {
    ObsidianFolderDestination::new(
        ObsidianSettings {
            vault_path: vault.to_string_lossy().into_owned(),
            people_folder: people.map(str::to_owned),
            include_audio: audio,
            task_tag: Some("task".to_owned()),
            extra: serde_json::Map::new(),
        },
        BERLIN,
    )
    .with_platform(Platform::Macos)
    .with_now(copy_clock)
}

const ANNA_PAGE: &str = "People/Anna Müller.md";

fn meeting_files(slug: &str) -> Vec<String> {
    let mut files = vec![
        format!("{slug} - Tasks.md"),
        format!("{slug} - Transcript.md"),
        format!("{slug}.md"),
        "audio.m4a".to_owned(),
        "meeting.json".to_owned(),
        "transcript.vtt".to_owned(),
    ];
    files.sort();
    files
}

fn without_audio(files: Vec<String>) -> Vec<String> {
    files.into_iter().filter(|f| f != "audio.m4a").collect()
}

/// The files a delivery without audio writes into `<FOLDER>-2`.
fn second_folder_files() -> Vec<String> {
    without_audio(meeting_files(&format!("{FOLDER_SLUG}-2")))
}

fn paths(receipt: &DeliveryReceipt) -> Vec<String> {
    receipt
        .files
        .iter()
        .map(|f| f.relative_path.clone())
        .collect()
}

fn hash_in(receipt: &DeliveryReceipt, path: &str) -> Option<Vec<u8>> {
    receipt
        .files
        .iter()
        .find(|file| file.relative_path == path)
        .map(|file| file.sha256.clone())
}

fn managed(receipt: &DeliveryReceipt) -> Vec<String> {
    receipt
        .files
        .iter()
        .filter(|f| f.ownership == FileOwnership::ManagedBlock)
        .map(|f| f.relative_path.clone())
        .collect()
}

fn deliver(
    destination: &ObsidianFolderDestination,
    export: &MeetingExport,
    previous: Option<&DeliveryReceipt>,
) -> DeliveryReceipt {
    destination.deliver_meeting(export, previous).unwrap()
}

fn folder_note() -> String {
    format!("{FOLDER}/{FOLDER_SLUG}.md")
}

/// The copy beside the folder note: unnumbered, then `… 2`, `… 3`.
fn copy(number: u32) -> String {
    DeliveryLedger::copy_beside_path(&folder_note(), "2026-10-08", number)
}

/// The user's addition at the end of the folder note; returns the note.
fn edit_the_folder_note(vault: &Vault) -> String {
    let edited = format!("{}\nMy addition.\n", vault.text(&folder_note()));
    vault.write(&folder_note(), &edited);
    edited
}

/// The export with its first decision reworded: the folder note and
/// `meeting.json` change, the folder name does not.
fn with_a_changed_decision(export: &MeetingExport) -> MeetingExport {
    let mut changed = export.clone();
    changed.decisions[0].text = String::from("Die Aufteilung wird verschoben.");
    changed
}

fn retitled(export: &MeetingExport) -> MeetingExport {
    let mut renamed = export.clone();
    renamed.meeting.title = String::from("Neuer Titel");
    renamed
}

fn kept_one(name: &str, copy: &str) -> String {
    format!(
        "Kept your changes to {name} and put Steno's version beside it as {copy}; \
         to use Steno's, delete {name} and export again"
    )
}

fn kept_several(names: &[&str]) -> String {
    let (last, rest) = names.split_last().unwrap();
    format!(
        "Kept your changes to {} and {last} and put Steno's versions beside them; \
         to use Steno's, delete your versions and export again",
        rest.join(", ")
    )
}

/// A Latin-1 person page, which cannot be merged.
fn latin1_page() -> Vec<u8> {
    let mut page = b"# Anna M".to_vec();
    page.push(0xFC);
    page.extend(b"ller\n");
    page
}

#[test]
fn first_delivery_writes_the_six_file_layout_and_two_person_pages() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let destination = vault.destination();
    destination.validate_vault().unwrap();

    let receipt = deliver(&destination, &export, None);

    assert_eq!(vault.list("Meetings"), [FOLDER_SLUG]);
    assert_eq!(vault.list(FOLDER), meeting_files(FOLDER_SLUG));
    assert_eq!(
        vault.list("People"),
        ["Anna Müller.md", "Nicolai Schmid.md"]
    );
    for (file, golden) in [
        (
            format!("{FOLDER_SLUG}.md"),
            "folder-note-wikilink-berlin.md",
        ),
        (
            format!("{FOLDER_SLUG} - Transcript.md"),
            "transcript-wikilink.md",
        ),
        (format!("{FOLDER_SLUG} - Tasks.md"), "tasks-wikilink-tag.md"),
        ("transcript.vtt".to_owned(), "transcript.vtt"),
    ] {
        assert_matches_golden(
            &vault.text(&format!("{FOLDER}/{file}")),
            &format!("snapshots/obsidian/{golden}"),
        );
    }
    assert_eq!(
        vault.read(&format!("{FOLDER}/meeting.json")),
        ArtifactRenderer::new().render_json(&export).unwrap(),
        "it carries this run's mixdown path"
    );
    assert_eq!(vault.read(&format!("{FOLDER}/audio.m4a")), mixdown_bytes());
    assert_matches_golden(
        &vault.text(ANNA_PAGE),
        "snapshots/obsidian/person-page-new.md",
    );

    assert_eq!(receipt.root, vault.root.to_string_lossy());
    assert_eq!(receipt.folder, FOLDER);
    assert_eq!(receipt.renderer_version, ArtifactRenderer::VERSION);
    let mut expected: Vec<String> = meeting_files(FOLDER_SLUG)
        .iter()
        .map(|f| format!("{FOLDER}/{f}"))
        .collect();
    expected.extend([ANNA_PAGE.to_owned(), "People/Nicolai Schmid.md".to_owned()]);
    assert_eq!(paths(&receipt), expected);
    assert_eq!(managed(&receipt), expected[6..]);
    for file in &receipt.files {
        assert_eq!(
            file.sha256,
            sha256(&vault.read(&file.relative_path)),
            "{}",
            file.relative_path
        );
    }
}

#[test]
fn an_unchanged_reexport_is_byte_identical_and_warns_of_nothing() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let destination = vault.destination();
    let first = deliver(&destination, &export, None);
    let before = vault.contents(FOLDER);
    let second = deliver(&destination, &export, Some(&first));
    assert_eq!(second, first);
    assert_eq!(vault.contents(FOLDER), before);
    assert!(!serde_json::to_string(&second).unwrap().contains("warnings"));
}

#[test]
fn validate_rejects_a_missing_vault_and_a_bad_people_folder() {
    let vault = Vault::new();
    let file = vault.directory.path().join("file");
    fs::write(&file, b"").unwrap();
    for missing in [vault.directory.path().join("nope"), file] {
        assert_eq!(
            destination_at(&missing, false, None).validate_vault(),
            Err(ObsidianError::VaultMissing(
                missing.to_string_lossy().into_owned()
            ))
        );
    }
    for bad in [
        "/People",
        "../People",
        "People/../..",
        "./People",
        "People/.",
        ".",
        "",
        "a//b",
        "a\\b",
        " People",
        "People\n",
    ] {
        assert_eq!(
            vault.destination_with(true, Some(bad)).validate_vault(),
            Err(ObsidianError::PeopleFolderInvalid(bad.to_owned())),
            "{bad:?}"
        );
    }
    vault
        .destination_with(true, Some("Notes/People"))
        .validate_vault()
        .unwrap();
    assert!(vault.list("").is_empty(), "the probe leaves nothing behind");
}

/// Every entry under `root`, recursively, links not followed, special
/// files never opened.
fn listing(root: &Path) -> BTreeMap<PathBuf, String> {
    fn walk(root: &Path, folder: &Path, into: &mut BTreeMap<PathBuf, String>) {
        for entry in fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if metadata.file_type().is_symlink() {
                into.insert(
                    relative,
                    format!("link to {}", fs::read_link(&path).unwrap().display()),
                );
            } else if metadata.is_dir() {
                into.insert(relative, "folder".to_owned());
                walk(root, &path, into);
            } else if metadata.is_file() {
                into.insert(
                    relative,
                    String::from_utf8_lossy(&fs::read(&path).unwrap()).into(),
                );
            } else {
                into.insert(relative, "special file".to_owned());
            }
        }
    }
    let mut entries = BTreeMap::new();
    walk(root, root, &mut entries);
    entries
}

/// A vault check removes only the empty probe files a crash left at the
/// root; the user's entries, also ones named like a probe, stay as they
/// were, and so does a vault the check refuses.
#[test]
fn a_vault_check_leaves_a_lived_in_vault_as_it_was() {
    let vault = Vault::new();
    for (relative, content) in [
        (".obsidian/plugins/sync/data.json", "{\"device\":\"mac\"}"),
        ("Inbox.md", "- [ ] call back"),
        (".steno-probe-cafebabe", "mine"),
        (".steno-probe-notes.txt", ""),
        (".steno-probe-ABCDEF12", ""),
        ("Meetings/.steno-probe-deadbeef", ""),
    ] {
        fs::create_dir_all(vault.path(relative).parent().unwrap()).unwrap();
        vault.write(relative, content);
    }
    fs::create_dir(vault.path(".steno-probe-12345678")).unwrap();
    #[cfg(unix)]
    {
        let outside = vault.directory.path().join("outside.md");
        fs::write(&outside, "").unwrap();
        std::os::unix::fs::symlink(&outside, vault.path(".steno-probe-0badf00d")).unwrap();
        let fifo = std::process::Command::new("mkfifo")
            .arg(vault.path(".steno-probe-0f1f0f1f"))
            .status()
            .unwrap();
        assert!(fifo.success());
    }
    let before = listing(&vault.root);
    vault.write(".steno-probe-deadbeef", "");
    vault.write(".steno-probe-0123abcd", "");

    vault.destination().validate_vault().unwrap();
    vault.destination().validate_vault().unwrap();
    assert_eq!(listing(&vault.root), before);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let locked = Vault::new();
        vault.write(".steno-probe-deadbeef", "");
        let before = listing(&vault.root);
        for vault in [&vault, &locked] {
            fs::set_permissions(&vault.root, fs::Permissions::from_mode(0o555)).unwrap();
        }
        let refused = vault.destination().validate_vault();
        let delivered = locked.destination().deliver_meeting(&export(), None);
        for vault in [&vault, &locked] {
            fs::set_permissions(&vault.root, fs::Permissions::from_mode(0o755)).unwrap();
        }
        if refused.is_ok() {
            return; // root: the permission bits do not bite
        }
        let root = vault.root.to_string_lossy().into_owned();
        assert_eq!(refused, Err(ObsidianError::VaultNotWritable(root)));
        assert_eq!(
            listing(&vault.root),
            before,
            "a refused check writes and removes nothing"
        );
        let locked_root = locked.root.to_string_lossy().into_owned();
        assert!(
            matches!(&delivered, Err(ObsidianError::WriteFailed { path, .. }) if path.starts_with(&locked_root)),
            "{delivered:?}"
        );
    }
}

#[test]
fn reexport_keeps_the_folder_user_files_and_edits_and_updates_the_title() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let first = deliver(&vault.destination(), &export, None);
    vault.write(&format!("{FOLDER}/notes.md"), "my notes\n");
    let edited = format!(
        "Above the block.\n\n{}\nBelow the block.\n",
        vault.text(ANNA_PAGE)
    );
    vault.write(ANNA_PAGE, &edited);
    let renamed = retitled(&export);

    let second = deliver(
        &vault.destination_with(false, Some("People")),
        &renamed,
        Some(&first),
    );

    assert_eq!(
        second.folder, FOLDER,
        "the folder is pinned at first delivery"
    );
    assert_eq!(vault.list("Meetings"), [FOLDER_SLUG]);
    assert_eq!(vault.text(&format!("{FOLDER}/notes.md")), "my notes\n");
    assert_eq!(
        vault.read(&format!("{FOLDER}/audio.m4a")),
        mixdown_bytes(),
        "an opted-out audio copy stays"
    );
    assert_eq!(paths(&second), paths(&first), "and stays in the receipt");
    let note = vault.text(&folder_note());
    assert!(note.starts_with("---\ntitle: \"Neuer Titel\"\n"));
    assert!(
        note.contains(&format!("[[{FOLDER_SLUG} - Transcript|Transcript]]")),
        "the pinned slug"
    );
    assert_eq!(
        vault.text(ANNA_PAGE),
        edited.replace(
            "Produktstrategie: \"90/10\" & Roadmap für Q4]]",
            "Neuer Titel]]"
        ),
        "only this meeting's line changed"
    );
}

/// Taken folders get a suffix and stay untouched; a folder holding this
/// meeting's `meeting.json` is a crashed attempt, reused and swept of its
/// own temp files only.
#[test]
fn collisions_get_a_suffix_and_a_crashed_attempt_is_reused() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let mut other = export.clone();
    other.meeting.id = uuid(99);
    let two = format!("{FOLDER}-2");
    let three = format!("{FOLDER}-3");
    fs::create_dir_all(vault.path(&two)).unwrap();
    fs::create_dir_all(vault.path(FOLDER)).unwrap();
    vault.write(
        &format!("{FOLDER}/meeting.json"),
        ArtifactRenderer::new().render_json(&other).unwrap(),
    );
    vault.write(&format!("{two}/notes.md"), "theirs\n");

    let receipt = deliver(&vault.destination(), &export, None);

    assert_eq!(receipt.folder, three);
    assert_eq!(vault.list(FOLDER), ["meeting.json"]);
    assert_eq!(vault.list(&two), ["notes.md"]);
    assert_eq!(
        vault.list(&three),
        meeting_files(&format!("{FOLDER_SLUG}-3"))
    );
    assert!(
        vault
            .text(&format!("{three}/{FOLDER_SLUG}-3.md"))
            .contains(&format!("[[{FOLDER_SLUG}-3 - Transcript|Transcript]]"))
    );
    assert!(
        vault
            .text(ANNA_PAGE)
            .contains(&format!("[[{FOLDER_SLUG}-3|Produktstrategie"))
    );

    let theirs = format!("{two}/.steno-tmp-00000000-x.md");
    vault.write(&theirs, "theirs\n");
    vault.write(
        &format!("{three}/.steno-tmp-deadbeef-{FOLDER_SLUG}-3.md"),
        "half written\n",
    );
    let again = deliver(&vault.destination(), &export, None);
    assert_eq!(again.folder, three);
    assert_eq!(vault.list("Meetings").len(), 3);
    assert_eq!(
        vault.list(&three),
        meeting_files(&format!("{FOLDER_SLUG}-3")),
        "swept"
    );
    assert!(
        vault.path(&theirs).exists(),
        "another folder's temp is not ours"
    );
}

/// An audio file the user put where the copy goes is never opened, also
/// when the mixdown is gone.
#[test]
fn a_file_the_app_never_wrote_is_not_opened_on_reexport() {
    let vault = Vault::new();
    let mut export = vault.export_with_audio();
    let first = deliver(
        &vault.destination_with(false, Some("People")),
        &export,
        None,
    );
    let audio = format!("{FOLDER}/audio.m4a");
    vault.write(&audio, "user audio\n");
    for mixdown in [true, false] {
        if !mixdown {
            export.audio.as_mut().unwrap().mixdown_url = None;
        }
        let second = deliver(&vault.destination(), &export, Some(&first));
        assert_eq!(vault.text(&audio), "user audio\n");
        assert_eq!(paths(&second), paths(&first));
        assert_eq!(second.warnings, Vec::<String>::new());
    }
}

/// A handover meeting as `persist` leaves it: one AAC file, no mixdown.
fn phone_export(vault: &Vault) -> MeetingExport {
    let mut export = export();
    let recording = vault.directory.path().join("recording.m4a");
    fs::write(&recording, mixdown_bytes()).unwrap();
    let audio = export.audio.as_mut().unwrap();
    audio.url = file_url(&recording, false);
    audio.format = AudioFormat::M4aAac;
    audio.lanes = vec![AudioLane::Mic];
    audio.sidecars_16k = BTreeMap::new();
    audio.mixdown_url = None;
    export
}

#[test]
fn a_phone_meeting_without_a_mixdown_copies_its_recording() {
    let vault = Vault::new();
    let receipt = deliver(&vault.destination(), &phone_export(&vault), None);
    assert_eq!(receipt.warnings, Vec::<String>::new());
    assert_eq!(vault.read(&format!("{FOLDER}/audio.m4a")), mixdown_bytes());
    assert!(paths(&receipt).contains(&format!("{FOLDER}/audio.m4a")));
}

/// A missing audio source never blocks the notes: with the copy on, they go
/// out with a warning; with it off, without one.
#[test]
fn a_missing_audio_source_delivers_the_notes_and_warns() {
    let vault = Vault::new();
    let mut no_mixdown = export();
    no_mixdown.audio.as_mut().unwrap().mixdown_url = None;
    let mut gone_mixdown = export();
    gone_mixdown.audio.as_mut().unwrap().mixdown_url =
        Some(file_url(&vault.directory.path().join("gone.m4a"), false));
    let swept_phone = phone_export(&vault);
    fs::remove_file(vault.directory.path().join("recording.m4a")).unwrap();
    for export in [no_mixdown, gone_mixdown, swept_phone] {
        let vault = Vault::new();
        let receipt = deliver(&vault.destination(), &export, None);
        assert_eq!(
            receipt.warnings,
            [ObsidianFolderDestination::NO_AUDIO_WARNING]
        );
        assert_eq!(
            vault.list(FOLDER),
            without_audio(meeting_files(FOLDER_SLUG))
        );
        assert_eq!(vault.list("People").len(), 2);
        let off = Vault::new();
        let receipt = deliver(&off.destination_with(false, None), &export, None);
        assert_eq!(receipt.warnings, Vec::<String>::new(), "the copy is off");
    }
}

/// After the sweep the copy in the vault is the audio; once the user
/// deletes it too, the receipt drops it and the delivery warns.
#[test]
fn a_swept_mixdown_keeps_the_audio_copy_in_the_vault() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let first = deliver(&vault.destination(), &export, None);
    let audio = format!("{FOLDER}/audio.m4a");
    vault.sweep_the_mixdown();
    fs::remove_file(vault.path(&format!("{FOLDER}/meeting.json"))).unwrap();

    let second = deliver(&vault.destination(), &retitled(&export), Some(&first));
    assert_eq!(second.folder, FOLDER);
    assert!(vault.text(&folder_note()).contains("# Neuer Titel\n"));
    assert_eq!(vault.read(&audio), mixdown_bytes());
    assert_eq!(hash_in(&second, &audio), hash_in(&first, &audio));
    assert_eq!(second.warnings, Vec::<String>::new());

    fs::remove_file(vault.path(&audio)).unwrap();
    let third = deliver(&vault.destination(), &export, Some(&second));
    assert_eq!(
        third.warnings,
        [ObsidianFolderDestination::NO_AUDIO_WARNING]
    );
    assert_eq!(
        paths(&third),
        paths(&first)
            .into_iter()
            .filter(|path| *path != audio)
            .collect::<Vec<_>>()
    );
}

/// A page the user wrote keeps every byte and gets the block appended,
/// never Steno's frontmatter; other pages stay out of it.
#[test]
fn a_person_page_the_user_wrote_gets_the_block_appended() {
    let vault = Vault::new();
    fs::create_dir_all(vault.path("People")).unwrap();
    let user_page = "---\nrole: \"CEO\"\n---\n# Anna\n\nMet her at the fair.";
    vault.write(ANNA_PAGE, user_page);
    vault.write("People/Bob.md", "Bob's page.\n");

    let receipt = deliver(&vault.destination(), &export(), None);

    let line =
        ArtifactRenderer::new().render_person_pages(&export(), &wikilink(), Some(FOLDER_SLUG))[0]
            .line
            .clone();
    assert_eq!(
        vault.text(ANNA_PAGE),
        format!("{user_page}\n\n{}", ManagedBlock::block(&[line]))
    );
    assert_eq!(vault.text("People/Bob.md"), "Bob's page.\n");
    assert_eq!(managed(&receipt), [ANNA_PAGE, "People/Nicolai Schmid.md"]);
}

/// With the people folder off the notes stop linking people and the pages
/// stay on disk and in the receipt.
#[test]
fn people_folder_off_keeps_the_pages_and_writes_no_links() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let first = deliver(&vault.destination(), &export, None);
    let anna = vault.read(ANNA_PAGE);

    let second = deliver(&vault.destination_with(true, None), &export, Some(&first));

    assert_eq!(vault.read(ANNA_PAGE), anna, "not touched, not deleted");
    assert_eq!(paths(&second), paths(&first));
    let note = vault.text(&folder_note());
    assert!(note.contains("  - \"Anna Müller\"\n"));
    assert!(!note.contains("[[Anna"));

    let fresh = Vault::new();
    deliver(&fresh.destination_with(true, None), &export, None);
    assert!(!fresh.path("People").exists());
}

#[test]
fn a_second_meeting_on_the_same_day_gets_the_next_suffix_and_shares_the_person_pages() {
    let vault = Vault::new();
    let destination = vault.destination_with(false, Some("People"));
    let first = export();
    let mut second = first.clone();
    second.meeting.id = uuid(2);
    second.meeting.started_at = first.meeting.started_at + chrono::Duration::hours(3);

    deliver(&destination, &first, None);
    let first_files = vault.contents(FOLDER);
    let receipt = deliver(&destination, &second, None);

    assert_eq!(receipt.folder, format!("{FOLDER}-2"));
    assert_eq!(vault.list(&format!("{FOLDER}-2")), second_folder_files());
    assert_eq!(
        vault.contents(FOLDER),
        first_files,
        "the first meeting is untouched"
    );
    let anna = vault.text(ANNA_PAGE);
    let lines: Vec<&str> = anna
        .lines()
        .filter(|l| l.starts_with("- 2026-09-24 "))
        .collect();
    assert_eq!(lines.len(), 2, "one line per meeting, in one block");
    assert!(
        lines[0].contains(&format!("[[{FOLDER_SLUG}-2|")),
        "newest first: {anna}"
    );
    assert!(lines[0].ends_with(&ManagedBlock::marker(uuid(2))));
    assert_eq!(anna.matches(ManagedBlock::START).count(), 1);
    assert_eq!(deliver(&destination, &second, Some(&receipt)), receipt);
}

/// A renamed person gets a new page; the old one is never deleted, loses
/// only this meeting's line (the user's text around the block stays), and
/// is not opened again once that is gone.
#[test]
fn a_renamed_person_gets_a_new_page_and_only_the_line_leaves_the_old_one() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let first = deliver(&vault.destination(), &export, None);
    let marker = ManagedBlock::marker(export.meeting.id);
    let old_page = format!(
        "Above, with {marker} mentioned.\n\n{}\nBelow.\n",
        vault.text(ANNA_PAGE)
    );
    vault.write(ANNA_PAGE, &old_page);
    let mut renamed = export.clone();
    renamed.persons[0].display_name = "Anna Schulz".to_owned();
    renamed.participants[0].display_name = "Anna Schulz".to_owned();

    let second = deliver(&vault.destination(), &renamed, Some(&first));

    assert_eq!(
        vault.list("People"),
        ["Anna Müller.md", "Anna Schulz.md", "Nicolai Schmid.md"]
    );
    let cleaned = ManagedBlock::remove(export.meeting.id, &old_page);
    assert_eq!(vault.text(ANNA_PAGE), cleaned, "every other byte stays");
    assert_eq!(
        cleaned.matches(&marker).count(),
        1,
        "the user's mention stays"
    );
    let new_page = vault.text("People/Anna Schulz.md");
    assert!(new_page.contains(&ManagedBlock::marker(export.meeting.id)));
    assert!(new_page.contains("steno_person_id: \"00000000-0000-0000-0000-00000000000a\""));
    assert_eq!(
        managed(&second).len(),
        3,
        "the old page stays in the receipt"
    );
    let transcript = vault.text(&format!("{FOLDER}/{FOLDER_SLUG} - Transcript.md"));
    assert!(
        !transcript.contains("Anna Müller"),
        "every note uses the current name"
    );

    // A rename-based writer replaces a read-only file too, so the file's
    // identity shows the stale page was never opened.
    let before = fs::metadata(vault.path(ANNA_PAGE)).unwrap();
    assert_eq!(
        deliver(&vault.destination(), &renamed, Some(&second)),
        second
    );
    let after = fs::metadata(vault.path(ANNA_PAGE)).unwrap();
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(after.ino(), before.ino());
    }
}

/// A page that cannot be merged fails the delivery; no byte is replaced.
#[test]
fn a_person_page_that_is_not_utf8_is_left_alone_and_reported() {
    let vault = Vault::new();
    fs::create_dir_all(vault.path("People")).unwrap();
    vault.write(ANNA_PAGE, latin1_page());
    match vault.destination().deliver_meeting(&export(), None) {
        Err(ObsidianError::ReadFailed { path, underlying }) => {
            assert_eq!(Path::new(&path), vault.path(ANNA_PAGE));
            assert!(underlying.contains("not UTF-8"));
        }
        other => panic!("expected ReadFailed, got {other:?}"),
    }
    assert_eq!(vault.read(ANNA_PAGE), latin1_page());
}

fn with_anna_named(name: &str) -> MeetingExport {
    let mut export = export();
    name.clone_into(&mut export.persons[0].display_name);
    name.clone_into(&mut export.participants[0].display_name);
    export
}

/// On a case-insensitive vault (macOS, Windows) a case-only rename keeps
/// the meeting on the one page; on a case-sensitive one the line moves to
/// the new page.
#[test]
fn a_case_only_rename_keeps_the_meeting_on_the_persons_page() {
    let vault = Vault::new();
    vault.write("case-probe", "");
    let insensitive = vault.path("CASE-PROBE").exists();
    fs::remove_file(vault.path("case-probe")).unwrap();
    let destination = vault.destination_with(false, Some("People"));
    let first = deliver(&destination, &with_anna_named("anna müller"), None);
    deliver(&destination, &with_anna_named("Anna Müller"), Some(&first));

    let marker = ManagedBlock::marker(meeting_id());
    assert!(vault.text(ANNA_PAGE).contains(&marker));
    if insensitive {
        assert_eq!(vault.list("People").len(), 2, "one page per person");
    } else {
        assert!(!vault.text("People/anna müller.md").contains(&marker));
    }
}

#[test]
fn the_destination_names_calls_after_the_platform_the_app_runs_on() {
    let settings = Vault::new().destination().settings().clone();
    assert_eq!(
        ObsidianFolderDestination::new(settings, BERLIN).platform(),
        Platform::CURRENT
    );
}

/// A receipt from another vault pins nothing and carries nothing over: the
/// new vault is written fresh under the collision rule, the old one is left
/// alone.
#[test]
fn a_receipt_from_another_vault_is_a_first_delivery() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let first = deliver(&vault.destination(), &export, None);
    let before = vault.contents(FOLDER);

    let moved = vault.directory.path().join("moved");
    fs::create_dir_all(&moved).unwrap();
    let second = deliver(
        &destination_at(&moved, true, Some("People")),
        &export,
        Some(&first),
    );
    assert_eq!(second.root, moved.to_string_lossy());
    assert_eq!(paths(&second), paths(&first));

    let other = vault.directory.path().join("other");
    let mut theirs = export.clone();
    theirs.meeting.id = uuid(99);
    fs::create_dir_all(other.join(FOLDER)).unwrap();
    fs::write(
        other.join(FOLDER).join("meeting.json"),
        ArtifactRenderer::new().render_json(&theirs).unwrap(),
    )
    .unwrap();
    let third = deliver(
        &destination_at(&other, true, Some("People")),
        &export,
        Some(&first),
    );
    assert_eq!(third.folder, format!("{FOLDER}-2"));
    assert_eq!(list(&other.join(FOLDER)), ["meeting.json"]);
    assert_eq!(vault.contents(FOLDER), before);
}

/// The vault path spelled differently (trailing slash, `.` and `..`, a
/// symlink, other case on a case-insensitive disk) is the same vault, so
/// the receipt applies and no second folder appears.
#[test]
fn other_spellings_of_the_vault_path_are_the_same_vault() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let first = deliver(&vault.destination(), &export, None);
    let root = vault.root.to_string_lossy().into_owned();
    let mut spellings = vec![
        PathBuf::from(format!("{root}/")),
        vault.root.join("Meetings/.."),
    ];
    #[cfg(unix)]
    {
        let link = vault.directory.path().join("vault-link");
        std::os::unix::fs::symlink(&vault.root, &link).unwrap();
        spellings.push(link);
    }
    let shouted = vault.directory.path().join("VAULT");
    if shouted.exists() {
        spellings.push(shouted);
    }
    for spelling in spellings {
        let renamed = retitled(&with_a_changed_decision(&export));
        let destination = destination_at(&spelling, true, Some("People"));
        let again = deliver(&destination, &renamed, Some(&first));
        assert_eq!(again.folder, FOLDER, "{}", spelling.display());
        assert_eq!(
            again.root,
            spelling.to_string_lossy(),
            "recorded as configured"
        );
        assert_eq!(paths(&again), paths(&first), "{}", spelling.display());
        assert_eq!(vault.list("Meetings"), [FOLDER_SLUG]);
        assert!(vault.text(&folder_note()).contains("# Neuer Titel\n"));
    }
}

/// `ours` delivered, its folder removed, and another meeting of the same
/// date and title delivered into the name since. Returns `ours`' receipt.
fn lose_the_folder_to_another_meeting(vault: &Vault, ours: &MeetingExport) -> DeliveryReceipt {
    let mut theirs = ours.clone();
    theirs.meeting.id = uuid(2);
    let first = deliver(&vault.destination_with(false, Some("People")), ours, None);
    fs::remove_dir_all(vault.path(FOLDER)).unwrap();
    assert_eq!(
        deliver(&vault.destination_with(false, None), &theirs, None).folder,
        FOLDER
    );
    first
}

/// A redelivery never writes into a folder another meeting claimed since;
/// it gets its own, and the receipt drops only the lost folder's files.
#[test]
fn a_redelivery_whose_folder_another_meeting_claimed_since_gets_a_folder_of_its_own() {
    let vault = Vault::new();
    let ours = export();
    let first = lose_the_folder_to_another_meeting(&vault, &ours);
    let theirs = vault.contents(FOLDER);

    let again = deliver(
        &vault.destination_with(false, Some("People")),
        &ours,
        Some(&first),
    );

    assert_eq!(again.folder, format!("{FOLDER}-2"));
    assert_eq!(vault.list(&format!("{FOLDER}-2")), second_folder_files());
    assert_eq!(vault.contents(FOLDER), theirs);
    assert!(
        paths(&again)
            .iter()
            .all(|path| !path.starts_with(&format!("{FOLDER}/")))
    );
    assert_eq!(managed(&again), managed(&first));
}

/// A pinned folder whose `meeting.json` names no meeting, or whose notes
/// name another meeting, is never written.
#[test]
fn a_pinned_folder_that_is_not_this_meetings_is_never_written() {
    let ours = export();
    let damaged = Vault::new();
    let first = deliver(
        &damaged.destination_with(false, Some("People")),
        &ours,
        None,
    );
    damaged.write(&format!("{FOLDER}/meeting.json"), "{\"meeting\": 7}\n");

    let another = Vault::new();
    let lost = lose_the_folder_to_another_meeting(&another, &ours);
    fs::remove_file(another.path(&format!("{FOLDER}/meeting.json"))).unwrap();

    for (vault, previous) in [(damaged, first), (another, lost)] {
        let left = vault.contents(FOLDER);
        let again = deliver(
            &vault.destination_with(false, Some("People")),
            &ours,
            Some(&previous),
        );
        assert_eq!(again.folder, format!("{FOLDER}-2"));
        assert_eq!(vault.contents(FOLDER), left, "never written over");
    }
}

/// With `meeting.json` deleted, either note alone names the meeting (also
/// with CRLF endings and the id in capitals), so it is written back in
/// place.
#[test]
fn a_deleted_meeting_json_is_written_back_in_the_same_folder() {
    let note = format!("{FOLDER_SLUG}.md");
    for (also_deleted, left) in [
        (note.as_str(), "transcript.vtt"),
        ("transcript.vtt", note.as_str()),
    ] {
        let vault = Vault::new();
        let destination = vault.destination_with(false, Some("People"));
        let mut ours = export();
        ours.meeting.id = "0d133bbf-29e8-49aa-af88-951c84fd8e5a".parse().unwrap();
        let first = deliver(&destination, &ours, None);
        for name in ["meeting.json", also_deleted] {
            fs::remove_file(vault.path(&format!("{FOLDER}/{name}"))).unwrap();
        }
        let left = format!("{FOLDER}/{left}");
        let id = ours.meeting.id.to_string();
        vault.write(
            &left,
            vault
                .text(&left)
                .replace(&id, &id.to_uppercase())
                .replace('\n', "\r\n"),
        );

        let again = deliver(&destination, &ours, Some(&first));

        assert_eq!(again.folder, FOLDER, "{also_deleted}");
        assert_eq!(
            vault.read(&format!("{FOLDER}/meeting.json")),
            ArtifactRenderer::new().render_json(&ours).unwrap()
        );
        assert!(!vault.path(&format!("{FOLDER}-2")).exists());
    }
}

/// A file that names the pinned folder's meeting but cannot be read fails
/// the delivery, so a locked file never passes for another meeting's.
#[test]
fn an_unreadable_pin_fails_the_delivery_and_claims_nothing() {
    let note = format!("{FOLDER_SLUG}.md");
    for (unreadable, deleted) in [
        ("meeting.json", &[][..]),
        ("transcript.vtt", &["meeting.json"][..]),
        (note.as_str(), &["meeting.json", "transcript.vtt"][..]),
    ] {
        let vault = Vault::new();
        let destination = vault.destination_with(false, Some("People"));
        let first = deliver(&destination, &export(), None);
        for name in deleted.iter().chain([&unreadable]) {
            fs::remove_file(vault.path(&format!("{FOLDER}/{name}"))).unwrap();
        }
        // A directory in its place fails to read on every platform.
        let path = vault.path(&format!("{FOLDER}/{unreadable}"));
        fs::create_dir(&path).unwrap();
        match destination.deliver_meeting(&export(), Some(&first)) {
            Err(ObsidianError::ReadFailed { path: failed, .. }) => {
                assert_eq!(Path::new(&failed), path, "{unreadable}");
            }
            other => panic!("{unreadable}: expected ReadFailed, got {other:?}"),
        }
        assert!(!vault.path(&format!("{FOLDER}-2")).exists(), "{unreadable}");
    }
}

#[test]
fn a_gone_pinned_folder_is_claimed_again_in_place() {
    let vault = Vault::new();
    let ours = vault.export_with_audio();
    let first = deliver(&vault.destination(), &ours, None);
    fs::remove_dir_all(vault.path("Meetings")).unwrap();
    let again = deliver(&vault.destination(), &ours, Some(&first));
    assert_eq!(again.folder, FOLDER);
    assert_eq!(paths(&again), paths(&first));
    assert_eq!(vault.list(FOLDER), meeting_files(FOLDER_SLUG));
}

// A note the user edited in the vault is kept; Steno's render goes beside
// it as `<note> (Steno <date>).md`, and the receipt never takes the user's
// bytes for Steno's.

#[test]
fn reexport_keeps_an_edited_note_and_writes_the_new_one_beside_it() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let destination = vault.destination();
    let first = deliver(&destination, &export, None);
    let original = vault.read(&folder_note());
    let edited = edit_the_folder_note(&vault);
    fs::remove_file(vault.path(&format!("{FOLDER}/transcript.vtt"))).unwrap();

    let second = deliver(&destination, &export, Some(&first));

    assert_eq!(
        vault.text(&folder_note()),
        edited,
        "the edited note is left alone"
    );
    assert_eq!(
        vault.read(&copy(1)),
        original,
        "the render beside it, dated in Berlin"
    );
    let mut expected = meeting_files(FOLDER_SLUG);
    expected.push(format!("{FOLDER_SLUG} (Steno 2026-10-08).md"));
    expected.sort();
    assert_eq!(
        vault.list(FOLDER),
        expected,
        "the deleted transcript.vtt is back"
    );
    assert_eq!(
        second.warnings,
        [kept_one(
            &format!("{FOLDER_SLUG}.md"),
            &format!("{FOLDER_SLUG} (Steno 2026-10-08).md")
        )]
    );
    assert_eq!(
        hash_in(&second, &folder_note()),
        hash_in(&first, &folder_note()),
        "the delivered hash, so the next delivery sees the edit too"
    );
    assert_eq!(hash_in(&second, &copy(1)), Some(sha256(&original)));

    let third = deliver(
        &destination,
        &with_a_changed_decision(&export),
        Some(&second),
    );
    assert_eq!(vault.text(&folder_note()), edited);
    assert_eq!(vault.list(FOLDER), expected, "the unedited copy is reused");
    assert!(
        vault
            .text(&copy(1))
            .contains("Die Aufteilung wird verschoben.")
    );

    // A copy the user edited too is kept; `… 2` is written, then reused.
    vault.write(&copy(1), "my copy\n");
    let fourth = deliver(&destination, &export, Some(&third));
    let fifth = deliver(&destination, &export, Some(&fourth));
    assert_eq!(vault.text(&copy(1)), "my copy\n");
    assert_eq!(vault.read(&copy(2)), original);
    assert!(!vault.path(&copy(3)).exists());
    assert_eq!(vault.text(&folder_note()), edited);
    assert!(fifth.warnings[0].contains(&format!("as {FOLDER_SLUG} (Steno 2026-10-08 2).md;")));
    assert!(
        paths(&fifth).contains(&copy(1)),
        "the edited copy stays on the receipt"
    );
}

/// Without an applying receipt any other bytes count as an edit: a changed
/// render goes beside every such file, in one warning; the same render
/// writes no copy.
#[test]
fn a_receiptless_delivery_keeps_edited_notes() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let destination = vault.destination();
    deliver(&destination, &export, None);
    assert_eq!(
        deliver(&destination, &export, None).warnings,
        Vec::<String>::new()
    );
    assert_eq!(vault.list(FOLDER), meeting_files(FOLDER_SLUG));

    let edited = edit_the_folder_note(&vault);
    let changed = with_a_changed_decision(&export);
    let again = deliver(&destination, &changed, None);

    assert_eq!(
        again.folder, FOLDER,
        "the folder holding meeting.json is reused"
    );
    assert_eq!(vault.text(&folder_note()), edited);
    assert!(
        vault
            .text(&copy(1))
            .contains("Die Aufteilung wird verschoben.")
    );
    assert!(
        vault
            .path(&format!("{FOLDER}/meeting (Steno 2026-10-08).json"))
            .is_file()
    );
    assert_eq!(
        again.warnings,
        [kept_several(&[
            "meeting.json",
            &format!("{FOLDER_SLUG}.md")
        ])]
    );
    let third = deliver(&destination, &changed, Some(&again));
    assert_eq!(
        vault.text(&folder_note()),
        edited,
        "still kept with the receipt"
    );
    assert_eq!(third.warnings.len(), 1);
}

/// A delivery that failed after writing the notes (here on a non-UTF-8
/// person page) leaves Steno's render, which the retry does not take for
/// an edit; a copy it wrote beside an edited note is reused.
#[test]
fn a_retry_after_a_failed_delivery_takes_steno_s_render_for_steno_s() {
    for user_edited in [false, true] {
        let vault = Vault::new();
        let destination = vault.destination_with(false, Some("People"));
        let first = deliver(&destination, &export(), None);
        let edited = user_edited.then(|| edit_the_folder_note(&vault));
        let changed = with_a_changed_decision(&export());
        vault.write(ANNA_PAGE, latin1_page());
        assert!(matches!(
            destination.deliver_meeting(&changed, Some(&first)),
            Err(ObsidianError::ReadFailed { .. })
        ));
        vault.write(ANNA_PAGE, "# Anna\n");

        let retry = deliver(&destination, &changed, Some(&first));

        assert!(!vault.path(&copy(2)).exists(), "{:?}", vault.list(FOLDER));
        match edited {
            None => {
                assert_eq!(retry.warnings, Vec::<String>::new());
                assert_eq!(
                    vault.list(FOLDER),
                    without_audio(meeting_files(FOLDER_SLUG))
                );
                assert_eq!(
                    hash_in(&retry, &folder_note()),
                    Some(sha256(&vault.read(&folder_note())))
                );
            }
            Some(edited) => {
                assert_eq!(vault.text(&folder_note()), edited);
                assert!(
                    retry.warnings[0].contains(&format!("as {FOLDER_SLUG} (Steno 2026-10-08).md;"))
                );
                assert!(paths(&retry).contains(&copy(1)));
            }
        }
    }
}

/// Moving the copy over the note makes the note Steno's again, also when
/// the render changes right after.
#[test]
fn moving_the_copy_over_the_note_makes_it_steno_s_again() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let destination = vault.destination();
    let mut receipt = deliver(&destination, &export, None);
    for (render, next) in [
        (retitled(&export), retitled(&export)),
        (export.clone(), with_a_changed_decision(&export)),
    ] {
        edit_the_folder_note(&vault);
        let kept = deliver(&destination, &render, Some(&receipt));
        assert_eq!(kept.warnings.len(), 1, "{:?}", kept.warnings);
        fs::rename(vault.path(&copy(1)), vault.path(&folder_note())).unwrap();

        receipt = deliver(&destination, &next, Some(&kept));

        assert_eq!(receipt.warnings, Vec::<String>::new());
        assert_eq!(vault.list(FOLDER), meeting_files(FOLDER_SLUG), "no copy");
        assert_eq!(
            vault.read(&folder_note()),
            ArtifactRenderer::new()
                .render_folder_note(&next, &wikilink_tag(), Some(FOLDER_SLUG))
                .into_bytes()
        );
    }
}

/// An edited `meeting.json` (which still pins the folder) and
/// `transcript.vtt` are kept like notes, all kept files in one warning;
/// deleting them all, as it says, writes Steno's back in the same folder.
#[test]
fn edited_data_files_are_kept_and_deleting_the_kept_files_restores_steno_s() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let destination = vault.destination();
    let first = deliver(&destination, &export, None);
    let json = format!("{FOLDER}/meeting.json");
    let mut value: serde_json::Value = serde_json::from_slice(&vault.read(&json)).unwrap();
    value["myKey"] = serde_json::Value::from("mine");
    vault.write(&json, serde_json::to_vec_pretty(&value).unwrap());
    let vtt = format!("{FOLDER}/transcript.vtt");
    vault.write(&vtt, format!("{}\nNOTE mine\n", vault.text(&vtt)));
    edit_the_folder_note(&vault);
    let changed = with_a_changed_decision(&export);

    let kept = deliver(&destination, &changed, Some(&first));

    assert_eq!(kept.folder, FOLDER);
    assert_eq!(
        kept.warnings,
        [kept_several(&[
            "meeting.json",
            &format!("{FOLDER_SLUG}.md"),
            "transcript.vtt"
        ])]
    );
    assert_eq!(
        vault.read(&format!("{FOLDER}/meeting (Steno 2026-10-08).json")),
        ArtifactRenderer::new().render_json(&changed).unwrap()
    );
    assert!(
        vault
            .path(&format!("{FOLDER}/transcript (Steno 2026-10-08).vtt"))
            .is_file()
    );

    for path in [&json, &folder_note(), &vtt] {
        fs::remove_file(vault.path(path)).unwrap();
    }
    let again = deliver(&destination, &changed, Some(&kept));
    assert_eq!(again.folder, FOLDER);
    assert_eq!(vault.list("Meetings"), [FOLDER_SLUG]);
    assert_eq!(again.warnings, Vec::<String>::new());
    assert_eq!(
        vault.read(&json),
        ArtifactRenderer::new().render_json(&changed).unwrap()
    );
    assert!(vault.text(&vtt).starts_with("WEBVTT - Steno "));
}

/// A file at a rendered path the receipt does not list is left alone and
/// stays off the receipt, so a rolled-back Swift app, which writes any
/// listed path, never overwrites it.
#[test]
fn a_file_the_receipt_does_not_list_stays_off_the_receipt() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let mut first = deliver(&vault.destination(), &export, None);
    let vtt = format!("{FOLDER}/transcript.vtt");
    first.files.retain(|file| file.relative_path != vtt);
    vault.write(&vtt, "WEBVTT\n\nNOTE the user's own file\n");

    let second = deliver(&vault.destination(), &export, Some(&first));

    assert_eq!(vault.text(&vtt), "WEBVTT\n\nNOTE the user's own file\n");
    assert!(
        vault
            .path(&format!("{FOLDER}/transcript (Steno 2026-10-08).vtt"))
            .is_file()
    );
    assert_eq!(hash_in(&second, &vtt), None);
}

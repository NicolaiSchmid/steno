//! `ObsidianDestinationIntegrationTests`: the destination against a real
//! temp vault: first delivery, validation, re-export, collision,
//! preservation. Every test gets a fresh directory.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use steno_adapters::fs::AtomicFileWriter;
use steno_adapters::obsidian::{ManagedBlock, ObsidianError, ObsidianFolderDestination};
use steno_adapters::rendering::ArtifactRenderer;
use steno_core::content_hash::sha256;
use steno_core::json::uuid_string;
use steno_core::paths::{file_url, file_url_path};
use steno_core::{
    DeliveryReceipt, FileOwnership, MeetingExport, ObsidianSettings, Person, SpeakerAssignment,
};

struct Vault {
    directory: tempfile::TempDir,
    root: PathBuf,
}

impl Vault {
    fn new() -> Self {
        let directory = temp_dir("vault");
        let root = directory.path().join("vault");
        fs::create_dir_all(&root).unwrap();
        Vault { directory, root }
    }

    fn settings(&self, include_audio: bool, people_folder: Option<&str>) -> ObsidianSettings {
        settings(&self.root, include_audio, people_folder)
    }

    fn destination(&self) -> ObsidianFolderDestination {
        self.destination_with(true, Some("People"))
    }

    fn destination_with(
        &self,
        include_audio: bool,
        people_folder: Option<&str>,
    ) -> ObsidianFolderDestination {
        ObsidianFolderDestination::new(self.settings(include_audio, people_folder), BERLIN)
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
    fn export_with_audio(&self) -> MeetingExport {
        export_with_audio_in(self.directory.path())
    }
}

/// The settings every test configures: the fixture's `task` tag, the rest
/// as given.
fn settings(
    vault_path: &Path,
    include_audio: bool,
    people_folder: Option<&str>,
) -> ObsidianSettings {
    ObsidianSettings {
        vault_path: vault_path.to_string_lossy().into_owned(),
        people_folder: people_folder.map(str::to_owned),
        include_audio,
        task_tag: Some("task".to_owned()),
    }
}

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

fn paths(receipt: &DeliveryReceipt) -> Vec<String> {
    receipt
        .files
        .iter()
        .map(|f| f.relative_path.clone())
        .collect()
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
    let renderer = ArtifactRenderer::new();
    assert_eq!(
        vault.text(&format!("{FOLDER}/{FOLDER_SLUG}.md")),
        renderer.render_folder_note(&export, &wikilink_tag(), None)
    );
    assert_matches_golden(
        &vault.text(&format!("{FOLDER}/{FOLDER_SLUG}.md")),
        "snapshots/obsidian/folder-note-wikilink-berlin.md",
    );
    assert_matches_golden(
        &vault.text(&format!("{FOLDER}/{FOLDER_SLUG} - Transcript.md")),
        "snapshots/obsidian/transcript-wikilink.md",
    );
    assert_matches_golden(
        &vault.text(&format!("{FOLDER}/{FOLDER_SLUG} - Tasks.md")),
        "snapshots/obsidian/tasks-wikilink-tag.md",
    );
    assert_matches_golden(
        &vault.text(&format!("{FOLDER}/transcript.vtt")),
        "snapshots/obsidian/transcript.vtt",
    );
    assert_eq!(
        vault.read(&format!("{FOLDER}/meeting.json")),
        renderer.render_json(&export).unwrap(),
        "meeting.json carries this run's mixdown path, so it is compared with the renderer"
    );
    assert_eq!(vault.read(&format!("{FOLDER}/audio.m4a")), mixdown_bytes());
    assert_matches_golden(
        &vault.text("People/Anna Müller.md"),
        "snapshots/obsidian/person-page-new.md",
    );

    assert_eq!(receipt.root, vault.root.to_string_lossy());
    assert_eq!(receipt.folder, FOLDER);
    assert_eq!(receipt.renderer_version, ArtifactRenderer::VERSION);
    let mut expected: Vec<String> = meeting_files(FOLDER_SLUG)
        .iter()
        .map(|f| format!("{FOLDER}/{f}"))
        .collect();
    expected.extend([
        "People/Anna Müller.md".to_owned(),
        "People/Nicolai Schmid.md".to_owned(),
    ]);
    assert_eq!(paths(&receipt), expected);
    assert_eq!(
        receipt
            .files
            .iter()
            .filter(|f| f.ownership == FileOwnership::Owned)
            .count(),
        6
    );
    assert_eq!(managed(&receipt).len(), 2);
    for file in &receipt.files {
        assert_eq!(
            file.sha256,
            sha256(&vault.read(&file.relative_path)),
            "{} hash",
            file.relative_path
        );
    }
    assert!(
        vault
            .list(FOLDER)
            .iter()
            .all(|f| !f.starts_with(AtomicFileWriter::TEMPORARY_PREFIX))
    );
}

#[test]
fn validate_rejects_missing_unwritable_and_bad_people_folder() {
    let vault = Vault::new();
    let nope = vault.directory.path().join("nope");
    let missing = ObsidianFolderDestination::new(settings(&nope, false, None), BERLIN);
    assert_eq!(
        missing.validate_vault(),
        Err(ObsidianError::VaultMissing(
            nope.to_string_lossy().into_owned()
        ))
    );
    let file = vault.directory.path().join("file");
    fs::write(&file, b"").unwrap();
    let on_file = ObsidianFolderDestination::new(settings(&file, false, None), BERLIN);
    assert_eq!(
        on_file.validate_vault(),
        Err(ObsidianError::VaultMissing(
            file.to_string_lossy().into_owned()
        ))
    );

    // `./People` is refused too: the ledger refuses receipt paths with a
    // `.` component, so the destination refuses the folder up front.
    for bad in [
        "/People",
        "../People",
        "People/../..",
        "./People",
        "People/.",
        "People/./Notes",
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
    vault.destination_with(true, None).validate_vault().unwrap();
    assert!(vault.list("").is_empty(), "the probe leaves nothing behind");
    assert!(
        ObsidianError::AudioUnavailable
            .to_string()
            .contains("no audio mixdown")
    );
}

#[cfg(unix)]
#[test]
fn validate_and_deliver_report_an_unwritable_vault() {
    use std::os::unix::fs::PermissionsExt as _;
    let vault = Vault::new();
    let locked = vault.directory.path().join("locked");
    fs::create_dir_all(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(locked.join("probe"), b"").is_ok() {
        return; // root
    }
    let destination = ObsidianFolderDestination::new(settings(&locked, false, None), BERLIN);
    assert_eq!(
        destination.validate_vault(),
        Err(ObsidianError::VaultNotWritable(
            locked.to_string_lossy().into_owned()
        ))
    );
    match destination.deliver_meeting(&export(), None) {
        Err(ObsidianError::WriteFailed { path, .. }) => {
            assert!(path.starts_with(&*locked.to_string_lossy()));
        }
        other => panic!("expected WriteFailed, got {other:?}"),
    }
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn reexport_keeps_the_folder_user_files_and_edits_and_updates_the_title() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let first = deliver(&vault.destination(), &export, None);

    fs::write(vault.path(&format!("{FOLDER}/notes.md")), b"my notes\n").unwrap();
    let anna = "People/Anna Müller.md";
    let edited = format!(
        "Above the block.\n\n{}\nBelow the block.\n",
        vault.text(anna)
    );
    fs::write(vault.path(anna), edited).unwrap();
    let mut renamed = export.clone();
    renamed.meeting.title = "Neuer Titel nach dem Re-Run".to_owned();

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
    let mut expected = meeting_files(FOLDER_SLUG);
    expected.push("notes.md".to_owned());
    expected.sort();
    assert_eq!(vault.list(FOLDER), expected);
    assert_eq!(vault.text(&format!("{FOLDER}/notes.md")), "my notes\n");
    assert_eq!(
        vault.read(&format!("{FOLDER}/audio.m4a")),
        mixdown_bytes(),
        "an opted-out audio copy stays"
    );
    let note = vault.text(&format!("{FOLDER}/{FOLDER_SLUG}.md"));
    assert!(note.starts_with("---\ntitle: \"Neuer Titel nach dem Re-Run\"\n"));
    assert!(note.contains("# Neuer Titel nach dem Re-Run\n"));
    assert!(
        note.contains(&format!("[[{FOLDER_SLUG} - Transcript|Transcript]]")),
        "note links keep the pinned slug"
    );
    let page = vault.text(anna);
    assert!(page.starts_with("Above the block.\n\n---\n"));
    assert!(page.ends_with("<!-- steno:meetings:end -->\n\nBelow the block.\n"));
    assert!(page.contains(&format!(
        "[[{FOLDER_SLUG}|Neuer Titel nach dem Re-Run]] %%steno:"
    )));
    assert!(
        !page.contains("Roadmap für Q4]]"),
        "the old line for this meeting is replaced"
    );
    assert_eq!(paths(&second), paths(&first), "audio stays in the receipt");
    let audio = |receipt: &DeliveryReceipt| {
        receipt
            .files
            .iter()
            .find(|f| f.relative_path.ends_with("audio.m4a"))
            .cloned()
    };
    assert_eq!(audio(&second), audio(&first));
    assert_eq!(second.renderer_version, ArtifactRenderer::VERSION);
}

#[test]
fn unchanged_meeting_reexports_byte_identically() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let destination = vault.destination();
    let first = deliver(&destination, &export, None);
    let before: Vec<(String, Vec<u8>)> = first
        .files
        .iter()
        .map(|f| (f.relative_path.clone(), vault.read(&f.relative_path)))
        .collect();
    let second = deliver(&destination, &export, Some(&first));
    assert_eq!(second, first);
    for (path, data) in before {
        assert_eq!(vault.read(&path), data, "{path}");
    }
}

#[test]
fn collisions_get_a_suffix_and_a_crashed_attempt_is_reused() {
    let vault = Vault::new();
    let export = vault.export_with_audio();

    let mut other = export.clone();
    other.meeting.id = uuid(99);
    fs::create_dir_all(vault.path(FOLDER)).unwrap();
    fs::write(
        vault.path(&format!("{FOLDER}/meeting.json")),
        ArtifactRenderer::new().render_json(&other).unwrap(),
    )
    .unwrap();
    fs::create_dir_all(vault.path(&format!("{FOLDER}-2"))).unwrap();
    fs::write(vault.path(&format!("{FOLDER}-2/notes.md")), b"theirs\n").unwrap();

    let receipt = deliver(&vault.destination(), &export, None);
    assert_eq!(receipt.folder, format!("{FOLDER}-3"));
    assert_eq!(
        vault.list("Meetings"),
        [
            FOLDER_SLUG.to_owned(),
            format!("{FOLDER_SLUG}-2"),
            format!("{FOLDER_SLUG}-3")
        ]
    );
    assert_eq!(
        vault.list(&format!("{FOLDER}-2")),
        ["notes.md"],
        "the taken folders are untouched"
    );
    assert_eq!(vault.list(FOLDER), ["meeting.json"]);
    assert!(
        vault
            .list(&format!("{FOLDER}-3"))
            .contains(&format!("{FOLDER_SLUG}-3.md")),
        "notes are named after the suffixed folder"
    );
    let note = vault.text(&format!("{FOLDER}-3/{FOLDER_SLUG}-3.md"));
    assert!(note.contains(&format!("[[{FOLDER_SLUG}-3 - Transcript|Transcript]]")));
    assert!(
        vault
            .text("People/Anna Müller.md")
            .contains(&format!("[[{FOLDER_SLUG}-3|Produktstrategie"))
    );

    let again = deliver(&vault.destination(), &export, None);
    assert_eq!(again.folder, format!("{FOLDER}-3"));
    assert_eq!(vault.list("Meetings").len(), 3);
}

#[test]
fn a_crashed_attempt_is_reused_and_its_temporary_swept() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    fs::create_dir_all(vault.path(FOLDER)).unwrap();
    fs::write(
        vault.path(&format!("{FOLDER}/meeting.json")),
        ArtifactRenderer::new().render_json(&export).unwrap(),
    )
    .unwrap();
    let temporary = format!("{FOLDER}/.steno-tmp-deadbeef-{FOLDER_SLUG}.md");
    fs::write(vault.path(&temporary), b"half written\n").unwrap();

    let receipt = deliver(&vault.destination(), &export, None);
    assert_eq!(
        receipt.folder, FOLDER,
        "our meeting.json: the folder is ours"
    );
    assert_eq!(vault.list("Meetings"), [FOLDER_SLUG]);
    assert_eq!(
        vault.list(FOLDER),
        meeting_files(FOLDER_SLUG),
        "the temporary is gone and nothing else was added"
    );
    assert!(!vault.path(&temporary).exists());
}

#[test]
fn files_the_app_never_wrote_are_not_opened_on_reexport() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let first = deliver(
        &vault.destination_with(false, Some("People")),
        &export,
        None,
    );
    assert!(!paths(&first).iter().any(|p| p.ends_with("audio.m4a")));
    fs::write(vault.path(&format!("{FOLDER}/audio.m4a")), b"user audio\n").unwrap();
    let second = deliver(
        &vault.destination_with(true, Some("People")),
        &export,
        Some(&first),
    );
    assert_eq!(vault.text(&format!("{FOLDER}/audio.m4a")), "user audio\n");
    assert!(!paths(&second).iter().any(|p| p.ends_with("audio.m4a")));
}

#[test]
fn missing_mixdown_fails_after_every_other_file_is_written() {
    let vault = Vault::new();
    let mut export = export();
    export.audio.as_mut().unwrap().mixdown_url = None;
    assert_eq!(
        vault.destination().deliver_meeting(&export, None),
        Err(ObsidianError::AudioUnavailable)
    );
    assert_eq!(
        vault.list(FOLDER),
        without_audio(meeting_files(FOLDER_SLUG))
    );
    assert_eq!(vault.list("People").len(), 2);
}

#[test]
fn stale_temporaries_are_swept_and_people_off_writes_no_pages() {
    let vault = Vault::new();
    fs::create_dir_all(vault.path(FOLDER)).unwrap();
    fs::write(
        vault.path(&format!("{FOLDER}/.steno-tmp-x.md-00000000")),
        b"ours\n",
    )
    .unwrap();
    let export = vault.export_with_audio();
    let receipt = deliver(&vault.destination_with(true, None), &export, None);
    assert_eq!(receipt.folder, format!("{FOLDER}-2"));
    assert_eq!(
        vault.list(&format!("{FOLDER}-2")),
        meeting_files(&format!("{FOLDER_SLUG}-2"))
    );
    assert_eq!(
        vault.list(FOLDER),
        [".steno-tmp-x.md-00000000"],
        "another folder's temp is not ours to sweep"
    );
    assert!(!vault.path("People").exists());
    assert_eq!(receipt.files.len(), 6);
    let note = vault.text(&format!("{FOLDER}-2/{FOLDER_SLUG}-2.md"));
    assert!(
        note.contains("  - \"Anna Müller\"\n"),
        "no people folder, no links"
    );
}

#[test]
fn first_delivery_appends_the_block_to_a_person_page_the_user_already_wrote() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    fs::create_dir_all(vault.path("People")).unwrap();
    let user_page = "---\nrole: \"CEO\"\n---\n# Anna\n\nMet her at the fair.";
    fs::write(vault.path("People/Anna Müller.md"), user_page).unwrap();
    fs::write(vault.path("People/Bob.md"), b"Bob's page.\n").unwrap();

    let receipt = deliver(&vault.destination(), &export, None);

    let anna = vault.text("People/Anna Müller.md");
    assert!(anna.starts_with(&format!(
        "{user_page}\n\n<!-- steno:meetings:start -->\n- 2026-09-24 [["
    )));
    assert!(anna.ends_with("<!-- steno:meetings:end -->\n"));
    assert!(
        !anna.contains("steno_person_id"),
        "an existing page never gets Steno's frontmatter"
    );
    assert_eq!(vault.text("People/Bob.md"), "Bob's page.\n");
    assert_eq!(
        vault.list("People"),
        ["Anna Müller.md", "Bob.md", "Nicolai Schmid.md"]
    );
    assert!(!paths(&receipt).contains(&"People/Bob.md".to_owned()));
    assert_eq!(
        receipt
            .files
            .iter()
            .find(|f| f.relative_path == "People/Anna Müller.md")
            .unwrap()
            .ownership,
        FileOwnership::ManagedBlock
    );
}

#[test]
fn reexport_rewrites_owned_notes_and_recreates_a_deleted_one() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let destination = vault.destination();
    let first = deliver(&destination, &export, None);
    let note = format!("{FOLDER}/{FOLDER_SLUG}.md");
    let original = vault.read(&note);

    fs::write(
        vault.path(&note),
        format!("{}\nMy addition.\n", vault.text(&note)),
    )
    .unwrap();
    fs::remove_file(vault.path(&format!("{FOLDER}/transcript.vtt"))).unwrap();

    let second = deliver(&destination, &export, Some(&first));

    assert_eq!(
        vault.read(&note),
        original,
        "an owned note is rewritten from the model"
    );
    assert_eq!(
        vault.list(FOLDER),
        meeting_files(FOLDER_SLUG),
        "transcript.vtt is back"
    );
    assert_matches_golden(
        &vault.text(&format!("{FOLDER}/transcript.vtt")),
        "snapshots/obsidian/transcript.vtt",
    );
    assert_eq!(second, first);
}

#[test]
fn people_folder_off_keeps_the_pages_on_disk_and_in_the_receipt() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let first = deliver(&vault.destination(), &export, None);
    let anna = vault.read("People/Anna Müller.md");

    let second = deliver(&vault.destination_with(true, None), &export, Some(&first));

    assert_eq!(
        vault.list("People"),
        ["Anna Müller.md", "Nicolai Schmid.md"]
    );
    assert_eq!(
        vault.read("People/Anna Müller.md"),
        anna,
        "not touched, not deleted"
    );
    assert_eq!(paths(&second), paths(&first));
    assert_eq!(managed(&second).len(), 2);
    let note = vault.text(&format!("{FOLDER}/{FOLDER_SLUG}.md"));
    assert!(
        note.contains("  - \"Anna Müller\"\n"),
        "the notes stop linking people"
    );
    assert!(!note.contains("[[Anna"));
}

#[test]
fn a_second_meeting_on_the_same_day_gets_the_next_suffix_and_shares_the_person_pages() {
    let vault = Vault::new();
    let destination = vault.destination_with(false, Some("People"));
    let first = export();
    let mut second = first.clone();
    second.meeting.id = uuid(2);
    second.meeting.started_at = first.meeting.started_at + chrono::Duration::hours(3);

    let receipt_one = deliver(&destination, &first, None);
    let files_one: Vec<(String, Vec<u8>)> = receipt_one
        .files
        .iter()
        .map(|f| (f.relative_path.clone(), vault.read(&f.relative_path)))
        .collect();
    let receipt_two = deliver(&destination, &second, None);

    assert_eq!(receipt_one.folder, FOLDER);
    assert_eq!(receipt_two.folder, format!("{FOLDER}-2"));
    assert_eq!(
        vault.list("Meetings"),
        [FOLDER_SLUG.to_owned(), format!("{FOLDER_SLUG}-2")]
    );
    assert_eq!(
        vault.list(&format!("{FOLDER}-2")),
        without_audio(meeting_files(&format!("{FOLDER_SLUG}-2")))
    );
    for (path, data) in files_one
        .iter()
        .filter(|(path, _)| !path.starts_with("People/"))
    {
        assert_eq!(
            &vault.read(path),
            data,
            "{path}: the first meeting's files are untouched"
        );
    }
    let anna = vault.text("People/Anna Müller.md");
    let lines: Vec<String> = anna
        .split('\n')
        .filter(|l| l.starts_with("- 2026-09-24 "))
        .map(str::to_owned)
        .collect();
    assert_eq!(lines.len(), 2, "one line per meeting");
    assert!(anna.contains("%%steno:00000000-0000-0000-0000-000000000001%%"));
    assert!(anna.contains("%%steno:00000000-0000-0000-0000-000000000002%%"));
    assert!(
        anna.contains(&format!("[[{FOLDER_SLUG}-2|Produktstrategie")),
        "the line links the suffixed folder"
    );
    assert_eq!(anna.matches(ManagedBlock::START).count(), 1, "one block");
    assert_eq!(
        lines,
        ManagedBlock::sorted_newest_first(lines.clone()),
        "the block is in sorted order"
    );

    let again = deliver(&destination, &second, Some(&receipt_two));
    assert_eq!(again, receipt_two);
    assert_eq!(vault.list("Meetings").len(), 2);
}

#[test]
fn a_renamed_person_gets_a_new_page_and_the_old_one_stays() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let first = deliver(&vault.destination(), &export, None);
    let old_page = vault.text("People/Anna Müller.md");

    let mut renamed = export.clone();
    renamed.persons[0].display_name = "Anna Schulz".to_owned();
    renamed.participants[0].display_name = "Anna Schulz".to_owned();
    let second = deliver(&vault.destination(), &renamed, Some(&first));

    assert_eq!(
        vault.list("People"),
        ["Anna Müller.md", "Anna Schulz.md", "Nicolai Schmid.md"]
    );
    let marker = ManagedBlock::marker(export.meeting.id);
    assert_eq!(
        vault.text("People/Anna Müller.md"),
        ManagedBlock::remove(export.meeting.id, &old_page),
        "never deleted; only this meeting's line leaves the old page"
    );
    assert!(!vault.text("People/Anna Müller.md").contains(&marker));
    let new_page = vault.text("People/Anna Schulz.md");
    assert!(new_page.contains(&marker));
    assert!(new_page.contains("# Anna Schulz\n"));
    assert!(new_page.contains("steno_person_id: \"00000000-0000-0000-0000-00000000000a\""));
    assert_eq!(
        managed(&second),
        [
            "People/Anna Müller.md",
            "People/Anna Schulz.md",
            "People/Nicolai Schmid.md"
        ],
        "the old page stays in the receipt"
    );
    let transcript = vault.text(&format!("{FOLDER}/{FOLDER_SLUG} - Transcript.md"));
    assert!(transcript.contains("## [[Anna Schulz]] — 00:00:04"));
    assert!(
        !transcript.contains("Anna Müller"),
        "every note uses the current name"
    );
}

#[test]
fn an_unchanged_stale_page_is_not_opened_for_writing() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let first = deliver(&vault.destination(), &export, None);
    let mut renamed = export.clone();
    renamed.persons[0].display_name = "Anna Schulz".to_owned();
    renamed.participants[0].display_name = "Anna Schulz".to_owned();
    let second = deliver(&vault.destination(), &renamed, Some(&first));
    let anna = "People/Anna Müller.md";
    let before = vault.read(anna);
    assert!(
        !before.windows(7).any(|w| w == b"%%steno"),
        "the line is gone"
    );
    let metadata = fs::metadata(vault.path(anna)).unwrap();

    // A rename-based writer replaces a read-only file as well, so the
    // identity of the file on disk is what shows it was never opened.
    let third = deliver(&vault.destination(), &renamed, Some(&second));

    assert_eq!(third, second);
    assert_eq!(vault.read(anna), before);
    let after = fs::metadata(vault.path(anna)).unwrap();
    assert_eq!(after.modified().unwrap(), metadata.modified().unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(after.ino(), metadata.ino(), "the same file, not a rewrite");
    }
}

#[test]
fn a_pinned_folder_with_a_trailing_slash_still_names_the_notes_after_it() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let pinned = DeliveryReceipt {
        root: vault.root.to_string_lossy().into_owned(),
        folder: format!("{FOLDER}/"),
        files: vec![],
        renderer_version: ArtifactRenderer::VERSION,
    };

    let receipt = deliver(&vault.destination(), &export, Some(&pinned));

    assert_eq!(
        receipt.folder,
        format!("{FOLDER}/"),
        "the pin is kept as stored"
    );
    assert_eq!(vault.list(FOLDER), meeting_files(FOLDER_SLUG));
    let note = vault.text(&format!("{FOLDER}/{FOLDER_SLUG}.md"));
    assert!(note.contains(&format!("[[{FOLDER_SLUG} - Transcript|Transcript]]")));
    assert!(
        vault
            .text("People/Anna Müller.md")
            .contains(&format!("[[{FOLDER_SLUG}|Produktstrategie")),
        "the person line links the folder, not an empty slug"
    );
}

#[test]
fn redeliver_after_reassignment_removes_the_old_person_line() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let destination = vault.destination();
    let first = deliver(&destination, &export, None);
    let anna = "People/Anna Müller.md";
    let marker = ManagedBlock::marker(export.meeting.id);

    let older = format!(
        "- 2026-08-01 [[2026-08-01-kickoff|Kickoff]] {}",
        ManagedBlock::marker(uuid(9))
    );
    let above = format!("Above the block, with {marker} mentioned.\n\n");
    let below = "\nBelow the block.\n";
    let block = ManagedBlock::merge(&older, uuid(9), &vault.text(anna));
    fs::write(vault.path(anna), format!("{above}{block}{below}")).unwrap();

    // Speaker 1 turns out to be Bea, a new person; Anna leaves the export.
    let bea_id = uuid(12);
    let mut reassigned = export.clone();
    reassigned.speakers[1].assignment = SpeakerAssignment::Confirmed { person_id: bea_id };
    reassigned.persons[0] = Person {
        id: bea_id,
        display_name: "Bea Braun".to_owned(),
        email: None,
        embedding: None,
        sample_count: 1,
        created_at: created_at(),
    };
    reassigned.participants[0].person_id = Some(bea_id);
    reassigned.participants[0].display_name = "Bea Braun".to_owned();
    reassigned.participants[0].email = None;
    let second = deliver(&destination, &reassigned, Some(&first));

    assert_eq!(
        vault.list("People"),
        ["Anna Müller.md", "Bea Braun.md", "Nicolai Schmid.md"]
    );
    let page = vault.text(anna);
    assert!(
        page.starts_with(&above),
        "bytes above the block are untouched"
    );
    assert!(
        page.ends_with(&format!("{}\n{below}", ManagedBlock::END)),
        "bytes below the block are untouched"
    );
    let inside = page
        .split(ManagedBlock::START)
        .nth(1)
        .unwrap()
        .split(ManagedBlock::END)
        .next()
        .unwrap();
    assert_eq!(
        inside,
        format!("\n{older}\n"),
        "only this meeting's line left the block"
    );
    assert!(
        vault.text("People/Bea Braun.md").contains(&marker),
        "Bea's page lists the meeting"
    );
    assert_eq!(
        managed(&second),
        [anna, "People/Bea Braun.md", "People/Nicolai Schmid.md"],
        "Anna's page stays in the receipt"
    );
    assert_eq!(
        second
            .files
            .iter()
            .find(|f| f.relative_path == anna)
            .unwrap()
            .sha256,
        sha256(&vault.read(anna)),
        "with the hash of what is on disk"
    );

    let before = vault.read(anna);
    let third = deliver(&destination, &reassigned, Some(&second));
    assert_eq!(vault.read(anna), before);
    assert_eq!(third, second);

    // Anna's page deleted by the user is skipped, not recreated; a page that
    // is not UTF-8 text is left as it is, and neither fails the delivery.
    fs::remove_file(vault.path(anna)).unwrap();
    let mut binary = vec![0xFF, 0xFE, 0x00, 0x41];
    binary.extend(ManagedBlock::block(std::slice::from_ref(&marker)).into_bytes());
    fs::write(vault.path("People/Nicolai Schmid.md"), &binary).unwrap();
    let mut without_nicolai = reassigned.clone();
    without_nicolai.persons.pop();
    let fourth = deliver(&destination, &without_nicolai, Some(&third));
    assert!(!vault.path(anna).exists());
    assert_eq!(vault.read("People/Nicolai Schmid.md"), binary);
    assert_eq!(paths(&fourth), paths(&third));
}

#[test]
fn a_moved_vault_is_written_fresh_under_the_pinned_folder_and_the_old_one_is_left_alone() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let first = deliver(&vault.destination(), &export, None);
    let before: Vec<(String, Vec<u8>)> = first
        .files
        .iter()
        .map(|f| (f.relative_path.clone(), vault.read(&f.relative_path)))
        .collect();

    let moved = vault.directory.path().join("moved");
    fs::create_dir_all(&moved).unwrap();
    let destination =
        ObsidianFolderDestination::new(settings(&moved, true, Some("People")), BERLIN);
    let second = deliver(&destination, &export, Some(&first));

    assert_eq!(second.root, moved.to_string_lossy());
    assert_eq!(
        second.folder, first.folder,
        "another root is a first delivery; the empty vault resolves to the same name"
    );
    assert_eq!(paths(&second), paths(&first));
    for (path, data) in &before {
        assert_eq!(
            &fs::read(moved.join(path)).unwrap(),
            data,
            "{path} in the new vault"
        );
        assert_eq!(&vault.read(path), data, "{path} in the old vault");
    }
}

#[test]
fn a_mixdown_path_without_a_file_is_audio_unavailable() {
    let vault = Vault::new();
    let mut export = export();
    export.audio.as_mut().unwrap().mixdown_url =
        Some(file_url(&vault.directory.path().join("gone.m4a"), false));
    assert_eq!(
        vault.destination().deliver_meeting(&export, None),
        Err(ObsidianError::AudioUnavailable)
    );
    assert_eq!(
        vault.list(FOLDER),
        without_audio(meeting_files(FOLDER_SLUG))
    );
    let no_audio = deliver(
        &vault.destination_with(false, Some("People")),
        &export,
        None,
    );
    assert_eq!(
        no_audio.files.len(),
        7,
        "with audio off the missing mixdown is no error"
    );
}

#[test]
fn a_swept_mixdown_is_no_error_when_the_audio_copy_is_already_in_the_vault() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let first = deliver(&vault.destination(), &export, None);
    let audio_file = first
        .files
        .iter()
        .find(|f| f.relative_path.ends_with("audio.m4a"))
        .cloned()
        .unwrap();

    fs::remove_file(vault.directory.path().join("audio.m4a")).unwrap();
    let mut renamed = export.clone();
    renamed.meeting.title = "Nach dem Sweep".to_owned();
    let second = deliver(&vault.destination(), &renamed, Some(&first));
    assert!(
        vault
            .text(&format!("{FOLDER}/{FOLDER_SLUG}.md"))
            .contains("# Nach dem Sweep\n")
    );
    assert_eq!(
        second
            .files
            .iter()
            .find(|f| f.relative_path.ends_with("audio.m4a")),
        Some(&audio_file)
    );
    assert_eq!(vault.read(&format!("{FOLDER}/audio.m4a")), mixdown_bytes());

    renamed.audio.as_mut().unwrap().mixdown_url = None;
    let third = deliver(&vault.destination(), &renamed, Some(&second));
    assert_eq!(paths(&third), paths(&second));
    assert_eq!(
        third
            .files
            .iter()
            .find(|f| f.relative_path.ends_with("audio.m4a")),
        Some(&audio_file)
    );
}

#[test]
fn a_users_audio_file_counts_when_the_mixdown_is_gone() {
    let vault = Vault::new();
    let mut export = vault.export_with_audio();
    let first = deliver(
        &vault.destination_with(false, Some("People")),
        &export,
        None,
    );
    fs::write(vault.path(&format!("{FOLDER}/audio.m4a")), b"user audio\n").unwrap();
    export.audio.as_mut().unwrap().mixdown_url = None;

    let second = deliver(
        &vault.destination_with(true, Some("People")),
        &export,
        Some(&first),
    );
    assert_eq!(vault.text(&format!("{FOLDER}/audio.m4a")), "user audio\n");
    assert!(!paths(&second).iter().any(|p| p.ends_with("audio.m4a")));
    assert_eq!(second.files.len(), first.files.len());
}

#[test]
fn an_unreadable_mixdown_is_a_read_failure_not_missing_audio() {
    let vault = Vault::new();
    let mut export = export();
    let directory = vault.directory.path().join("mixdown-dir.m4a");
    fs::create_dir_all(&directory).unwrap();
    export.audio.as_mut().unwrap().mixdown_url = Some(file_url(&directory, false));
    match vault.destination().deliver_meeting(&export, None) {
        Err(error) => {
            let ObsidianError::ReadFailed { path, .. } = &error else {
                panic!("expected ReadFailed, got {error:?}");
            };
            assert_eq!(Path::new(path), directory);
            assert!(
                error
                    .to_string()
                    .starts_with(&format!("Could not read {}: ", directory.display()))
            );
        }
        other => panic!("expected ReadFailed, got {other:?}"),
    }
}

#[test]
fn a_receipt_from_another_root_is_a_first_delivery_with_the_collision_rule() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let first = deliver(&vault.destination(), &export, None);

    let other = vault.directory.path().join("other");
    let mut theirs = export.clone();
    theirs.meeting.id = uuid(99);
    fs::create_dir_all(other.join(FOLDER)).unwrap();
    fs::write(
        other.join(FOLDER).join("meeting.json"),
        ArtifactRenderer::new().render_json(&theirs).unwrap(),
    )
    .unwrap();
    let destination =
        ObsidianFolderDestination::new(settings(&other, true, Some("People")), BERLIN);

    let second = deliver(&destination, &export, Some(&first));

    assert_eq!(second.root, other.to_string_lossy());
    assert_eq!(
        second.folder,
        format!("{FOLDER}-2"),
        "the other root does not pin the folder"
    );
    assert_eq!(
        second.files.len(),
        8,
        "every file is written, nothing is carried over"
    );
    assert_eq!(
        list(&other.join(FOLDER)),
        ["meeting.json"],
        "the taken folder is untouched"
    );
    assert_eq!(
        list(&other.join(format!("{FOLDER}-2"))),
        meeting_files(&format!("{FOLDER_SLUG}-2"))
    );
    assert_eq!(
        vault.list(FOLDER),
        meeting_files(FOLDER_SLUG),
        "the first vault is left alone"
    );
}

#[test]
fn another_spelling_of_the_vault_path_is_the_same_root() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    let first = deliver(&vault.destination(), &export, None);
    let mut renamed = export.clone();
    renamed.meeting.title = "Neuer Titel".to_owned();

    let root = vault.root.to_string_lossy().into_owned();
    for spelling in [format!("{root}/"), format!("{root}/./Meetings/..")] {
        let destination = ObsidianFolderDestination::new(
            settings(Path::new(&spelling), true, Some("People")),
            BERLIN,
        );
        let second = deliver(&destination, &renamed, Some(&first));
        assert_eq!(second.folder, first.folder, "{spelling}");
        assert_eq!(paths(&second), paths(&first), "{spelling}");
        assert_eq!(
            second.root, spelling,
            "the receipt records the path as configured"
        );
        assert!(
            vault
                .text(&format!("{FOLDER}/{FOLDER_SLUG}.md"))
                .contains("# Neuer Titel\n"),
            "{spelling}: a re-export"
        );
        assert_eq!(
            vault.list("Meetings"),
            [FOLDER_SLUG],
            "{spelling}: no second folder"
        );
    }
}

#[test]
fn a_person_page_that_is_not_utf8_is_left_alone_and_reported() {
    let vault = Vault::new();
    let export = vault.export_with_audio();
    fs::create_dir_all(vault.path("People")).unwrap();
    let mut latin1 = b"# Anna M".to_vec();
    latin1.push(0xFC);
    latin1.extend(b"ller\n");
    fs::write(vault.path("People/Anna Müller.md"), &latin1).unwrap();

    match vault.destination().deliver_meeting(&export, None) {
        Err(ObsidianError::ReadFailed { path, underlying }) => {
            assert_eq!(Path::new(&path), vault.path("People/Anna Müller.md"));
            assert!(underlying.contains("not UTF-8"));
        }
        other => panic!("expected ReadFailed, got {other:?}"),
    }
    assert_eq!(
        vault.read("People/Anna Müller.md"),
        latin1,
        "no byte was replaced"
    );
    assert_eq!(
        vault.list(FOLDER),
        without_audio(meeting_files(FOLDER_SLUG))
    );
}

#[test]
fn the_receipt_carries_the_renderer_version() {
    let directory = temp_dir("version");
    let destination =
        ObsidianFolderDestination::new(settings(directory.path(), false, None), BERLIN);
    let receipt = deliver(&destination, &export(), None);
    assert_eq!(receipt.renderer_version, ArtifactRenderer::VERSION);
    let recorded = fixture_text("snapshots/obsidian/VERSION");
    assert!(recorded.starts_with(&format!("{} ", ArtifactRenderer::VERSION)));
}

#[test]
fn the_sample_clip_url_round_trips_as_a_file_url() {
    let clip = format!(
        "file:///tmp/steno/{}/speakers/x.wav",
        uuid_string(meeting_id())
    );
    assert_eq!(
        file_url_path(&clip),
        Some(PathBuf::from(format!(
            "/tmp/steno/{}/speakers/x.wav",
            uuid_string(meeting_id())
        )))
    );
    assert_eq!(
        file_url_path("file:///tmp/a%20b/%C3%BC.wav"),
        Some(PathBuf::from("/tmp/a b/ü.wav"))
    );
    assert_eq!(file_url_path("https://example.com/x"), None);
}

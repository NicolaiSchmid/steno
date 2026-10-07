//! Every fixture in `apps/macos/web/fixtures/bridge/` decodes into its Rust
//! type and re-encodes byte for byte. The mapping from file name to type
//! mirrors `BridgeSamples.fixtures` in `Sources/StenoBridge/BridgeSamples.swift`,
//! followed by the fixtures the Rust bridge alone owns (the import step);
//! `contract.ts` is checked for every string enum the crate spells, top level
//! and nested. Plan: `.plans/2026-10-02-rust-core-and-tauri-shell.md`.

mod common;

use std::collections::BTreeSet;
use std::fs;

use common::{bridge_fixtures_dir as fixtures_dir, repository_root};
use serde::Serialize;
use serde::de::DeserializeOwned;
use similar::TextDiff;
use steno_bridge::*;

/// Raw values in declaration order, for the `contract.ts` comparisons.
fn raw<T: std::fmt::Display>(all: &[T]) -> Vec<String> {
    all.iter().map(ToString::to_string).collect()
}

type Reencode = fn(&[u8]) -> Result<Vec<u8>, String>;

/// Decodes the bytes as `T` and re-encodes them as the fixture file would be
/// written: `BridgeJSON.encode` plus a trailing newline.
fn reencode<T: DeserializeOwned + Serialize>(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let value: T = serde_json::from_slice(bytes).map_err(|e| format!("decode: {e}"))?;
    let mut out = json::to_canonical_string(&value).map_err(|e| format!("encode: {e}"))?;
    out.push('\n');
    Ok(out.into_bytes())
}

/// File name to type, in the order of `BridgeSamples.fixtures`.
const FIXTURES: &[(&str, Reencode)] = &[
    ("app", reencode::<AppSnapshot>),
    ("recording", reencode::<RecordingSnapshot>),
    ("recording.live", reencode::<RecordingSnapshot>),
    ("progress", reencode::<ProgressSnapshot>),
    ("meetings.list", reencode::<MeetingsListSnapshot>),
    ("meeting.detail", reencode::<MeetingDetailSnapshot>),
    ("settings.general", reencode::<GeneralSettingsSnapshot>),
    ("settings.recording", reencode::<RecordingSettingsSnapshot>),
    (
        "settings.transcription",
        reencode::<TranscriptionSettingsSnapshot>,
    ),
    ("settings.summaries", reencode::<SummariesSettingsSnapshot>),
    (
        "settings.summaries.codex",
        reencode::<SummariesSettingsSnapshot>,
    ),
    (
        "settings.summaries.fileKey",
        reencode::<SummariesSettingsSnapshot>,
    ),
    ("settings.export", reencode::<ExportSettingsSnapshot>),
    ("settings.iphone", reencode::<PhoneSettingsSnapshot>),
    ("settings.iphone.pairing", reencode::<PhoneSettingsSnapshot>),
    ("onboarding", reencode::<OnboardingSnapshot>),
    ("onboarding.setup", reencode::<OnboardingSnapshot>),
    ("envelope.request", reencode::<BridgeRequest>),
    ("envelope.reply", reencode::<BridgeReply>),
    ("envelope.error", reencode::<BridgeReply>),
    ("envelope.event", reencode::<BridgeEvent>),
    ("speakers.options.reply", reencode::<SpeakerOptionsReply>),
    ("params.page.layout", reencode::<PageLayoutParams>),
    ("params.meetings.setFilter", reencode::<SetFilterParams>),
    (
        "params.meetings.setTagFilter",
        reencode::<SetTagFilterParams>,
    ),
    ("params.meetings.setQuery", reencode::<SetQueryParams>),
    ("params.meetingID", reencode::<MeetingIdParams>),
    ("params.meeting.setTab", reencode::<SetTabParams>),
    ("params.meeting.setTags", reencode::<SetTagsParams>),
    ("params.meeting.setTemplate", reencode::<SetTemplateParams>),
    ("params.bool", reencode::<SetBoolParams>),
    ("params.string", reencode::<SetStringParams>),
    ("params.meeting.saveNotes", reencode::<SaveNotesParams>),
    ("params.speakers.options", reencode::<SpeakerOptionsParams>),
    ("params.speakers.select", reencode::<SelectSpeakerParams>),
    ("params.speakerID", reencode::<SpeakerIdParams>),
    ("params.recording.start", reencode::<StartRecordingParams>),
    (
        "params.settings.recording.setRetention",
        reencode::<SetRetentionParams>,
    ),
    ("params.permissionKind", reencode::<PermissionKindParams>),
    ("params.assetID", reencode::<AssetIdParams>),
    (
        "params.settings.general.setAutomaticUpdates",
        reencode::<SetAutomaticUpdatesParams>,
    ),
    (
        "params.settings.summaries.update",
        reencode::<SummariesUpdateParams>,
    ),
    (
        "params.settings.export.update",
        reencode::<ExportUpdateParams>,
    ),
    ("params.deviceID", reencode::<DeviceIdParams>),
    ("params.onboarding.setupStep", reencode::<SetupStepParams>),
    ("params.system.openURL", reencode::<OpenUrlParams>),
    ("params.window", reencode::<WindowParams>),
    (
        "params.ui.confirmDestructive",
        reencode::<ConfirmDestructiveParams>,
    ),
    ("reply.confirm", reencode::<ConfirmReply>),
    ("reply.chosenPath", reencode::<ChosenPathReply>),
    // Rust only, after Swift's list (`BridgeFixturesTests.rustOnly`).
    ("onboarding.import", reencode::<OnboardingSnapshot>),
];

fn index() -> Vec<String> {
    let bytes = fs::read(fixtures_dir().join("index.json")).expect("index.json");
    serde_json::from_slice(&bytes).expect("index.json is a list of names")
}

fn files_on_disk() -> BTreeSet<String> {
    fs::read_dir(fixtures_dir())
        .expect("fixtures directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter_map(|name| name.strip_suffix(".json").map(str::to_owned))
        .filter(|name| name != "index")
        .collect()
}

#[test]
fn every_fixture_round_trips_byte_for_byte() {
    let mut failures = Vec::new();
    for (name, reencode) in FIXTURES {
        let path = fixtures_dir().join(format!("{name}.json"));
        let on_disk = fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(
            !on_disk.windows(2).any(|w| w == b"\r\n"),
            "{name}.json has CRLF line endings; .gitattributes pins the fixtures to LF"
        );
        match reencode(&on_disk) {
            Ok(again) if again == on_disk => {}
            Ok(again) => {
                let diff = TextDiff::from_lines(
                    String::from_utf8_lossy(&on_disk).as_ref(),
                    String::from_utf8_lossy(&again).as_ref(),
                )
                .unified_diff()
                .header(&format!("{name}.json"), "re-encoded")
                .to_string();
                failures.push(format!("{name}.json does not round-trip:\n{diff}"));
            }
            Err(error) => failures.push(format!("{name}.json: {error}")),
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn every_fixture_file_has_a_rust_type_and_every_type_a_file() {
    let mapped: BTreeSet<String> = FIXTURES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    assert_eq!(
        mapped.len(),
        FIXTURES.len(),
        "duplicate fixture names in the mapping"
    );
    let on_disk = files_on_disk();
    let unmapped: Vec<_> = on_disk.difference(&mapped).collect();
    assert!(
        unmapped.is_empty(),
        "fixtures without a Rust type: {unmapped:?}"
    );
    let missing: Vec<_> = mapped.difference(&on_disk).collect();
    assert!(
        missing.is_empty(),
        "Rust types without a fixture file: {missing:?}"
    );
}

#[test]
fn the_mapping_follows_index_json() {
    let names: Vec<&str> = FIXTURES.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        index(),
        names,
        "index.json and the Rust mapping list different fixtures"
    );
}

#[test]
fn every_topic_has_a_snapshot_fixture() {
    let on_disk = files_on_disk();
    for topic in BridgeTopic::ALL {
        assert!(
            on_disk.contains(topic.as_str()),
            "no fixture for topic {topic}"
        );
    }
}

/// The string literals of one list in `contract.ts`, found by walking the
/// anchors in order: the last anchor ends in the list's `[`, the earlier ones
/// narrow the search to the right declaration (`export const
/// recordingSnapshot = z`, then `state: z.enum([`). Both spellings the file
/// uses, `export const bridgeTopics = [` and `export const listFilter = z.enum([`,
/// are anchors in this sense.
fn contract_ts_strings(anchors: &[&str]) -> Vec<String> {
    let path = repository_root().join("apps/macos/web/src/bridge/contract.ts");
    let source = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut rest = source.as_str();
    for anchor in anchors {
        let at = rest
            .find(anchor)
            .unwrap_or_else(|| panic!("contract.ts has no `{anchor}` after {anchors:?}"));
        rest = &rest[at + anchor.len()..];
    }
    assert!(
        anchors.last().is_some_and(|last| last.ends_with('[')),
        "the last anchor opens the list: {anchors:?}"
    );
    let close = rest.find(']').expect("the list closes");
    rest[..close]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

/// `export const <name> = [` or `export const <name> = z.enum([`.
fn top_level(name: &str) -> Vec<String> {
    let path = repository_root().join("apps/macos/web/src/bridge/contract.ts");
    let source = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let plain = format!("export const {name} = [");
    let zod = format!("export const {name} = z.enum([");
    if source.contains(&plain) {
        contract_ts_strings(&[&plain])
    } else {
        contract_ts_strings(&[&zod])
    }
}

#[test]
fn contract_ts_topics_methods_and_error_codes_match() {
    assert_eq!(top_level("bridgeTopics"), raw(BridgeTopic::ALL));
    assert_eq!(top_level("bridgeMethods"), raw(BridgeMethod::ALL));
    assert_eq!(
        contract_ts_strings(&["export const bridgeError = z", "code: z.enum(["]),
        raw(BridgeErrorCode::ALL)
    );
}

#[test]
fn contract_ts_shared_vocabulary_matches() {
    assert_eq!(top_level("permissionKind"), raw(PermissionKind::ALL));
    assert_eq!(top_level("permissionState"), raw(PermissionState::ALL));
    assert_eq!(top_level("settingsSection"), raw(SettingsSection::ALL));
    assert_eq!(top_level("bridgeWindow"), raw(BridgeWindow::ALL));
    assert_eq!(top_level("meetingSource"), raw(MeetingSource::ALL));
    assert_eq!(top_level("meetingState"), raw(MeetingState::ALL));
    assert_eq!(top_level("captureMode"), raw(CaptureMode::ALL));
    assert_eq!(top_level("listFilter"), raw(ListFilter::ALL));
    assert_eq!(top_level("detailTab"), raw(DetailTab::ALL));
    assert_eq!(top_level("retentionMode"), raw(RetentionMode::ALL));
    assert_eq!(top_level("platform"), raw(Platform::ALL));
}

/// `READS_CALENDAR` in `src/lib/platform.tsx`, as Biome writes it,
/// against whether `PermissionKind::for_platform` lists the calendar:
/// the page shows General's calendar row by it.
#[test]
fn platform_tsx_reads_the_calendar_where_the_platform_lists_it() {
    let path = repository_root().join("apps/macos/web/src/lib/platform.tsx");
    let source = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let entries: Vec<String> = Platform::ALL
        .iter()
        .map(|platform| {
            let reads = PermissionKind::for_platform(*platform).contains(&PermissionKind::Calendar);
            format!("\t{platform}: {reads},")
        })
        .collect();
    let record = format!(
        "const READS_CALENDAR: Record<PlatformOS, boolean> = {{\n{}\n}};",
        entries.join("\n")
    );
    assert!(
        source.replace("\r\n", "\n").contains(&record),
        "platform.tsx has no\n{record}"
    );
}

/// The `z.enum` lists nested in a snapshot or params schema, against the
/// Rust enum the matching field uses; a new case on either side fails here.
#[test]
fn contract_ts_nested_enums_match() {
    let nested = |anchors: &[&str]| contract_ts_strings(anchors);
    assert_eq!(
        nested(&["export const recordingSnapshot = z", "state: z.enum(["]),
        raw(RecordingState::ALL)
    );
    let detail = "export const meetingDetailSnapshot = z";
    assert_eq!(
        nested(&[detail, "retention: z", "kind: z.enum(["]),
        raw(DetailRetentionKind::ALL)
    );
    assert_eq!(
        nested(&[detail, "speakers: z.array(", "assignment: z.enum(["]),
        raw(SpeakerAssignmentKind::ALL)
    );
    assert_eq!(
        nested(&[detail, "summaryStatus: z", "kind: z.enum(["]),
        raw(DetailSummaryStatusKind::ALL)
    );
    assert_eq!(
        nested(&[detail, "tasks: z.array(", "priority: z.enum(["]),
        raw(TaskPriority::ALL)
    );
    assert_eq!(
        nested(&[detail, "export: z", "status: z.enum(["]),
        raw(DetailExportStatus::ALL)
    );
    assert_eq!(
        nested(&["export const acknowledgement = z", "group: z.enum(["]),
        raw(GeneralAcknowledgementGroup::ALL)
    );
    let general = "export const generalSettingsSnapshot = z";
    assert_eq!(
        nested(&[general, "loginItem: z.enum(["]),
        raw(GeneralLoginItem::ALL)
    );
    assert_eq!(
        nested(&[general, "updates: z", "outcome: z.enum(["]),
        raw(GeneralUpdatesOutcome::ALL)
    );
    assert_eq!(
        nested(&[
            "export const recordingSettingsSnapshot = z",
            "folderUsage: z.enum(["
        ]),
        raw(RecordingFolderUsage::ALL)
    );
    assert_eq!(
        nested(&[
            "export const transcriptionSettingsSnapshot = z",
            "assets: z.array(",
            "state: z.enum(["
        ]),
        raw(TranscriptionAssetState::ALL)
    );
    assert_eq!(
        nested(&[
            "export const summariesSettingsSnapshot = z",
            "codex: z",
            "signIn: z.enum(["
        ]),
        raw(SummariesCodexSignIn::ALL)
    );
    assert_eq!(
        nested(&[
            "export const summariesSettingsSnapshot = z",
            "keyStore: z.enum(["
        ]),
        raw(SummariesKeyStore::ALL)
    );
    assert_eq!(
        nested(&[
            "export const phoneSettingsSnapshot = z",
            "listener: z",
            "state: z.enum(["
        ]),
        raw(PhoneListenerState::ALL)
    );
}

/// As [`contract_ts_nested_enums_match`], for onboarding and the speaker
/// options.
#[test]
fn contract_ts_nested_onboarding_enums_match() {
    let nested = |anchors: &[&str]| contract_ts_strings(anchors);
    let onboarding = "export const onboardingSnapshot = z";
    assert_eq!(
        nested(&[onboarding, "page: z.enum(["]),
        raw(OnboardingPage::ALL)
    );
    assert_eq!(
        nested(&[onboarding, "setup: z.array(", "kind: z.enum(["]),
        raw(OnboardingSetupStepKind::ALL)
    );
    assert_eq!(
        nested(&[onboarding, "setup: z.array(", "state: z.enum(["]),
        raw(OnboardingSetupStepState::ALL)
    );
    assert_eq!(
        nested(&[onboarding, "swiftImport: z", "state: z.enum(["]),
        raw(OnboardingImportState::ALL)
    );
    assert_eq!(
        nested(&["export const speakerOption = z", "kind: z.enum(["]),
        raw(SpeakerOptionKind::ALL)
    );
    assert_eq!(
        nested(&["export const setupStepParams = z", "step: z.enum(["]),
        raw(OnboardingSetupStepKind::ALL)
    );
}

#[test]
fn meeting_detail_accepts_null() {
    let detail: Option<MeetingDetailSnapshot> = serde_json::from_str("null").unwrap();
    assert!(detail.is_none());
    let event = BridgeEvent::snapshot(BridgeTopic::MeetingDetail, &detail).unwrap();
    assert_eq!(
        json::to_compact_string(&event).unwrap(),
        r#"{"payload":null,"topic":"meeting.detail"}"#
    );
}

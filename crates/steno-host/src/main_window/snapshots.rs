//! The main window's topics as pure mappings from view model state to the
//! contract snapshots, after `Web/MainWindowSnapshots.swift`. No store
//! access here: the view models own every rule, these spell their state in
//! the wire vocabulary.

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset, Utc};
use steno_bridge::{
    AppPhone, AppSetupBanner, AppSnapshot, DetailExport, DetailExportStatus, DetailRetention,
    DetailRetentionKind, DetailSpeaker, DetailSummaryStatus, DetailSummaryStatusKind, DetailTask,
    DetailTemplate, DetailTurn, ListCounts, ListDayGroup, ListFilter, ListTag,
    MeetingDetailSnapshot, MeetingRow, MeetingSource as BridgeSource, MeetingState as BridgeState,
    MeetingsListSnapshot, Platform, ProgressEntry as BridgeProgressEntry, ProgressSnapshot,
    RecordingAutoStop, RecordingLevel, RecordingSnapshot, SettingsSection, SpeakerChip,
};
use steno_core::{
    Delivery, DeliveryStatus, Meeting, MeetingSource, MeetingStateKind, Person, Settings, Speaker,
    TranscriptSegment,
};
use uuid::Uuid;

use crate::labels::{
    day_string, destination_display_name, display_title, end_reason_sentence, time,
};
use crate::main_window::detail::{MeetingDetailViewModel, RecordingStatus};
use crate::main_window::list::MeetingListViewModel;
use crate::main_window::progress::ProcessingProgressModel;
use crate::services::RecorderStatus;
use crate::setup::{ExportStatus, SetupBannerMessage, SummaryStatus, copy};
use crate::summary_markdown::detail_sections;

/// How many entries the page's people palette has.
pub const PEOPLE_PALETTE_SIZE: u32 = 8;

/// The palette index of an id: FNV-1a over the 16 bytes, so one person
/// keeps one colour across launches. Swift: `MainWindowSnapshots.colorIndex`.
#[must_use]
pub fn color_index(id: Uuid) -> i64 {
    let mut hash: u32 = 2_166_136_261;
    for byte in id.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    i64::from(hash % PEOPLE_PALETTE_SIZE)
}

#[must_use]
pub fn source(source: MeetingSource) -> BridgeSource {
    match source {
        MeetingSource::MacCall => BridgeSource::Call,
        MeetingSource::MacInPerson => BridgeSource::InPerson,
        MeetingSource::Phone => BridgeSource::Phone,
    }
}

#[must_use]
pub fn state(kind: MeetingStateKind) -> BridgeState {
    match kind {
        MeetingStateKind::Recording => BridgeState::Recording,
        MeetingStateKind::Queued => BridgeState::Queued,
        MeetingStateKind::Processing => BridgeState::Processing,
        MeetingStateKind::Ready => BridgeState::Ready,
        MeetingStateKind::Failed => BridgeState::Failed,
    }
}

/// The controller state the `app` topic reads. Swift: the `AppController`
/// properties `AppSnapshot.init(controller:)` touched.
#[derive(Debug, Clone, Default)]
pub struct AppState {
    pub requested_meeting_id: Option<Uuid>,
    pub requested_settings_section: Option<SettingsSection>,
    pub setup_banner_dismissed: bool,
    pub stored_settings: Option<Settings>,
    /// The iPhone card; the Swift main window never set it (the parity list
    /// notes it), the Rust host fills it from the handover service.
    pub phone: Option<AppPhone>,
}

/// The setup banner shows while at least one meeting exists, the
/// configuration is incomplete and "Not now" was not pressed this launch.
/// Deep links are the controller's pending requests as they stand.
#[must_use]
pub fn app_snapshot(
    app: &AppState,
    has_meetings: bool,
    version: &str,
    platform: Platform,
) -> AppSnapshot {
    let banner = app
        .stored_settings
        .as_ref()
        .and_then(SetupBannerMessage::of)
        .filter(|_| has_meetings && !app.setup_banner_dismissed)
        .map(|message| AppSetupBanner {
            title: message.title().to_owned(),
            body: message.body(platform).to_owned(),
            offers_summaries: message.offers_summaries(),
            offers_vault: message.offers_vault(),
        });
    AppSnapshot {
        version: version.to_owned(),
        setup_banner: banner,
        phone: app.phone.clone(),
        requested_meeting_id: app.requested_meeting_id,
        requested_settings_section: app.requested_settings_section,
    }
}

/// The recorder as the sidebar control renders it: the levels while
/// recording only, the armed auto-stop as seconds, and the messages.
#[must_use]
pub fn recording_snapshot(status: &RecorderStatus) -> RecordingSnapshot {
    let recording = status.state == steno_bridge::RecordingState::Recording;
    RecordingSnapshot {
        state: status.state,
        started_at: status.started_at.filter(|_| recording),
        mode: status.mode,
        call_app: status.call_app.clone(),
        meeting_id: status.meeting_id,
        level: status
            .levels
            .filter(|_| recording)
            .map(|levels| RecordingLevel {
                mic: levels.mic,
                system: levels.system.unwrap_or(0.0),
            }),
        auto_stop: status.auto_stop.as_ref().map(|armed| RecordingAutoStop {
            remaining_seconds: armed.remaining_seconds,
            total_seconds: armed.total_seconds,
            reason: format!(
                "{} closed the microphone.",
                armed.app_name.as_deref().unwrap_or("The call app")
            ),
        }),
        denied_permissions: status.denied_permissions.clone(),
        warning: status.warning.clone(),
        error: status.error.clone(),
    }
}

/// Every queued or processing meeting the model tracks, oldest entry
/// first.
#[must_use]
pub fn progress_snapshot(model: &ProcessingProgressModel) -> ProgressSnapshot {
    let mut entries: Vec<_> = model.entries.values().collect();
    entries.sort_by(|left, right| {
        left.since
            .cmp(&right.since)
            .then_with(|| left.meeting_id.cmp(&right.meeting_id))
    });
    ProgressSnapshot {
        entries: entries
            .into_iter()
            .map(|entry| BridgeProgressEntry {
                meeting_id: entry.meeting_id,
                stage: entry
                    .stage()
                    .map_or_else(|| "waiting".to_owned(), |stage| stage.as_str().to_owned()),
                title: entry.title(),
                fraction: entry.fraction(),
                estimated_remaining_seconds: entry.estimated_remaining_seconds(),
            })
            .collect(),
    }
}

/// The person's first letter, or "?" for a speaker nobody has named yet.
fn chip(speaker: &Speaker, person: Option<&Person>) -> SpeakerChip {
    let initial = person
        .map(|person| person.display_name.trim())
        .and_then(|name| name.chars().next())
        .map_or_else(|| "?".to_owned(), |first| first.to_uppercase().collect());
    SpeakerChip {
        id: speaker.id,
        initial,
        color_index: color_index(person.map_or(speaker.id, |person| person.id)),
        is_confirmed: speaker.assignment.is_confirmed(),
    }
}

fn meeting_row(
    meeting: &Meeting,
    list: &MeetingListViewModel,
    now: DateTime<Utc>,
    zone: FixedOffset,
) -> MeetingRow {
    let bullets: Vec<_> = meeting
        .summary
        .iter()
        .flat_map(|summary| summary.sections.iter())
        .flat_map(|section| section.bullets.iter())
        .collect();
    let preview = match meeting.state.kind() {
        MeetingStateKind::Queued | MeetingStateKind::Processing => None,
        MeetingStateKind::Recording | MeetingStateKind::Ready | MeetingStateKind::Failed => {
            bullets.first().map(|bullet| {
                if bullet.lead.is_empty() {
                    bullet.text.clone()
                } else {
                    format!("{}: {}", bullet.lead, bullet.text)
                }
            })
        }
    };
    let speakers = list
        .speakers_by_meeting
        .get(&meeting.id)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|speaker| {
            chip(
                speaker,
                speaker
                    .person_id()
                    .and_then(|id| list.persons_by_id.get(&id)),
            )
        })
        .collect();
    MeetingRow {
        id: meeting.id,
        title: display_title(meeting, now, zone),
        started_at: meeting.started_at,
        duration_seconds: meeting.duration,
        source: source(meeting.source),
        state: state(meeting.state.kind()),
        failure_reason: meeting.state.failure_reason().map(str::to_owned),
        preview,
        has_summary: !bullets.is_empty(),
        speakers,
        tags: meeting.tags.clone(),
    }
}

/// The list column: the filters as set, the nav counts, the tags with how
/// many meetings carry each, and the day groups.
#[must_use]
pub fn list_snapshot(list: &MeetingListViewModel, now: DateTime<Utc>) -> MeetingsListSnapshot {
    let mut tag_counts: BTreeMap<&str, i64> = BTreeMap::new();
    for tag in list.all.iter().flat_map(|meeting| meeting.tags.iter()) {
        *tag_counts.entry(tag.as_str()).or_default() += 1;
    }
    MeetingsListSnapshot {
        filter: list.state_filter,
        tag_filter: list.tag_filter.clone(),
        query: list.query.clone(),
        counts: ListCounts {
            all: list.count_for(ListFilter::All),
            processing: list.count_for(ListFilter::Processing),
            ready: list.count_for(ListFilter::Ready),
            failed: list.count_for(ListFilter::Failed),
        },
        tags: tag_counts
            .into_iter()
            .map(|(name, count)| ListTag {
                name: name.to_owned(),
                count,
            })
            .collect(),
        groups: list
            .day_groups
            .iter()
            .map(|group| ListDayGroup {
                day: group.day.clone(),
                meetings: group
                    .meetings
                    .iter()
                    .map(|meeting| meeting_row(meeting, list, now, list.zone))
                    .collect(),
            })
            .collect(),
        selection: list.selection,
        error: list.error.clone(),
    }
}

/// Segments grouped into speaker turns: consecutive segments of one speaker
/// become one turn spanning them; a segment without a speaker is always
/// its own turn. Swift: `MainWindowSnapshots.turns`.
#[must_use]
pub fn turns(
    segments: &[TranscriptSegment],
    display_name: impl Fn(Option<Uuid>) -> String,
) -> Vec<DetailTurn> {
    let mut sorted: Vec<&TranscriptSegment> = segments.iter().collect();
    sorted.sort_by(|left, right| left.start.total_cmp(&right.start));
    let mut turns: Vec<DetailTurn> = Vec::new();
    for segment in sorted {
        match (turns.last_mut(), segment.speaker_id) {
            (Some(last), Some(speaker)) if last.speaker_id == Some(speaker) => {
                last.text.push(' ');
                last.text.push_str(&segment.text);
                last.end_seconds = last.end_seconds.max(segment.end);
            }
            _ => turns.push(DetailTurn {
                id: segment.id,
                speaker_id: segment.speaker_id,
                speaker_name: display_name(segment.speaker_id),
                start_seconds: segment.start,
                end_seconds: segment.end,
                text: segment.text.clone(),
            }),
        }
    }
    turns
}

/// One delivery as the footer words it: the destination, then "Exported
/// 10:02", "Pending" or "Failed: reason", and after an export each warning
/// its receipt carries. Swift: `MainWindowSnapshots.deliveryLine`; the
/// warnings are Rust only.
#[must_use]
pub fn delivery_line(delivery: &Delivery, zone: FixedOffset) -> String {
    let status = match &delivery.status {
        DeliveryStatus::Pending => "Pending".to_owned(),
        DeliveryStatus::Delivered => delivery.last_attempt_at.map_or_else(
            || "Exported".to_owned(),
            |at| format!("Exported {}", time(at, zone)),
        ),
        DeliveryStatus::Failed(message) => format!("Failed: {message}"),
    };
    let mut line = format!("{} · {status}", destination_display_name(delivery));
    if let (DeliveryStatus::Delivered, Some(receipt)) = (&delivery.status, &delivery.receipt) {
        for warning in &receipt.warnings {
            line.push_str(" · ");
            line.push_str(warning);
        }
    }
    line
}

/// A failed delivery once the launch stopped retrying it: "Export to
/// Obsidian (Work) keeps failing: reason". Rust only: Swift never retried
/// at launch.
#[must_use]
pub fn keeps_failing_line(delivery: &Delivery, reason: &str) -> String {
    format!(
        "Export to {} keeps failing: {reason}",
        destination_display_name(delivery)
    )
}

fn export_snapshot(detail: &MeetingDetailViewModel, zone: FixedOffset) -> DetailExport {
    match detail.export_status() {
        ExportStatus::NoVault => DetailExport {
            status: DetailExportStatus::NotConfigured,
            message: copy::NOT_EXPORTED_NO_VAULT.to_owned(),
            can_reexport: false,
            can_reveal: false,
        },
        ExportStatus::NotExported => DetailExport {
            status: DetailExportStatus::Pending,
            message: copy::NOT_EXPORTED_YET.to_owned(),
            can_reexport: detail.can_reexport(),
            can_reveal: false,
        },
        ExportStatus::Exported(deliveries) => {
            let any_failed = deliveries
                .iter()
                .any(|delivery| matches!(delivery.status, DeliveryStatus::Failed(_)));
            let any_pending = deliveries
                .iter()
                .any(|delivery| delivery.status == DeliveryStatus::Pending);
            DetailExport {
                status: if any_failed {
                    DetailExportStatus::Failed
                } else if any_pending {
                    DetailExportStatus::Pending
                } else {
                    DetailExportStatus::Delivered
                },
                message: deliveries
                    .iter()
                    .map(|delivery| match delivery.status.failure_message() {
                        Some(reason) if detail.export_keeps_failing => {
                            keeps_failing_line(delivery, reason)
                        }
                        _ => delivery_line(delivery, zone),
                    })
                    .collect::<Vec<_>>()
                    .join("; "),
                can_reexport: detail.can_reexport(),
                can_reveal: deliveries.iter().any(|delivery| delivery.receipt.is_some()),
            }
        }
    }
}

/// The detail pane once its export has loaded; `None` before (the host
/// publishes `null` for the topic then). Every derived value comes from
/// the view model's own accessors.
#[must_use]
pub fn detail_snapshot(
    detail: &MeetingDetailViewModel,
    playing: Option<Uuid>,
    now: DateTime<Utc>,
    zone: FixedOffset,
) -> Option<MeetingDetailSnapshot> {
    let export = detail.export.as_ref()?;
    let meeting = &export.meeting;
    let (retention_kind, deletes_at) = match detail.recording_status() {
        None => (DetailRetentionKind::KeptForever, None),
        Some(RecordingStatus::Deleted) => (DetailRetentionKind::Deleted, None),
        Some(RecordingStatus::DeletesOn(on)) => (DetailRetentionKind::DeletesOn, Some(on)),
        Some(RecordingStatus::KeptUntilExportSucceeds) => {
            (DetailRetentionKind::KeptUntilExportSucceeds, None)
        }
        Some(RecordingStatus::KeptProcessingFailed) => {
            (DetailRetentionKind::KeptProcessingFailed, None)
        }
        Some(RecordingStatus::KeptIncomplete) => (DetailRetentionKind::KeptIncomplete, None),
        Some(RecordingStatus::KeptWhileProcessing) => {
            (DetailRetentionKind::KeptWhileProcessing, None)
        }
    };

    let summary_status = detail.summary_status();
    let skipped = summary_status.skipped_row(steno_bridge::DetailTab::Summary);

    Some(MeetingDetailSnapshot {
        id: meeting.id,
        // The heading derives the title like the list does ("Monday 10:06"
        // for the intake default). Swift: `meeting.displayTitle()`.
        title: display_title(meeting, now, zone),
        started_at: meeting.started_at,
        duration_seconds: meeting.duration,
        language: meeting.language.as_ref().map(|tag| tag.as_str().to_owned()),
        source: source(meeting.source),
        state: state(meeting.state.kind()),
        failure_reason: meeting.state.failure_reason().map(str::to_owned),
        end_reason: meeting.end_reason.as_ref().and_then(end_reason_sentence),
        tags: meeting.tags.clone(),
        tab: detail.tab,
        retention: DetailRetention {
            kind: retention_kind,
            deletes_at,
            keeps_audio: detail.keeps_audio(),
            shows_keep_toggle: detail.shows_keep_toggle(),
            files_exist: detail.recording_files_exist,
        },
        speakers: speaker_rows(detail, playing),
        templates: MeetingDetailViewModel::templates()
            .iter()
            .map(|template| DetailTemplate {
                id: template.id.clone(),
                name: template.display_name.clone(),
            })
            .collect(),
        template_id: meeting.template_id.clone(),
        summary_status: DetailSummaryStatus {
            kind: match summary_status {
                SummaryStatus::Pending => DetailSummaryStatusKind::Pending,
                SummaryStatus::Present => DetailSummaryStatusKind::Present,
                SummaryStatus::SkippedUnconfigured => DetailSummaryStatusKind::SkippedUnconfigured,
                SummaryStatus::SkippedRunnable => DetailSummaryStatusKind::SkippedRunnable,
            },
            title: skipped.map(|row| row.title.to_owned()),
            body: skipped.map(|row| row.body.to_owned()),
            action_title: skipped.map(|row| row.action.title().to_owned()),
        },
        summary: detail_sections(export),
        transcript: turns(&export.segments, |id| detail.display_name(id)),
        tasks: task_rows(export),
        decisions: export
            .decisions
            .iter()
            .map(|decision| decision.text.clone())
            .collect(),
        notes: meeting.scratchpad.clone(),
        export: export_snapshot(detail, zone),
        can_process_again: detail.can_process_again(),
        can_rerun_summary: detail.can_rerun_summary(),
        is_busy: detail.is_busy,
        // The speakers model's error (a clip that would not play, a name
        // that could not be saved) rides on the detail's: the page has one
        // error line. Swift showed it in the popover; the parity list notes it.
        error: detail
            .error
            .clone()
            .or_else(|| detail.speakers.error.clone()),
    })
}

/// One row per speaker: the person's name or the cluster label, the
/// suggestion the picker opens with, the palette index and the playback
/// state.
fn speaker_rows(detail: &MeetingDetailViewModel, playing: Option<Uuid>) -> Vec<DetailSpeaker> {
    let speakers = &detail.speakers;
    speakers
        .rows
        .iter()
        .map(|row| DetailSpeaker {
            id: row.id(),
            cluster_label: row.speaker.cluster_label.clone(),
            display_name: row.display_name().to_owned(),
            assignment: row.speaker.assignment.kind(),
            person_id: row.speaker.person_id(),
            email: row.person.as_ref().and_then(|person| person.email.clone()),
            suggestion_name: speakers.prefill(row.id()),
            color_index: color_index(row.person.as_ref().map_or(row.id(), |person| person.id)),
            has_clip: row.can_play,
            is_playing: playing == Some(row.id()),
        })
        .collect()
}

/// The tasks with their assignee resolved to a person where the export has one.
fn task_rows(export: &steno_core::MeetingExport) -> Vec<DetailTask> {
    export
        .tasks
        .iter()
        .map(|task| {
            let assignee = task.assignee_person_id.and_then(|id| export.person(id));
            DetailTask {
                id: task.id,
                text: task.text.clone(),
                assignee_name: assignee
                    .map(|person| person.display_name.clone())
                    .or_else(|| task.assignee_name.clone()),
                assignee_color_index: assignee.map(|person| color_index(person.id)),
                due_date: task.due_date,
                priority: task.priority,
                done: task.done,
            }
        })
        .collect()
}

/// `day_string` of a meeting in the list's zone, for tests that pin the
/// grouping.
#[must_use]
pub fn day_of(meeting: &Meeting, zone: FixedOffset) -> String {
    day_string(meeting.started_at, zone)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_are_stable_fnv_folds() {
        let anna = Uuid::parse_str("00000000-0000-0000-0000-00000000000C").unwrap();
        assert_eq!(color_index(anna), 1);
        assert_eq!(color_index(anna), color_index(anna));
        assert!((0..8).contains(&color_index(Uuid::new_v4())));
    }
}

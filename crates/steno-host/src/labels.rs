//! User-facing words for the core's enums and values, after
//! `apps/macos/Steno/Design/Labels.swift`. Raw values are storage and wire
//! spellings and never reach a label.
//!
//! Dates: Swift formatted through `Date.FormatStyle` in the viewer's locale
//! and time zone; the host has no locale table, so the English wordings
//! and the 24-hour clock stand in, in the zone the [`Host`](crate::Host)
//! was given. The parity list notes the gap.

use chrono::{DateTime, Datelike, FixedOffset, Utc};
use steno_core::{
    AudioRetention, Delivery, Meeting, MeetingSource, MeetingStateKind, PipelineStage,
    RecordingEndReason,
};

/// Swift: `MeetingSource.label`.
#[must_use]
pub fn source_label(source: MeetingSource) -> &'static str {
    match source {
        MeetingSource::MacCall => "Call",
        MeetingSource::MacInPerson => "In person",
        MeetingSource::Phone => "Phone",
    }
}

/// What the queue row says while the stage runs. Swift: `PipelineStage.label`.
#[must_use]
pub fn stage_label(stage: PipelineStage) -> &'static str {
    match stage {
        PipelineStage::Decode => "Decoding",
        PipelineStage::Transcribe => "Transcribing",
        PipelineStage::Diarize => "Finding speakers",
        PipelineStage::MatchSpeakers => "Matching speakers",
        PipelineStage::Merge => "Merging",
        PipelineStage::Cleanup => "Cleaning up",
        PipelineStage::Summarize => "Summarising",
        PipelineStage::Persist => "Saving",
        PipelineStage::Deliver => "Delivering",
        PipelineStage::Retention => "Applying retention",
    }
}

/// The state word. Swift: `MeetingState.label`.
#[must_use]
pub fn state_label(state: MeetingStateKind) -> &'static str {
    match state {
        MeetingStateKind::Recording => "Recording",
        MeetingStateKind::Queued => "Queued",
        MeetingStateKind::Processing => "Processing",
        MeetingStateKind::Ready => "Ready",
        MeetingStateKind::Failed => "Failed",
    }
}

/// "10:06". Swift: `DisplayFormat.time`.
#[must_use]
pub fn time(date: DateTime<Utc>, zone: FixedOffset) -> String {
    date.with_timezone(&zone).format("%H:%M").to_string()
}

/// "Monday". Swift: `DisplayFormat.weekday`.
#[must_use]
pub fn weekday(date: DateTime<Utc>, zone: FixedOffset) -> String {
    date.with_timezone(&zone).format("%A").to_string()
}

/// "Sep 28". Swift: `DisplayFormat.monthDay`.
#[must_use]
pub fn month_day(date: DateTime<Utc>, zone: FixedOffset) -> String {
    let local = date.with_timezone(&zone);
    format!("{} {}", local.format("%b"), local.day())
}

/// `YYYY-MM-DD` of `date` in `zone`; the page formats the label.
/// Swift: `MainWindowSnapshots.dayString`.
#[must_use]
pub fn day_string(date: DateTime<Utc>, zone: FixedOffset) -> String {
    date.with_timezone(&zone).format("%Y-%m-%d").to_string()
}

/// The calendar day `date` falls on in `zone`, as a day number: what
/// groups the list and measures a title's age. Swift: `calendar.startOfDay`.
#[must_use]
pub fn day_number(date: DateTime<Utc>, zone: FixedOffset) -> i32 {
    date.with_timezone(&zone).date_naive().num_days_from_ce()
}

/// The last six days read as a weekday in the derived title; older days as
/// month and day. Swift: `Meeting.weekdayTitleDays`.
pub const WEEKDAY_TITLE_DAYS: i32 = 6;

/// What the list entry and the detail heading call the meeting: the stored
/// title unless it is the intake's default, in which case "Monday 10:06"
/// for the last six days and "Sep 28 10:06" before that, in `zone`. The
/// stored value is never touched. Swift: `Meeting.displayTitle`.
#[must_use]
pub fn display_title(meeting: &Meeting, now: DateTime<Utc>, zone: FixedOffset) -> String {
    if !meeting.title_origin.is_default() {
        return meeting.title.clone();
    }
    let days = day_number(now, zone) - day_number(meeting.started_at, zone);
    let day = if (0..=WEEKDAY_TITLE_DAYS).contains(&days) {
        weekday(meeting.started_at, zone)
    } else {
        month_day(meeting.started_at, zone)
    };
    format!("{day} {}", time(meeting.started_at, zone))
}

/// The meeting header's end-reason row; `None` for a manual stop, where
/// the user was there. Swift: `RecordingEndReason.sentence`.
#[must_use]
pub fn end_reason_sentence(reason: &RecordingEndReason) -> Option<String> {
    match reason {
        RecordingEndReason::Manual => None,
        RecordingEndReason::CallEnded { app_name } => Some(format!(
            "Ended automatically when {} closed the microphone.",
            app_name.as_deref().unwrap_or("the call app")
        )),
        RecordingEndReason::DeviceLost => Some(
            "Ended because an audio device disappeared. The recording up to that point was kept."
                .to_owned(),
        ),
        RecordingEndReason::Quit => Some("Ended when Steno quit.".to_owned()),
        RecordingEndReason::Failed => Some(
            "Ended because the recording failed. The recording up to that point was kept."
                .to_owned(),
        ),
    }
}

/// What the rule does to the files, as Settings > Audio says it under the
/// picker. Swift: `AudioRetention.footnote`.
#[must_use]
pub fn retention_footnote(retention: AudioRetention) -> String {
    match retention {
        AudioRetention::KeepForever => {
            "Recordings stay in the folder above until you delete a meeting.".to_owned()
        }
        AudioRetention::KeepDays(days) => format!(
            "Each recording is deleted {days} {} after it was processed and exported. \
             Transcripts, summaries and exports are never deleted by this rule.",
            if days == 1 { "day" } else { "days" }
        ),
        AudioRetention::DeleteAfterProcessing => {
            "Each recording is deleted as soon as it was transcribed, summarised and exported. \
             Transcripts, summaries and exports stay."
                .to_owned()
        }
    }
}

/// The Obsidian destination's stored id. Swift:
/// `ObsidianFolderDestination.destinationID`.
pub const OBSIDIAN_DESTINATION_ID: &str = "obsidian-folder";

/// The destination as the footer names it; a destination this app does not
/// know is shown by its id. Swift: `Delivery.destinationDisplayName`.
#[must_use]
pub fn destination_display_name(delivery: &Delivery) -> String {
    if delivery.destination_id == OBSIDIAN_DESTINATION_ID {
        "Obsidian".to_owned()
    } else {
        delivery.destination_id.clone()
    }
}

/// "4.2 GB", "485 MB", "22 MB": Foundation's `ByteCountFormatter` in its
/// file style, decimal units, one decimal for megabytes and two for
/// gigabytes, trailing zeros dropped. Swift: `ByteCountFormatter.fileSize`.
#[must_use]
pub fn file_size(bytes: i64) -> String {
    const UNITS: [(&str, f64, usize); 4] = [
        ("KB", 1e3, 0),
        ("MB", 1e6, 1),
        ("GB", 1e9, 2),
        ("TB", 1e12, 2),
    ];
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    // Exact well past any file size the formatter will see.
    #[allow(clippy::cast_precision_loss)]
    let value = bytes as f64;
    let (unit, scale, decimals) = UNITS
        .iter()
        .rev()
        .find(|(_, scale, _)| value >= *scale)
        .copied()
        .unwrap_or(UNITS[0]);
    let mut text = format!("{:.*}", decimals, value / scale);
    if text.contains('.') {
        text = text.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    format!("{text} {unit}")
}

/// `zone` as the offset of UTC, the default a host without a zone runs in.
#[must_use]
pub fn utc() -> FixedOffset {
    FixedOffset::east_opt(0).expect("zero is a valid offset")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_sizes_follow_the_file_style() {
        assert_eq!(file_size(512), "512 bytes");
        assert_eq!(file_size(485_000_000), "485 MB");
        assert_eq!(file_size(22_000_000), "22 MB");
        assert_eq!(file_size(1_640_000_000), "1.64 GB");
        assert_eq!(file_size(1_500_000), "1.5 MB");
        assert_eq!(file_size(734_003_200), "734 MB");
        assert_eq!(file_size(4_200), "4 KB");
    }

    #[test]
    fn retention_footnotes_count_days() {
        assert!(retention_footnote(AudioRetention::KeepDays(1)).contains("1 day after"));
        assert!(retention_footnote(AudioRetention::KeepDays(30)).contains("30 days after"));
        assert!(
            retention_footnote(AudioRetention::DeleteAfterProcessing).starts_with("Each recording")
        );
    }
}

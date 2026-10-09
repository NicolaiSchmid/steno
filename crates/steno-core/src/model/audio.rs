//! Audio assets: their format, lanes and retention.
//! Swift: `Sources/StenoCore/Model/Audio.swift`.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use super::{AudioLane, Meeting, Speaker, TranscriptSegment, room_speaker_id};
use crate::json::{
    self,
    case_coding::{self, Case},
};
use crate::string_enum;

string_enum! {
    pub enum AudioFormat {
        /// The Mac master recording: CAF, 48 kHz, Float32, one channel per lane.
        Caf48kFloat32 = "caf48kFloat32",
        /// Phone recordings and the optional export mixdown.
        M4aAac = "m4aAAC",
        /// Fixtures, sample clips and `steno process` input: 16 kHz mono Int16.
        Wav16kInt16 = "wav16kInt16",
    }
}

impl AudioFormat {
    /// The file extension a file in this format gets.
    #[must_use]
    pub const fn file_extension(self) -> &'static str {
        match self {
            AudioFormat::Caf48kFloat32 => "caf",
            AudioFormat::M4aAac => "m4a",
            AudioFormat::Wav16kInt16 => "wav",
        }
    }
}

string_enum! {
    /// The case names of [`AudioRetention`], shared by `meeting.json` and
    /// the `audioAsset.retention` column.
    pub enum AudioRetentionKind {
        DeleteAfterProcessing = "deleteAfterProcessing",
        KeepDays = "keepDays",
        KeepForever = "keepForever",
    }
}

/// How long the audio files of a meeting stay on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioRetention {
    /// The retention stage sets `expiresAt = now`.
    DeleteAfterProcessing,
    KeepDays(i64),
    /// The default for a new install and the per-meeting keep toggle.
    KeepForever,
}

impl AudioRetention {
    #[must_use]
    pub fn kind(self) -> AudioRetentionKind {
        match self {
            AudioRetention::DeleteAfterProcessing => AudioRetentionKind::DeleteAfterProcessing,
            AudioRetention::KeepDays(_) => AudioRetentionKind::KeepDays,
            AudioRetention::KeepForever => AudioRetentionKind::KeepForever,
        }
    }

    /// The `keepDays` payload, the `retentionDays` column.
    #[must_use]
    pub fn days(self) -> Option<i64> {
        match self {
            AudioRetention::KeepDays(days) => Some(days),
            _ => None,
        }
    }

    /// The retention from its two columns.
    #[must_use]
    pub fn from_columns(kind: AudioRetentionKind, days: Option<i64>) -> Self {
        match kind {
            AudioRetentionKind::DeleteAfterProcessing => AudioRetention::DeleteAfterProcessing,
            AudioRetentionKind::KeepDays => AudioRetention::KeepDays(days.unwrap_or(0)),
            AudioRetentionKind::KeepForever => AudioRetention::KeepForever,
        }
    }

    /// When the files expire counted from `now`; `None` for `KeepForever`.
    #[must_use]
    pub fn expiry(self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            AudioRetention::DeleteAfterProcessing => Some(now),
            AudioRetention::KeepDays(days) => Some(now + Duration::days(days)),
            AudioRetention::KeepForever => None,
        }
    }
}

impl Serialize for AudioRetention {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            AudioRetention::KeepDays(days) => {
                case_coding::serialize_payload(self.kind().as_str(), days, serializer)
            }
            _ => case_coding::serialize_bare(self.kind().as_str(), serializer),
        }
    }
}

impl<'de> Deserialize<'de> for AudioRetention {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (name, payload) = Case::deserialize(deserializer)?.split()?;
        let kind: AudioRetentionKind = name.parse().map_err(serde::de::Error::custom)?;
        Ok(match kind {
            AudioRetentionKind::KeepDays => {
                AudioRetention::KeepDays(case_coding::payload(&name, payload)?)
            }
            other => AudioRetention::from_columns(other, None),
        })
    }
}

/// The recording files of one meeting. `url` is the master, `sidecars_16k`
/// the per-lane 16 kHz decodes, `mixdown_url` the AAC mono export. URLs are
/// `file://` strings, as Swift stores them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioAsset {
    #[serde(with = "json::uuid_text")]
    pub id: Uuid,
    #[serde(rename = "meetingID", with = "json::uuid_text")]
    pub meeting_id: Uuid,
    pub url: String,
    pub format: AudioFormat,
    pub lanes: Vec<AudioLane>,
    #[serde(rename = "sidecars16k", default)]
    pub sidecars_16k: BTreeMap<AudioLane, String>,
    #[serde(
        rename = "mixdownURL",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub mixdown_url: Option<String>,
    pub retention: AudioRetention,
    #[serde(
        default,
        with = "json::iso_time_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub expires_at: Option<DateTime<Utc>>,
}

impl AudioAsset {
    /// Master, sidecars (by lane name) and mixdown: the asset's own files.
    #[must_use]
    pub fn expirable_files(&self) -> Vec<String> {
        let mut files = vec![self.url.clone()];
        let mut sidecars: Vec<(&AudioLane, &String)> = self.sidecars_16k.iter().collect();
        sidecars.sort_by_key(|(lane, _)| lane.as_str());
        files.extend(sidecars.into_iter().map(|(_, url)| url.clone()));
        files.extend(self.mixdown_url.iter().cloned());
        files
    }
}

/// The longest recording in which a lane can come out with no segment at
/// all and still be taken as silent; past it the lane most likely failed to
/// transcribe ([`results_need_the_audio`]). Rust only.
pub const EMPTY_LANE_MAXIMUM_SECONDS: f64 = 30.0;

/// Whether a ready meeting's results could still need its recording, so
/// the automatic retention keeps it unstamped: the diarizer failed and the
/// room fell back to one unknown speaker ([`room_speaker_id`]), or a lane
/// the asset recorded (a call records the system audio and the mic, an
/// in-person or phone meeting one lane) has no segment while the meeting
/// runs longer than [`EMPTY_LANE_MAXIMUM_SECONDS`] or has no positive
/// duration. It errs towards keeping: a call whose mic stayed muted is
/// kept too. The pipeline asks it before the automatic stamp, over the
/// rows it reads from the store, and the meeting detail's recording line
/// asks it over the export, so both read the same rule. A rule the user
/// applies is stamped as chosen. Rust only: Swift stamps once every
/// delivery succeeded.
#[must_use]
pub fn results_need_the_audio(
    meeting: &Meeting,
    speakers: &[Speaker],
    segments: &[TranscriptSegment],
    asset: &AudioAsset,
) -> bool {
    let room = room_speaker_id(meeting.id);
    if speakers.iter().any(|speaker| speaker.id == room) {
        return true;
    }
    let positive = meeting.duration > 0.0;
    let long = meeting.duration > EMPTY_LANE_MAXIMUM_SECONDS;
    (long || !positive)
        && asset
            .lanes
            .iter()
            .any(|lane| !segments.iter().any(|segment| segment.lane == *lane))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::to_column_string;

    #[test]
    fn retention_uses_the_case_coding_shape() {
        assert_eq!(
            to_column_string(&AudioRetention::KeepForever).unwrap(),
            r#""keepForever""#
        );
        assert_eq!(
            to_column_string(&AudioRetention::KeepDays(30)).unwrap(),
            r#"{"keepDays":30}"#
        );
        let parsed: AudioRetention = serde_json::from_str(r#"{"keepDays":30}"#).unwrap();
        assert_eq!(parsed, AudioRetention::KeepDays(30));
    }

    fn call_asset(meeting_id: Uuid) -> AudioAsset {
        AudioAsset {
            id: Uuid::nil(),
            meeting_id,
            url: "file:///master.caf".to_owned(),
            format: AudioFormat::Caf48kFloat32,
            lanes: vec![AudioLane::Mic, AudioLane::System],
            sidecars_16k: BTreeMap::new(),
            mixdown_url: None,
            retention: AudioRetention::DeleteAfterProcessing,
            expires_at: None,
        }
    }

    fn segment(meeting_id: Uuid, lane: AudioLane) -> TranscriptSegment {
        TranscriptSegment {
            id: Uuid::new_v4(),
            meeting_id,
            start: 0.0,
            end: 1.0,
            speaker_id: None,
            lane,
            text: "Hello.".to_owned(),
            raw_text: "Hello.".to_owned(),
        }
    }

    fn speaker(meeting_id: Uuid, id: Uuid) -> Speaker {
        Speaker {
            id,
            meeting_id,
            cluster_label: "Speaker 1".to_owned(),
            assignment: crate::SpeakerAssignment::Unknown,
            embedding: None,
            sample_clip_range: None,
            sample_clip_url: None,
            cluster_confidence: 0.0,
        }
    }

    /// The room speaker the diarizer fallback leaves keeps the recording,
    /// however short the meeting and whatever its lanes hold.
    #[test]
    fn the_room_speaker_needs_the_audio() {
        let mut meeting = crate::testing::sample_data::meeting();
        meeting.duration = 6.0;
        let asset = call_asset(meeting.id);
        let both = [
            segment(meeting.id, AudioLane::Mic),
            segment(meeting.id, AudioLane::System),
        ];
        let room = [speaker(meeting.id, room_speaker_id(meeting.id))];
        let diarized = [speaker(meeting.id, Uuid::new_v4())];
        assert!(results_need_the_audio(&meeting, &room, &both, &asset));
        assert!(!results_need_the_audio(&meeting, &diarized, &both, &asset));
    }

    /// A recorded lane with no segment keeps the recording past the bound,
    /// and at any length when the meeting has no positive duration.
    #[test]
    fn an_empty_lane_needs_the_audio_past_the_bound_or_without_a_duration() {
        let mut meeting = crate::testing::sample_data::meeting();
        let asset = call_asset(meeting.id);
        let mic_only = [segment(meeting.id, AudioLane::Mic)];
        let both = [
            segment(meeting.id, AudioLane::Mic),
            segment(meeting.id, AudioLane::System),
        ];
        for (duration, kept) in [
            (EMPTY_LANE_MAXIMUM_SECONDS, false),
            (EMPTY_LANE_MAXIMUM_SECONDS + 0.5, true),
            (0.0, true),
            (-1.0, true),
        ] {
            meeting.duration = duration;
            assert_eq!(
                results_need_the_audio(&meeting, &[], &mic_only, &asset),
                kept,
                "{duration} s"
            );
            assert!(
                !results_need_the_audio(&meeting, &[], &both, &asset),
                "{duration} s with every lane transcribed"
            );
        }
        // A one-lane recording: the lane the asset did not record is not
        // missing.
        let mut phone = call_asset(meeting.id);
        phone.lanes = vec![AudioLane::Mixed];
        meeting.duration = 0.0;
        assert!(results_need_the_audio(&meeting, &[], &[], &phone));
        assert!(!results_need_the_audio(
            &meeting,
            &[],
            &[segment(meeting.id, AudioLane::Mixed)],
            &phone
        ));
    }

    #[test]
    fn sidecars_encode_as_an_object_keyed_by_lane() {
        let sidecars: BTreeMap<AudioLane, String> = [
            (AudioLane::System, "file:///b".to_owned()),
            (AudioLane::Mic, "file:///a".to_owned()),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            to_column_string(&sidecars).unwrap(),
            r#"{"mic":"file:///a","system":"file:///b"}"#
        );
    }
}

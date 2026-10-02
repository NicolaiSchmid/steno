//! Pure lane merge: one ordered transcript from per-lane raw segments.
//! Swift: `Sources/StenoCore/Pipeline/LaneMerger.swift`.

use std::collections::BTreeMap;

use steno_core::{
    AudioLane, RawSegment, Speaker, SpeakerAssignment, TimeRange, TranscriptSegment, derived_uuid,
};
use uuid::Uuid;

/// A diarization cluster and the `Speaker` row it became.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterSpeaker {
    pub speaker_id: Uuid,
    pub ranges: Vec<TimeRange>,
}

/// `.mic` segments belong to the deterministic "me" speaker; `.system` and
/// `.mixed` segments get the diarization cluster covering their midpoint,
/// and a `.mixed` segment is never assigned to "me".
pub struct LaneMerger;

impl LaneMerger {
    pub const ME_SPEAKER_LABEL: &'static str = "Me";

    /// The "me" speaker's id for a meeting, stable across re-runs.
    #[must_use]
    pub fn me_speaker_id(meeting_id: Uuid) -> Uuid {
        derived_uuid(meeting_id, "speaker-me")
    }

    /// The "me" participant's id for a meeting.
    #[must_use]
    pub fn me_participant_id(meeting_id: Uuid) -> Uuid {
        derived_uuid(meeting_id, "participant-me")
    }

    /// The `Speaker` row behind the mic lane: confirmed when the "me"
    /// participant is a known person, otherwise unknown with the label `Me`.
    #[must_use]
    pub fn me_speaker(meeting_id: Uuid, person_id: Option<Uuid>) -> Speaker {
        Speaker {
            id: Self::me_speaker_id(meeting_id),
            meeting_id,
            cluster_label: Self::ME_SPEAKER_LABEL.to_owned(),
            assignment: person_id.map_or(SpeakerAssignment::Unknown, |person_id| {
                SpeakerAssignment::Confirmed { person_id }
            }),
            embedding: None,
            sample_clip_range: None,
            sample_clip_url: None,
            cluster_confidence: 1.0,
        }
    }

    /// Deterministic segment id from meeting, lane and index.
    #[must_use]
    pub fn segment_id(meeting_id: Uuid, lane: AudioLane, index: usize) -> Uuid {
        derived_uuid(meeting_id, &format!("segment-{}-{index}", lane.as_str()))
    }

    /// The merged transcript, ordered by start, then lane order, then index.
    #[must_use]
    pub fn merge(
        meeting_id: Uuid,
        lanes: &BTreeMap<AudioLane, Vec<RawSegment>>,
        clusters: &[ClusterSpeaker],
        me_speaker_id: Option<Uuid>,
    ) -> Vec<TranscriptSegment> {
        let mut merged: Vec<((f64, usize, usize), TranscriptSegment)> = Vec::new();
        for (lane_index, lane) in AudioLane::ALL.iter().enumerate() {
            let Some(raw) = lanes.get(lane) else { continue };
            for (index, segment) in raw.iter().enumerate() {
                let speaker_id = match lane {
                    AudioLane::Mic => me_speaker_id,
                    AudioLane::System | AudioLane::Mixed => Self::cluster_covering(segment, clusters),
                };
                let transcript = TranscriptSegment {
                    id: Self::segment_id(meeting_id, *lane, index),
                    meeting_id,
                    start: segment.start,
                    end: segment.end,
                    speaker_id,
                    lane: *lane,
                    text: segment.text.clone(),
                    raw_text: segment.text.clone(),
                };
                merged.push(((segment.start, lane_index, index), transcript));
            }
        }
        merged.sort_by(|a, b| {
            a.0.0
                .partial_cmp(&b.0.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.1.cmp(&b.0.1))
                .then(a.0.2.cmp(&b.0.2))
        });
        merged.into_iter().map(|(_, segment)| segment).collect()
    }

    /// The cluster whose ranges contain the segment's midpoint; when none
    /// does, the cluster overlapping it the most; `None` when none overlaps.
    #[must_use]
    pub fn cluster_covering(segment: &RawSegment, clusters: &[ClusterSpeaker]) -> Option<Uuid> {
        let midpoint = f64::midpoint(segment.start, segment.end);
        if let Some(exact) = clusters.iter().find(|cluster| {
            cluster
                .ranges
                .iter()
                .any(|range| range.lower <= midpoint && midpoint <= range.upper)
        }) {
            return Some(exact.speaker_id);
        }
        let mut best: Option<(Uuid, f64)> = None;
        for cluster in clusters {
            let overlap: f64 = cluster
                .ranges
                .iter()
                .map(|range| {
                    (range.upper.min(segment.end) - range.lower.max(segment.start)).max(0.0)
                })
                .sum();
            if overlap > 0.0 && overlap > best.map_or(0.0, |(_, o)| o) {
                best = Some((cluster.speaker_id, overlap));
            }
        }
        best.map(|(id, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(start: f64, end: f64, text: &str) -> RawSegment {
        RawSegment {
            start,
            end,
            text: text.to_owned(),
            language: None,
            word_timings: None,
        }
    }

    #[test]
    fn mic_goes_to_me_and_system_to_the_covering_cluster() {
        let meeting = Uuid::new_v4();
        let me = LaneMerger::me_speaker_id(meeting);
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let clusters = vec![
            ClusterSpeaker {
                speaker_id: a,
                ranges: vec![TimeRange { lower: 0.0, upper: 1.0 }],
            },
            ClusterSpeaker {
                speaker_id: b,
                ranges: vec![TimeRange { lower: 1.0, upper: 2.0 }],
            },
        ];
        let mut lanes = BTreeMap::new();
        lanes.insert(AudioLane::Mic, vec![raw(0.5, 1.5, "mic")]);
        lanes.insert(AudioLane::System, vec![raw(0.0, 0.8, "sys a"), raw(1.2, 1.9, "sys b")]);
        let merged = LaneMerger::merge(meeting, &lanes, &clusters, Some(me));
        assert_eq!(
            merged.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            ["sys a", "mic", "sys b"]
        );
        assert_eq!(merged[0].speaker_id, Some(a));
        assert_eq!(merged[1].speaker_id, Some(me));
        assert_eq!(merged[2].speaker_id, Some(b));
        assert_eq!(merged[1].id, LaneMerger::segment_id(meeting, AudioLane::Mic, 0));
    }

    #[test]
    fn a_segment_outside_every_range_takes_the_largest_overlap() {
        let a = Uuid::new_v4();
        let clusters = vec![ClusterSpeaker {
            speaker_id: a,
            ranges: vec![TimeRange { lower: 0.0, upper: 0.4 }],
        }];
        assert_eq!(LaneMerger::cluster_covering(&raw(0.0, 2.0, ""), &clusters), Some(a));
        assert_eq!(LaneMerger::cluster_covering(&raw(3.0, 4.0, ""), &clusters), None);
    }
}

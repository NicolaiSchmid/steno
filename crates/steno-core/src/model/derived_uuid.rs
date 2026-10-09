//! UUIDs derived from another and a salt.
//! Swift: `Sources/StenoCore/Model/UUID+Derived.swift`.

use uuid::Uuid;

use crate::json::uuid_string;

/// A UUID derived from `base` and `salt`, stable across runs and machines,
/// bit for bit `UUID(derivedFrom:salt:)` in Swift: FNV-1a over the base's
/// uppercase string and the salt, a splitmix step for the second half, and a
/// version-4, variant-1 layout.
#[must_use]
pub fn derived_uuid(base: Uuid, salt: &str) -> Uuid {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in uuid_string(base).bytes().chain(salt.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut second = hash ^ 0x9e37_79b9_7f4a_7c15;
    second = second.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    second ^= second >> 31;

    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&hash.to_be_bytes());
    bytes[8..].copy_from_slice(&second.to_be_bytes());
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    Uuid::from_bytes(bytes)
}

/// The id of the one unknown room speaker a meeting falls back to when its
/// diarizer fails (the pipeline's `Diarization::one_room_speaker`): its own
/// id, so a later run that diarizes never inherits its confirmation, and
/// the sign [`results_need_the_audio`](crate::results_need_the_audio)
/// reads. Rust only: Swift fails the meeting.
#[must_use]
pub fn room_speaker_id(meeting_id: Uuid) -> Uuid {
    derived_uuid(meeting_id, "speaker-room")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A speaker row the Swift pipeline wrote: `speaker-<clusterLabel>`
    /// derived from its meeting id.
    #[test]
    fn matches_a_speaker_id_the_swift_pipeline_derived() {
        let meeting = Uuid::parse_str("516EADE8-40E5-4434-8AAF-9214A21A604E").unwrap();
        assert_eq!(
            uuid_string(derived_uuid(meeting, "speaker-Speaker 1")),
            "6091570E-A744-46C5-BD1E-ECCB841949C5"
        );
        assert_eq!(
            uuid_string(derived_uuid(meeting, "speaker-Speaker 2")),
            "6091540E-A744-41AC-B021-105574073719"
        );
    }
}

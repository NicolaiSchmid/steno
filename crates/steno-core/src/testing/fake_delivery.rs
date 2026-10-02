//! A destination and a handover intake over a temporary folder.
//! Swift: `Sources/StenoCore/Testing/FakeDelivery.swift` (the dispatcher
//! fake follows with the store's export in WP6).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use super::{CallLog, FakeFailure};
use crate::json::{to_column_string, uuid_string};
use crate::{
    BoundaryResult, DeliveredFile, DeliveryReceipt, Destination, FileOwnership, HandoverIntake,
    MeetingExport, PairedDevice, RecordingMetadata,
};

/// What a delivery within `fail_until` returns.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("transient failure on attempt {attempt}")]
pub struct Transient {
    pub attempt: usize,
}

/// A `Destination` that writes `meeting.json` under `<root>/<meeting id>/`
/// and records every export it received. The file holds the compact
/// `StenoJSON` form; the adapters package owns the pretty one.
#[derive(Debug)]
pub struct FakeDestination {
    pub id: String,
    pub root: PathBuf,
    pub validate_failure: Option<String>,
    pub deliver_failure: Option<String>,
    /// The first `fail_until` deliveries fail with [`Transient`]; the rest
    /// succeed.
    pub fail_until: usize,
    pub deliveries: CallLog<MeetingExport>,
}

impl FakeDestination {
    pub const RENDERER_VERSION: i64 = 1;

    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        FakeDestination {
            id: "fake".to_owned(),
            root: root.into(),
            validate_failure: None,
            deliver_failure: None,
            fail_until: 0,
            deliveries: CallLog::new(),
        }
    }

    /// The `meeting.json` this destination wrote for `meeting_id`.
    #[must_use]
    pub fn export_path(&self, meeting_id: Uuid) -> PathBuf {
        self.root.join(uuid_string(meeting_id)).join("meeting.json")
    }
}

#[async_trait]
impl Destination for FakeDestination {
    fn id(&self) -> &str {
        &self.id
    }

    async fn validate(&self) -> BoundaryResult<()> {
        FakeFailure::check(self.validate_failure.as_ref())
    }

    async fn deliver(
        &self,
        meeting: &MeetingExport,
        previous: Option<&DeliveryReceipt>,
    ) -> BoundaryResult<DeliveryReceipt> {
        self.deliveries.record(meeting.clone());
        FakeFailure::check(self.deliver_failure.as_ref())?;
        let attempt = self.deliveries.count();
        if attempt <= self.fail_until {
            return Err(Box::new(Transient { attempt }));
        }
        let folder = previous.map_or_else(
            || uuid_string(meeting.meeting.id),
            |receipt| receipt.folder.clone(),
        );
        let directory = self.root.join(&folder);
        std::fs::create_dir_all(&directory)?;
        let data = to_column_string(meeting)?;
        std::fs::write(directory.join("meeting.json"), &data)?;
        Ok(DeliveryReceipt {
            root: self.root.to_string_lossy().into_owned(),
            folder,
            files: vec![DeliveredFile {
                relative_path: "meeting.json".to_owned(),
                ownership: FileOwnership::Owned,
                sha256: Sha256::digest(data.as_bytes()).to_vec(),
            }],
            renderer_version: Self::RENDERER_VERSION,
        })
    }
}

/// One `admit` call as the fake saw it.
#[derive(Debug, Clone, PartialEq)]
pub struct Admission {
    pub file: PathBuf,
    pub metadata: RecordingMetadata,
    pub device: PairedDevice,
}

/// A `HandoverIntake` that records every admission and returns a fixed or
/// fresh meeting id.
#[derive(Debug, Default)]
pub struct FakeHandoverIntake {
    pub admissions: CallLog<Admission>,
    pub meeting_id: Option<Uuid>,
    pub failure: Option<String>,
}

#[async_trait]
impl HandoverIntake for FakeHandoverIntake {
    async fn admit(
        &self,
        file: &Path,
        metadata: &RecordingMetadata,
        device: &PairedDevice,
    ) -> BoundaryResult<Uuid> {
        self.admissions.record(Admission {
            file: file.to_path_buf(),
            metadata: metadata.clone(),
            device: device.clone(),
        });
        FakeFailure::check(self.failure.as_ref())?;
        Ok(self.meeting_id.unwrap_or_else(Uuid::new_v4))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AudioFormat;
    use crate::testing::sample_data;

    #[tokio::test]
    async fn the_destination_writes_meeting_json_and_reuses_the_folder() {
        let root = tempfile::tempdir().unwrap();
        let destination = FakeDestination {
            fail_until: 1,
            ..FakeDestination::new(root.path())
        };
        destination.validate().await.unwrap();
        let export = sample_data::export();
        let error = destination.deliver(&export, None).await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<Transient>(),
            Some(&Transient { attempt: 1 })
        );
        let receipt = destination.deliver(&export, None).await.unwrap();
        assert_eq!(
            receipt.folder,
            sample_data::meeting_id().to_string().to_uppercase()
        );
        let path = destination.export_path(export.meeting.id);
        let written = std::fs::read(&path).unwrap();
        assert_eq!(receipt.files[0].sha256, Sha256::digest(&written).to_vec());
        let parsed: MeetingExport = serde_json::from_slice(&written).unwrap();
        assert_eq!(parsed, export);
        let previous = DeliveryReceipt {
            folder: "elsewhere".to_owned(),
            ..receipt
        };
        let again = destination.deliver(&export, Some(&previous)).await.unwrap();
        assert_eq!(again.folder, "elsewhere");
        assert!(root.path().join("elsewhere/meeting.json").exists());
        assert_eq!(destination.deliveries.count(), 3);
    }

    #[tokio::test]
    async fn the_intake_records_admissions() {
        let fixed = Uuid::new_v4();
        let intake = FakeHandoverIntake {
            meeting_id: Some(fixed),
            ..Default::default()
        };
        let metadata = RecordingMetadata {
            recording_id: Uuid::new_v4(),
            started_at: sample_data::started_at(),
            duration_seconds: 12.0,
            byte_count: 1024,
            sha256: vec![0; 32],
            chunk_size: 512,
            format: AudioFormat::M4aAac,
            device_name: "Phone".to_owned(),
        };
        let device = PairedDevice {
            id: Uuid::new_v4(),
            name: "Phone".to_owned(),
            paired_at: sample_data::started_at(),
            last_seen_at: None,
        };
        let id = intake
            .admit(Path::new("/tmp/recording.m4a"), &metadata, &device)
            .await
            .unwrap();
        assert_eq!(id, fixed);
        assert_eq!(intake.admissions.entries()[0].metadata, metadata);
        // Codable in Swift: the metadata round-trips through JSON.
        let text = to_column_string(&metadata).unwrap();
        assert!(text.starts_with(r#"{"byteCount":1024,"chunkSize":512,"deviceName":"Phone","durationSeconds":12,"format":"m4aAAC","#));
        assert_eq!(
            serde_json::from_str::<RecordingMetadata>(&text).unwrap(),
            metadata
        );
    }
}

//! A destination and a handover intake over a temporary folder.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
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

/// A `Destination` that writes `meeting.json` in the column form under
/// `<root>/<meeting id>/` and records every export it received.
#[derive(Debug)]
pub struct FakeDestination {
    pub id: String,
    pub root: PathBuf,
    pub validate_failure: Option<String>,
    pub deliver_failure: Option<String>,
    /// The first `fail_until` deliveries fail with [`Transient`].
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
        let attempt = self.deliveries.record(meeting.clone());
        FakeFailure::check(self.deliver_failure.as_ref())?;
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
                sha256: crate::content_hash::sha256(data.as_bytes()),
            }],
            renderer_version: Self::RENDERER_VERSION,
            warnings: Vec::new(),
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

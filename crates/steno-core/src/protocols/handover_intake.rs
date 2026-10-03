//! Admits a fully received phone recording.
//! Swift: `HandoverIntake` in `Sources/StenoCore/Protocols/PipelineBoundaries.swift`.

use std::path::Path;

use async_trait::async_trait;
use uuid::Uuid;

use super::BoundaryResult;
use crate::{PairedDevice, RecordingMetadata};

/// Admits a fully received phone recording; returns the new `Meeting` id.
/// The core's recording intake implements it.
#[async_trait]
pub trait HandoverIntake: Send + Sync {
    async fn admit(
        &self,
        file: &Path,
        metadata: &RecordingMetadata,
        device: &PairedDevice,
    ) -> BoundaryResult<Uuid>;
}

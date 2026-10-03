//! A one-way push target.
//! Swift: `Sources/StenoCore/Protocols/Destination.swift`.

use async_trait::async_trait;

use super::BoundaryResult;
use crate::{DeliveryReceipt, MeetingExport};

/// A one-way push target. Constructed with its typed settings, renders its
/// own artefacts from the [`MeetingExport`]. `previous == None` is the
/// initial delivery; otherwise the destination overwrites the files it
/// wrote before and touches nothing else.
#[async_trait]
pub trait Destination: Send + Sync {
    /// The stable id the `delivery.destinationID` column carries.
    fn id(&self) -> &str;

    /// Checks the destination's settings (a reachable folder, a vault that
    /// exists) without delivering anything.
    async fn validate(&self) -> BoundaryResult<()>;

    async fn deliver(
        &self,
        meeting: &MeetingExport,
        previous: Option<&DeliveryReceipt>,
    ) -> BoundaryResult<DeliveryReceipt>;
}

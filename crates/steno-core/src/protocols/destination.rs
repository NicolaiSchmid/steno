//! A one-way push target.

use async_trait::async_trait;

use super::BoundaryResult;
use crate::{DeliveryReceipt, MeetingExport};

/// A one-way push target that renders its own artefacts from the
/// [`MeetingExport`]. With a `previous` receipt it overwrites only the files it
/// wrote before.
#[async_trait]
pub trait Destination: Send + Sync {
    /// The stable id the `delivery.destinationID` column carries.
    fn id(&self) -> &str;

    /// Checks the destination's settings without delivering anything.
    async fn validate(&self) -> BoundaryResult<()>;

    async fn deliver(
        &self,
        meeting: &MeetingExport,
        previous: Option<&DeliveryReceipt>,
    ) -> BoundaryResult<DeliveryReceipt>;
}

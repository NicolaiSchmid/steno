//! Delivers one meeting to every configured destination.

use async_trait::async_trait;
use uuid::Uuid;

use crate::Delivery;

/// Delivers one meeting to every configured destination with its stored
/// receipt as `previous`. Never fails: a failed destination is a [`Delivery`]
/// with `Failed`.
#[async_trait]
pub trait DeliveryDispatcher: Send + Sync {
    async fn deliver_all(&self, meeting_id: Uuid) -> Vec<Delivery>;

    /// Marks the export as owed to every configured destination before
    /// [`deliver_all`](Self::deliver_all) runs, so an exit in between leaves rows
    /// the next launch re-exports. The default writes nothing.
    fn mark_pending(&self, _meeting_id: Uuid) {}
}

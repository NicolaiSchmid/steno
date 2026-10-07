//! Delivers one meeting to every configured destination.
//! Swift: `DeliveryDispatcher` in `Sources/StenoCore/Protocols/PipelineBoundaries.swift`.

use async_trait::async_trait;
use uuid::Uuid;

use crate::Delivery;

/// Delivers one meeting to every configured destination (the destinations
/// in `steno-adapters`), passing each destination's stored receipt as
/// `previous`. Never fails: a failed destination is a [`Delivery`] with
/// `Failed`.
#[async_trait]
pub trait DeliveryDispatcher: Send + Sync {
    async fn deliver_all(&self, meeting_id: Uuid) -> Vec<Delivery>;

    /// Marks the meeting's export as owed to every configured destination
    /// (a `Pending` row each, a stored receipt kept) before the pipeline
    /// marks it ready, so an exit between `ready` and
    /// [`deliver_all`](Self::deliver_all) leaves rows the next launch
    /// delivers again. Never fails; the default writes nothing. Rust only.
    fn announce(&self, _meeting_id: Uuid) {}
}

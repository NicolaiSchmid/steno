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
    /// (a `Pending` row each, a stored receipt kept) before
    /// [`deliver_all`](Self::deliver_all) runs: the pipeline calls it
    /// before `persist` marks the meeting ready and once a re-run's summary
    /// is saved, so an exit before `deliver_all` leaves rows the next
    /// launch re-exports. Never fails; the default writes nothing. Rust
    /// only.
    fn mark_pending(&self, _meeting_id: Uuid) {}
}

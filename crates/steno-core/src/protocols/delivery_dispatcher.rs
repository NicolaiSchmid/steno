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
}

//! The `DeliveryDispatcher`.
//! Swift: `Sources/StenoAdapters/Runtime/DeliveryCoordinator.swift`.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use steno_core::{
    Delivery, DeliveryDispatcher, DeliveryStatus, Destination, Settings, Store, async_trait,
};
use uuid::Uuid;

use crate::obsidian::ObsidianFolderDestination;

/// Every destination the settings configure, in delivery order.
pub type DestinationFactory = dyn Fn(&Settings) -> Vec<Arc<dyn Destination>> + Send + Sync;

/// Exports the meeting once, then runs every configured destination in
/// order with one [`Delivery`] row per (meeting, destination), handing each
/// its stored receipt as `previous`. Never fails; a failed export or
/// destination is a `Failed` row and the next destination still runs. There
/// is no separate re-export path: the pipeline's redeliver calls
/// [`DeliveryDispatcher::deliver_all`] again. `deliver_all` does blocking
/// file I/O (the `fsync`ed writes, the audio copy) on the calling thread;
/// run it under `spawn_blocking` or an equivalent, not on an async worker.
pub struct DeliveryCoordinator {
    store: Arc<Store>,
    destinations: Box<DestinationFactory>,
    now: Box<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

impl std::fmt::Debug for DeliveryCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeliveryCoordinator")
            .finish_non_exhaustive()
    }
}

impl DeliveryCoordinator {
    /// The coordinator over the stored settings' destinations
    /// ([`DeliveryCoordinator::destinations_for`]) and the wall clock.
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use steno_adapters::DeliveryCoordinator;
    /// use steno_core::{DeliveryDispatcher as _, Store};
    /// use uuid::Uuid;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let coordinator = DeliveryCoordinator::new(Arc::new(Store::in_memory()?));
    /// let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    /// let rows = runtime.block_on(coordinator.deliver_all(Uuid::new_v4()));
    /// assert!(rows.is_empty(), "no destination configured: nothing to deliver");
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn new(store: Arc<Store>) -> Self {
        Self::with_destinations(store, Box::new(Self::destinations_for), Box::new(Utc::now))
    }

    /// A coordinator with its own destination list and clock; tests pin
    /// both.
    #[must_use]
    pub fn with_destinations(
        store: Arc<Store>,
        destinations: Box<DestinationFactory>,
        now: Box<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    ) -> Self {
        DeliveryCoordinator {
            store,
            destinations,
            now,
        }
    }

    /// Every destination the settings configure, in delivery order. Today
    /// the Obsidian folder when `settings.obsidian` is set, in the machine's
    /// time zone; a second destination adds one line here and one typed
    /// optional to [`Settings`].
    #[must_use]
    pub fn destinations_for(settings: &Settings) -> Vec<Arc<dyn Destination>> {
        settings
            .obsidian
            .iter()
            .map(|obsidian| {
                Arc::new(ObsidianFolderDestination::new(
                    obsidian.clone(),
                    local_time_zone(),
                )) as Arc<dyn Destination>
            })
            .collect()
    }

    /// Fails every stored row with `reason`: settings that do not load
    /// cannot say where to deliver, and silence would leave the meeting
    /// `ready` with stale rows and no trace of the failure.
    fn fail_all(&self, existing: Vec<Delivery>, reason: &str) -> Vec<Delivery> {
        existing
            .into_iter()
            .map(|mut delivery| {
                delivery.status = DeliveryStatus::Failed(reason.to_owned());
                delivery.last_attempt_at = Some((self.now)());
                let _ = self.store.save_delivery(&delivery);
                delivery
            })
            .collect()
    }
}

#[async_trait]
impl DeliveryDispatcher for DeliveryCoordinator {
    async fn deliver_all(&self, meeting_id: Uuid) -> Vec<Delivery> {
        let existing = self.store.deliveries(meeting_id).unwrap_or_default();
        let settings = match self.store.settings() {
            Ok(settings) => settings,
            Err(error) => return self.fail_all(existing, &format!("settings failed: {error}")),
        };
        let targets = (self.destinations)(&settings);
        // A row that still says an export is outstanding for a destination
        // that is no longer configured would defer the audio's expiry
        // forever. Those rows go; a `Delivered` row keeps its receipt for a
        // re-added destination to update in place.
        let stale: Vec<Uuid> = existing
            .iter()
            .filter(|delivery| {
                !targets
                    .iter()
                    .any(|target| target.id() == delivery.destination_id)
                    && delivery.status != DeliveryStatus::Delivered
            })
            .map(|delivery| delivery.id)
            .collect();
        let _ = self.store.delete_deliveries(&stale);
        if targets.is_empty() {
            return Vec::new();
        }
        let export = self.store.export(meeting_id);

        let mut results = Vec::with_capacity(targets.len());
        for destination in &targets {
            let previous = existing
                .iter()
                .find(|delivery| delivery.destination_id == destination.id())
                .and_then(|delivery| delivery.receipt.clone());
            let mut delivery = Delivery {
                id: Delivery::id_for(meeting_id, destination.id()),
                meeting_id,
                destination_id: destination.id().to_owned(),
                status: DeliveryStatus::Pending,
                last_attempt_at: Some((self.now)()),
                receipt: previous.clone(),
            };
            match &export {
                Err(error) => {
                    delivery.status = DeliveryStatus::Failed(format!("export failed: {error}"));
                }
                Ok(export) => {
                    let _ = self.store.save_delivery(&delivery);
                    match destination.deliver(export, previous.as_ref()).await {
                        Ok(receipt) => {
                            delivery.receipt = Some(receipt);
                            delivery.status = DeliveryStatus::Delivered;
                        }
                        Err(error) => delivery.status = DeliveryStatus::Failed(error.to_string()),
                    }
                }
            }
            let _ = self.store.save_delivery(&delivery);
            results.push(delivery);
        }
        results
    }
}

/// The machine's IANA time zone. Swift: `TimeZone.current`. When the name
/// cannot be read or the zone tables do not know it, the destination runs
/// in UTC: the folder dates and the times in the notes shift to UTC and
/// nothing is logged, since the crate has no logger. The shell is to call
/// this and show the zone in Settings, which is where a UTC fallback
/// becomes visible.
#[must_use]
pub fn local_time_zone() -> Tz {
    iana_time_zone::get_timezone()
        .ok()
        .and_then(|name| name.parse().ok())
        .unwrap_or(Tz::UTC)
}

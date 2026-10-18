//! The `DeliveryDispatcher`.

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
/// order with one [`Delivery`] row each, handing it its stored receipt.
/// Never fails: a failure is a `Failed` row. Needs a tokio runtime, as file
/// I/O and store calls run on the blocking pool;
/// [`DeliveryDispatcher::mark_pending`] is synchronous.
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

    /// A coordinator with its own destination list and clock.
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

    /// Every destination the settings configure, in delivery order: today
    /// the Obsidian folder, in the machine's time zone.
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

    /// Fails every stored row with `reason`, so unloadable settings leave
    /// a trace.
    async fn fail_all(&self, existing: Vec<Delivery>, reason: &str) -> Vec<Delivery> {
        let failed: Vec<Delivery> = existing
            .into_iter()
            .map(|mut delivery| {
                delivery.status = DeliveryStatus::Failed(reason.to_owned());
                delivery.last_attempt_at = Some((self.now)());
                delivery
            })
            .collect();
        let saved = failed.clone();
        let _ = self
            .store
            .writing(move |store| {
                for delivery in &saved {
                    let _ = store.save_delivery(delivery);
                }
                Ok(())
            })
            .await;
        failed
    }

    /// Saves `delivery`; a failed save is dropped.
    async fn save(&self, delivery: &Delivery) {
        let delivery = delivery.clone();
        let _ = self
            .store
            .writing(move |store| store.save_delivery(&delivery))
            .await;
    }
}

#[async_trait]
impl DeliveryDispatcher for DeliveryCoordinator {
    /// A `Pending` row per configured destination, keeping stored receipts.
    /// On a load error nothing is written; `deliver_all` fails the rows.
    fn mark_pending(&self, meeting_id: Uuid) {
        let Ok(settings) = self.store.settings() else {
            return;
        };
        let Ok(existing) = self.store.deliveries(meeting_id) else {
            return;
        };
        for destination in (self.destinations)(&settings) {
            let stored = existing
                .iter()
                .find(|delivery| delivery.destination_id == destination.id());
            let delivery = Delivery {
                id: Delivery::id_for(meeting_id, destination.id()),
                meeting_id,
                destination_id: destination.id().to_owned(),
                status: DeliveryStatus::Pending,
                last_attempt_at: stored.and_then(|delivery| delivery.last_attempt_at),
                receipt: stored.and_then(|delivery| delivery.receipt.clone()),
            };
            let _ = self.store.save_delivery(&delivery);
        }
    }

    async fn deliver_all(&self, meeting_id: Uuid) -> Vec<Delivery> {
        let (existing, settings) = self
            .store
            .reading(move |store| {
                Ok((
                    store.deliveries(meeting_id).unwrap_or_default(),
                    store.settings(),
                ))
            })
            .await
            .unwrap_or_else(|error| (Vec::new(), Err(error)));
        let settings = match settings {
            Ok(settings) => settings,
            Err(error) => {
                return self
                    .fail_all(existing, &format!("settings failed: {error}"))
                    .await;
            }
        };
        let targets = (self.destinations)(&settings);
        // Unfinished rows of destinations no longer configured would defer
        // the audio's expiry forever, so they go; `Delivered` rows stay.
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
        let _ = self
            .store
            .writing(move |store| store.delete_deliveries(&stale))
            .await;
        if targets.is_empty() {
            return Vec::new();
        }
        let export = self
            .store
            .reading(move |store| store.export(meeting_id))
            .await;

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
                    self.save(&delivery).await;
                    match destination.deliver(export, previous.as_ref()).await {
                        Ok(receipt) => {
                            delivery.receipt = Some(receipt);
                            delivery.status = DeliveryStatus::Delivered;
                        }
                        Err(error) => delivery.status = DeliveryStatus::Failed(error.to_string()),
                    }
                }
            }
            self.save(&delivery).await;
            results.push(delivery);
        }
        results
    }
}

/// The machine's IANA time zone, or UTC when it cannot be read or is
/// unknown. The shell shows the zone in Settings, where a fallback shows.
#[must_use]
pub fn local_time_zone() -> Tz {
    iana_time_zone::get_timezone()
        .ok()
        .and_then(|name| name.parse().ok())
        .unwrap_or(Tz::UTC)
}

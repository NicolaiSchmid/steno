//! iPhone: paired devices, the pairing QR code, revoke, and transfers in
//! flight. The listener starts for the first pairing and stays on while
//! phones are paired. Swift: `Settings/PhonesSettingsViewModel.swift`.
//!
//! Swift polled the paired devices every two seconds while a code was
//! shown; the blocking host refreshes on [`PhonesSettingsViewModel::refresh_after_pairing`],
//! which the shell calls on its timer and the tests call directly.
//!
//! Rust only: while the handover waits ([`Handover::waiting`]), the
//! section offers Try again and Pair again instead of a pairing. Both may
//! wait on a keychain prompt, so the host runs them in three steps, as the
//! onboarding's permission requests: [`PhonesSettingsViewModel::begin_busy`]
//! under its lock, the call with the lock released, then
//! [`PhonesSettingsViewModel::finish_try_again`] or
//! [`PhonesSettingsViewModel::finish_pair_again`].
//!
//! [`Handover::waiting`]: crate::services::Handover::waiting

use chrono::{DateTime, Utc};
use steno_bridge::PhoneWaitingReason;
use steno_core::protocols::BoundaryResult;
use steno_core::{HandoverReceipt, HandoverStateKind, PairedDevice};
use uuid::Uuid;

use super::SectionError;
use crate::services::{ListenerState, PairingCode, Services};

#[derive(Debug, Default)]
pub struct PhonesSettingsViewModel {
    pub devices: Vec<PairedDevice>,
    pub listener: ListenerState,
    pub receipts: Vec<HandoverReceipt>,
    pub pairing: Option<PairingCode>,
    /// The pairing code as a PNG, base64; the page draws it.
    pub qr_png_base64: Option<String>,
    /// Why the handover waits, as of the last refresh.
    pub waiting: Option<PhoneWaitingReason>,
    /// Try again or Pair again is under way.
    pub busy: bool,
    pub errors: SectionError,
}

/// The section's line when Try again left the handover waiting.
pub const TRY_AGAIN_FAILED: &str = "Your phones still can't send recordings.";
/// The section's line when Pair again did not finish.
pub const PAIR_AGAIN_FAILED: &str = "Steno could not make a new pairing.";

impl PhonesSettingsViewModel {
    #[must_use]
    pub fn new(services: &Services) -> Self {
        PhonesSettingsViewModel {
            listener: services
                .handover
                .as_ref()
                .map_or(ListenerState::Stopped, |handover| handover.state()),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn is_available(services: &Services) -> bool {
        services.handover.is_some()
    }

    #[must_use]
    pub fn mac_id(services: &Services) -> Option<String> {
        services.handover.as_ref().map(|handover| handover.mac_id())
    }

    #[must_use]
    pub fn pairing_is_open(&self, now: DateTime<Utc>) -> bool {
        self.pairing
            .as_ref()
            .is_some_and(|pairing| pairing.expires_at > now)
    }

    /// Transfers still arriving, oldest first.
    #[must_use]
    pub fn active_receipts(&self) -> Vec<&HandoverReceipt> {
        let mut active: Vec<&HandoverReceipt> = self
            .receipts
            .iter()
            .filter(|receipt| {
                matches!(
                    receipt.state.kind(),
                    HandoverStateKind::Receiving | HandoverStateKind::Verifying
                )
            })
            .collect();
        active.sort_by_key(|receipt| receipt.created_at);
        active
    }

    /// The listener state, the receipts and why the handover waits, as the
    /// service has them now: what the observations delivered.
    pub fn refresh(&mut self, services: &Services) {
        if let Some(handover) = &services.handover {
            self.listener = handover.state();
            self.receipts = handover.receipts();
            self.waiting = handover.waiting();
        }
    }

    /// Try again or Pair again is about to run with the host's lock
    /// released; `false` while one is already under way, which the host
    /// then leaves to itself.
    pub fn begin_busy(&mut self) -> bool {
        !std::mem::replace(&mut self.busy, true)
    }

    /// The user did not confirm Pair again: nothing ran.
    pub fn cancel_busy(&mut self) {
        self.busy = false;
    }

    /// Try again's outcome: the line says when the handover still waits.
    pub fn finish_try_again(&mut self, outcome: BoundaryResult<()>, services: &Services) {
        self.finish(outcome, TRY_AGAIN_FAILED, services);
    }

    /// Pair again's outcome: the phones it removed leave the list.
    pub fn finish_pair_again(&mut self, outcome: BoundaryResult<()>, services: &Services) {
        self.finish(outcome, PAIR_AGAIN_FAILED, services);
    }

    fn finish(&mut self, outcome: BoundaryResult<()>, failed: &str, services: &Services) {
        self.busy = false;
        match outcome {
            Ok(()) => self.errors.clear(),
            Err(error) => self.errors.fail(failed, error),
        }
        self.load(services);
        self.refresh(services);
    }

    pub fn load(&mut self, services: &Services) {
        let Some(handover) = &services.handover else {
            return;
        };
        match handover.paired_devices() {
            Ok(loaded) => {
                if loaded != self.devices {
                    self.devices = loaded;
                }
            }
            Err(error) => self
                .errors
                .fail("Paired phones could not be loaded.", error),
        }
    }

    /// Starts the listener when needed and opens a pairing window.
    pub fn begin_pairing(&mut self, services: &Services) {
        let Some(handover) = &services.handover else {
            return;
        };
        if let Err(error) = handover.start() {
            self.errors.fail("Pairing could not start.", error);
            return;
        }
        let code = handover.begin_pairing();
        self.qr_png_base64 = services.qr.png_base64(&code.url_string);
        self.pairing = Some(code);
        self.listener = handover.state();
        self.errors.clear();
    }

    pub fn cancel_pairing(&mut self, services: &Services) {
        let Some(handover) = &services.handover else {
            return;
        };
        handover.cancel_pairing();
        self.pairing = None;
        self.qr_png_base64 = None;
        self.load(services);
        if self.devices.is_empty() {
            handover.stop();
        }
        self.listener = handover.state();
    }

    pub fn revoke(&mut self, device_id: Uuid, services: &Services) {
        let Some(handover) = &services.handover else {
            return;
        };
        match handover.revoke(device_id) {
            Ok(()) => {
                self.load(services);
                if self.devices.is_empty() && self.pairing.is_none() {
                    handover.stop();
                }
                self.listener = handover.state();
            }
            Err(error) => self.errors.fail("The phone could not be removed.", error),
        }
    }

    /// A device appearing in the list closes the pairing window; a window
    /// that ran out with no phone closes itself.
    pub fn refresh_after_pairing(&mut self, services: &Services, now: DateTime<Utc>) {
        let before: Vec<Uuid> = self.devices.iter().map(|device| device.id).collect();
        self.load(services);
        let after: Vec<Uuid> = self.devices.iter().map(|device| device.id).collect();
        if before != after {
            self.pairing = None;
            self.qr_png_base64 = None;
        } else if self.pairing.is_some() && !self.pairing_is_open(now) {
            self.cancel_pairing(services);
        }
    }
}

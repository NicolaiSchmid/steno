//! iPhone: paired devices, the pairing QR code, revoke, and transfers in
//! flight. The listener starts for the first pairing and stays on while
//! phones are paired. Swift: `Settings/PhonesSettingsViewModel.swift`.
//!
//! Swift polled the paired devices every two seconds while a code was
//! shown; the blocking host refreshes on [`PhonesSettingsViewModel::refresh_after_pairing`],
//! which the shell calls on its timer and the tests call directly.

use chrono::{DateTime, Utc};
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
    pub errors: SectionError,
}

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

    /// The listener state and the receipts as the service has them now:
    /// what the two observations delivered.
    pub fn refresh(&mut self, services: &Services) {
        if let Some(handover) = &services.handover {
            self.listener = handover.state();
            self.receipts = handover.receipts();
        }
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

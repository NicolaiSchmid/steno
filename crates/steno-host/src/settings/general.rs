//! General: launch at login, meeting detection, the calendar permission
//! that names meetings, the default template, and the update status.
//! Swift: `Settings/GeneralSettingsViewModel.swift`.

use steno_bridge::{PermissionKind, PermissionState};
use steno_core::{Store, SummaryTemplate};

use super::{SectionError, update_settings};
use crate::services::{LoginItemStatus, Services};

#[derive(Debug)]
pub struct GeneralSettingsViewModel {
    pub login_item: LoginItemStatus,
    pub detection_enabled: bool,
    pub default_template_id: String,
    pub calendar_permission: PermissionState,
    pub requesting_calendar: bool,
    pub errors: SectionError,
}

impl GeneralSettingsViewModel {
    #[must_use]
    pub fn new(services: &Services) -> Self {
        GeneralSettingsViewModel {
            login_item: services.login_item.status(),
            detection_enabled: true,
            default_template_id: SummaryTemplate::DEFAULT_ID.to_owned(),
            calendar_permission: PermissionState::Unknown,
            requesting_calendar: false,
            errors: SectionError::default(),
        }
    }

    pub fn load(&mut self, store: &Store, services: &Services) {
        match store.settings() {
            Ok(settings) => {
                self.detection_enabled = settings.meeting_detection_enabled;
                self.default_template_id = settings.default_template_id;
                self.login_item = services.login_item.status();
            }
            Err(error) => self.errors.fail("Settings could not be loaded.", error),
        }
        self.calendar_permission = services.permissions.state(PermissionKind::Calendar);
    }

    #[must_use]
    pub fn launch_at_login(&self) -> bool {
        self.login_item.is_on()
    }

    /// The login item and the setting together. Swift: `AppEnvironment.setLaunchAtLogin`.
    pub fn set_launch_at_login(&mut self, enabled: bool, store: &Store, services: &Services) {
        let outcome = services.login_item.set_enabled(enabled).and_then(|()| {
            update_settings(store, |settings| settings.launch_at_login = enabled)?;
            Ok(())
        });
        if let Err(error) = outcome {
            self.errors
                .fail("Opening Steno at login could not be changed.", error);
        }
        self.login_item = services.login_item.status();
    }

    pub fn open_login_item_settings(&self, services: &Services) {
        services.login_item.open_system_settings();
    }

    pub fn set_detection_enabled(&mut self, enabled: bool, store: &Store) {
        self.detection_enabled = enabled;
        self.save(store, |settings| {
            settings.meeting_detection_enabled = enabled;
        });
    }

    /// An id the bundle does not have changes nothing.
    pub fn set_default_template(&mut self, id: &str, store: &Store) {
        if SummaryTemplate::bundled_with_id(id).is_none() {
            return;
        }
        id.clone_into(&mut self.default_template_id);
        let id = id.to_owned();
        self.save(store, move |settings| settings.default_template_id = id);
    }

    pub fn request_calendar(&mut self, services: &Services) {
        self.requesting_calendar = true;
        self.calendar_permission = services.permissions.request(PermissionKind::Calendar);
        self.requesting_calendar = false;
    }

    fn save(&mut self, store: &Store, mutate: impl FnOnce(&mut steno_core::Settings)) {
        match update_settings(store, mutate) {
            Ok(_) => self.errors.clear(),
            Err(error) => self.errors.fail("The setting could not be saved.", error),
        }
    }
}

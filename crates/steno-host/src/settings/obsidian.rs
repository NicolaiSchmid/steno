//! Export: `Settings::obsidian` (`None` means off), validated on save
//! through the destination; its messages are shown verbatim.
//! Swift: `Settings/ObsidianSettingsViewModel.swift`.

use std::path::{Path, PathBuf};

use steno_core::{ObsidianSettings, Store};

use super::{SectionError, update_settings};
use crate::services::Services;

#[derive(Debug, Default)]
pub struct ObsidianSettingsViewModel {
    pub enabled: bool,
    pub vault_path: String,
    pub people_folder: String,
    pub include_audio: bool,
    pub task_tag: String,
    pub errors: SectionError,
    pub validation_message: Option<String>,
    /// True after a save; cleared by saving again.
    pub saved: bool,
    stored: Option<ObsidianSettings>,
}

impl ObsidianSettingsViewModel {
    pub fn load(&mut self, store: &Store) {
        match store.settings() {
            Ok(settings) => {
                self.stored.clone_from(&settings.obsidian);
                if let Some(obsidian) = settings.obsidian {
                    self.enabled = true;
                    self.vault_path = obsidian.vault_path;
                    self.people_folder = obsidian.people_folder.unwrap_or_default();
                    self.include_audio = obsidian.include_audio;
                    self.task_tag = obsidian.task_tag.unwrap_or_default();
                } else {
                    self.enabled = false;
                }
            }
            Err(error) => self.errors.fail("Settings could not be loaded.", error),
        }
    }

    /// The vault's folder name, for the folder row; empty until chosen.
    #[must_use]
    pub fn vault_name(&self) -> String {
        self.vault_url()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_default()
    }

    #[must_use]
    pub fn vault_url(&self) -> Option<PathBuf> {
        (!self.vault_path.is_empty()).then(|| PathBuf::from(&self.vault_path))
    }

    /// On, but no folder chosen yet.
    #[must_use]
    pub fn needs_vault(&self) -> bool {
        self.enabled && self.vault_path.trim().is_empty()
    }

    /// The typed settings as entered; `None` when disabled. Fields this
    /// build does not know come from the stored value, so a draft nobody
    /// edited equals it and [`commit`](Self::commit) saves nothing; the
    /// save takes them again from the value stored at that moment, since
    /// another writer may have added some.
    #[must_use]
    pub fn draft(&self) -> Option<ObsidianSettings> {
        if !self.enabled {
            return None;
        }
        let people = self.people_folder.trim();
        let tag = self.task_tag.trim();
        Some(ObsidianSettings {
            vault_path: self.vault_path.clone(),
            people_folder: (!people.is_empty()).then(|| people.to_owned()),
            include_audio: self.include_audio,
            task_tag: (!tag.is_empty()).then(|| tag.to_owned()),
            extra: self
                .stored
                .as_ref()
                .map(|stored| stored.extra.clone())
                .unwrap_or_default(),
        })
    }

    pub fn set_enabled(&mut self, on: bool, store: &Store, services: &Services) {
        self.enabled = on;
        self.commit(store, services);
    }

    pub fn choose_vault(&mut self, folder: &Path, store: &Store, services: &Services) {
        self.vault_path = folder.to_string_lossy().into_owned();
        self.commit(store, services);
    }

    pub fn set_include_audio(&mut self, on: bool, store: &Store, services: &Services) {
        self.include_audio = on;
        self.commit(store, services);
    }

    /// Saves when the draft differs from what is stored. On without a
    /// vault waits for the chooser and saves nothing.
    pub fn commit(&mut self, store: &Store, services: &Services) {
        if self.needs_vault() || self.draft() == self.stored {
            self.validation_message = None;
            return;
        }
        self.save(store, services);
    }

    /// Validates through the destination and stores the settings; a failed
    /// validation stores nothing and reports the destination's message.
    pub fn save(&mut self, store: &Store, services: &Services) {
        self.saved = false;
        self.validation_message = None;
        let draft = self.draft();
        if let Some(draft) = &draft
            && let Err(message) = services.export_validator.validate(draft)
        {
            self.validation_message = Some(message.to_string());
            return;
        }
        let result = update_settings(store, |settings| {
            // The unknown fields as stored now, not as loaded: another
            // writer may have added some since. They may belong to the
            // vault, so another vault starts without them.
            let mut next = draft;
            if let Some(next) = &mut next {
                next.extra = match &settings.obsidian {
                    Some(current) if current.vault_path == next.vault_path => current.extra.clone(),
                    _ => serde_json::Map::new(),
                };
            }
            settings.obsidian = next;
        });
        match result {
            Ok(settings) => {
                self.stored = settings.obsidian;
                self.saved = true;
                self.errors.clear();
            }
            Err(error) => self.errors.fail("Settings could not be saved.", error),
        }
    }
}

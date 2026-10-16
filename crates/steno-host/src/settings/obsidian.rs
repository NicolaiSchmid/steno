//! Export: `Settings::obsidian` (`None` means off), validated on save
//! through the destination with the host's lock released; its messages
//! are shown verbatim.
//! Swift: `Settings/ObsidianSettingsViewModel.swift`.

use std::path::{Path, PathBuf};

use steno_core::{ObsidianSettings, Store};

use super::{SectionError, update_settings};
use crate::services::{Services, VaultRefusal};

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
    /// Counts the saves begun, so that each has a number.
    saves: u64,
    /// The number of the save whose vault check runs now: only that save
    /// stores, and a load while it runs leaves the form to it.
    in_flight: Option<u64>,
}

/// A save begun under the host's lock, whose vault check runs with the
/// lock released ([`ObsidianSettingsViewModel::begin_save`]). Rust only:
/// Swift validated on the main actor, which blocked nothing else.
#[derive(Debug)]
pub struct VaultSave {
    /// The settings to store; `None` turns the export off.
    draft: Option<ObsidianSettings>,
    /// Its number among the view model's saves.
    number: u64,
}

impl VaultSave {
    /// The destination's check of the draft: it writes and removes a probe
    /// file in the vault, which a slow folder, or the Mac while its Files
    /// and Folders prompt is up, can hold. Turning the export off checks
    /// nothing.
    pub fn validate(&self, services: &Services) -> Result<(), VaultRefusal> {
        self.draft
            .as_ref()
            .map_or(Ok(()), |draft| services.export_validator.validate(draft))
    }
}

impl ObsidianSettingsViewModel {
    /// Loads the stored settings into the form. A save whose vault check
    /// runs keeps the form: it stores the draft the user chose, and a load
    /// would show the settings it replaces.
    pub fn load(&mut self, store: &Store) {
        if self.in_flight.is_some() {
            return;
        }
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

    pub fn set_enabled(&mut self, on: bool) -> Option<VaultSave> {
        self.enabled = on;
        self.commit()
    }

    pub fn choose_vault(&mut self, folder: &Path) -> Option<VaultSave> {
        self.vault_path = folder.to_string_lossy().into_owned();
        self.commit()
    }

    pub fn set_include_audio(&mut self, on: bool) -> Option<VaultSave> {
        self.include_audio = on;
        self.commit()
    }

    /// Begins a save when the draft differs from what is stored, or when
    /// a check runs: an edit undone while it runs saves the stored value
    /// again, so the earlier save does not store what the form no longer
    /// shows. On without a vault waits for the chooser and saves nothing.
    pub fn commit(&mut self) -> Option<VaultSave> {
        if self.needs_vault() || (self.in_flight.is_none() && self.draft() == self.stored) {
            self.validation_message = None;
            return None;
        }
        Some(self.begin_save())
    }

    /// Begins a save of the draft: [`VaultSave::validate`] checks it
    /// through the destination, which writes and removes a probe file in
    /// the vault, so the host runs it with its lock released, and
    /// [`finish_save`](Self::finish_save) stores it.
    pub fn begin_save(&mut self) -> VaultSave {
        self.saved = false;
        self.validation_message = None;
        self.saves += 1;
        self.in_flight = Some(self.saves);
        VaultSave {
            draft: self.draft(),
            number: self.saves,
        }
    }

    /// Stores the settings `save` validated; a failed validation stores
    /// nothing and reports the destination's message. A save that another
    /// save followed while its check ran stores nothing: the later one
    /// decides what is stored.
    pub fn finish_save(
        &mut self,
        save: VaultSave,
        validated: Result<(), VaultRefusal>,
        store: &Store,
    ) {
        if self.in_flight != Some(save.number) {
            return;
        }
        self.in_flight = None;
        if let Err(message) = validated {
            self.validation_message = Some(message.to_string());
            return;
        }
        let draft = save.draft;
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

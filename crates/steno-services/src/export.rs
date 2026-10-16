//! The host's `ExportValidator` over the Obsidian destination: the Export
//! settings' vault check, and onboarding's vault row, which tells a folder
//! that refused the write from one that is not there. Swift:
//! `ObsidianFolderDestination.validate()`.

use steno_adapters::{ObsidianError, ObsidianFolderDestination};
use steno_core::ObsidianSettings;
use steno_host::services::{ExportValidator, VaultRefusal};

#[derive(Debug, Default)]
pub struct ObsidianExportValidator;

impl ExportValidator for ObsidianExportValidator {
    fn validate(&self, settings: &ObsidianSettings) -> Result<(), VaultRefusal> {
        ObsidianFolderDestination::new(settings.clone(), steno_adapters::runtime::local_time_zone())
            .validate_vault()
            .map_err(|error| VaultRefusal {
                not_writable: matches!(error, ObsidianError::VaultNotWritable(_)),
                message: error.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(path: &std::path::Path) -> ObsidianSettings {
        ObsidianSettings {
            vault_path: path.to_string_lossy().into_owned(),
            people_folder: None,
            include_audio: false,
            task_tag: None,
            extra: serde_json::Map::new(),
        }
    }

    /// A vault that is not there is not a folder that refused the write;
    /// a folder that refused it is, and both keep the destination's words.
    #[test]
    fn a_refusal_says_whether_the_folder_refused_the_write() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("gone");
        let refusal = ObsidianExportValidator
            .validate(&vault(&missing))
            .unwrap_err();
        assert!(!refusal.not_writable);
        assert_eq!(
            refusal.message,
            ObsidianError::VaultMissing(missing.to_string_lossy().into_owned()).to_string()
        );
        ObsidianExportValidator
            .validate(&vault(directory.path()))
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let locked = directory.path().join("locked");
            std::fs::create_dir(&locked).unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
            if std::fs::write(locked.join("probe"), b"").is_err() {
                let refusal = ObsidianExportValidator
                    .validate(&vault(&locked))
                    .unwrap_err();
                assert!(refusal.not_writable, "{refusal}");
            }
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

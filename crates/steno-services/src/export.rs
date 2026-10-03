//! The host's `ExportValidator` over the Obsidian destination: the Export
//! settings' vault check. Swift: `ObsidianFolderDestination.validateVault`.

use steno_adapters::ObsidianFolderDestination;
use steno_core::ObsidianSettings;
use steno_host::services::ExportValidator;

#[derive(Debug, Default)]
pub struct ObsidianExportValidator;

impl ExportValidator for ObsidianExportValidator {
    fn validate(&self, settings: &ObsidianSettings) -> Result<(), String> {
        ObsidianFolderDestination::new(settings.clone(), steno_adapters::runtime::local_time_zone())
            .validate_vault()
            .map_err(|error| error.to_string())
    }
}

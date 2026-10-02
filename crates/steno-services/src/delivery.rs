//! The host's `ExportValidator` over the Obsidian destination.

use steno_adapters::ObsidianFolderDestination;
use steno_core::ObsidianSettings;
use steno_host::services::ExportValidator;

#[derive(Debug, Default)]
pub struct ObsidianValidator;

impl ExportValidator for ObsidianValidator {
    fn validate(&self, settings: &ObsidianSettings) -> Result<(), String> {
        ObsidianFolderDestination::new(settings.clone(), steno_adapters::runtime::local_time_zone())
            .validate_vault()
            .map_err(|error| error.to_string())
    }
}

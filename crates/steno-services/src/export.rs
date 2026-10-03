//! The host's `ExportValidator` over the Obsidian destination: the Export
//! settings' vault check. Swift: `ObsidianFolderDestination.validate()`.

use steno_adapters::ObsidianFolderDestination;
use steno_core::ObsidianSettings;
use steno_core::protocols::BoundaryResult;
use steno_host::services::ExportValidator;

#[derive(Debug, Default)]
pub struct ObsidianExportValidator;

impl ExportValidator for ObsidianExportValidator {
    fn validate(&self, settings: &ObsidianSettings) -> BoundaryResult<()> {
        Ok(ObsidianFolderDestination::new(
            settings.clone(),
            steno_adapters::runtime::local_time_zone(),
        )
        .validate_vault()?)
    }
}

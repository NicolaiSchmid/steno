//! `steno export <meeting-id>`: writes `meeting.json` (the `StenoJSON`
//! pretty encoding of `MeetingExport`) into `--out` and prints its path.

use std::path::PathBuf;

use clap::Args;
use uuid::Uuid;

use crate::wiring::{DatabaseOptions, Failure, Outcome, parse_uuid};

#[derive(Debug, Args)]
pub struct Export {
    /// The meeting id printed by steno process.
    #[arg(value_parser = parse_uuid)]
    pub meeting_id: Uuid,
    /// Output directory; defaults to the current directory.
    #[arg(long, default_value = ".")]
    pub out: PathBuf,
    #[command(flatten)]
    pub database: DatabaseOptions,
}

impl Export {
    pub fn run(self) -> Outcome {
        let store = self.database.open()?;
        let export = store
            .export(self.meeting_id)
            .map_err(Failure::runtime)?
            .ok_or_else(|| Failure::runtime(format!("meeting {} not found", self.meeting_id)))?;
        let out = crate::wiring::standardized(&self.out);
        std::fs::create_dir_all(&out).map_err(Failure::runtime)?;
        let path = out.join("meeting.json");
        let json = steno_adapters::ArtifactRenderer
            .render_json(&export)
            .map_err(Failure::runtime)?;
        // A partial file a killed process left behind is overwritten and
        // renamed away by the next export.
        steno_services::files::replace_file(
            &path,
            &out.join(".meeting.json.partial"),
            &json,
            false,
        )
        .map_err(Failure::runtime)?;
        println!("{}", path.display());
        Ok(())
    }
}

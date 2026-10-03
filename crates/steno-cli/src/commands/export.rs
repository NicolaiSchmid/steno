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
        // Written beside the target and renamed over it, so a reader never
        // sees a half-written file. A failed write removes its partial
        // file; one a killed process left behind is overwritten and
        // renamed away by the next export.
        let partial = out.join(".meeting.json.partial");
        std::fs::write(&partial, json)
            .and_then(|()| std::fs::rename(&partial, &path))
            .map_err(|error| {
                let _ = std::fs::remove_file(&partial);
                Failure::runtime(error)
            })?;
        println!("{}", path.display());
        Ok(())
    }
}

//! `steno deliver <meeting-id>`: re-exports a processed meeting to every
//! configured destination through the pipeline's `redeliver` and the real
//! coordinator. Without `--vault` the stored Obsidian settings decide; with
//! it the destination is built for this run alone under its own id
//! (`obsidian-folder@<vault>`), so neither the stored settings nor the
//! stored destination's receipt are touched. Prints one line per
//! destination of this run.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Args;
use steno_adapters::{DeliveryCoordinator, ObsidianFolderDestination};
use steno_core::{DeliveryStatus, Destination, ObsidianSettings};
use steno_pipeline::{MeetingEventBus, ProcessingPipeline};
use uuid::Uuid;

use crate::wiring::{DatabaseOptions, Failure, Outcome, parse_uuid};

#[derive(Debug, Args)]
pub struct ObsidianOptions {
    /// Obsidian vault for this run; defaults to the stored Obsidian settings.
    #[arg(long)]
    pub vault: Option<PathBuf>,
    /// Folder for person pages inside --vault.
    #[arg(long = "people-folder")]
    pub people_folder: Option<String>,
    /// Copy the audio mixdown into --vault.
    #[arg(long = "include-audio")]
    pub include_audio: bool,
    /// Tag appended to every task line in --vault.
    #[arg(long = "task-tag")]
    pub task_tag: Option<String>,
}

impl ObsidianOptions {
    fn validate(&self) -> Result<(), Failure> {
        if self.vault.is_none() && (self.people_folder.is_some() || self.include_audio || self.task_tag.is_some()) {
            return Err(Failure::usage(
                "--people-folder, --include-audio and --task-tag need --vault.",
            ));
        }
        Ok(())
    }

    /// The destination for `--vault`, `None` when the stored settings decide.
    fn destination(&self) -> Option<Arc<dyn Destination>> {
        let vault = self.vault.as_ref()?;
        let path = std::path::absolute(vault)
            .unwrap_or_else(|_| vault.clone())
            .to_string_lossy()
            .trim_end_matches('/')
            .to_owned();
        Some(Arc::new(ObsidianFolderDestination::with_id(
            ObsidianSettings {
                vault_path: path.clone(),
                people_folder: self.people_folder.clone(),
                include_audio: self.include_audio,
                task_tag: self.task_tag.clone(),
            },
            steno_adapters::runtime::local_time_zone(),
            &format!("{}@{path}", ObsidianFolderDestination::DESTINATION_ID),
        )))
    }
}

#[derive(Debug, Args)]
pub struct Deliver {
    /// The meeting id printed by `steno process`.
    #[arg(value_parser = parse_uuid)]
    pub meeting_id: Uuid,
    #[command(flatten)]
    pub obsidian: ObsidianOptions,
    #[command(flatten)]
    pub database: DatabaseOptions,
}

impl Deliver {
    pub async fn run(self) -> Outcome {
        self.obsidian.validate()?;
        let store = self.database.open()?;
        let settings = store.settings().map_err(Failure::runtime)?;
        let targets: Vec<Arc<dyn Destination>> = if let Some(ad_hoc) = self.obsidian.destination() {
            vec![ad_hoc]
        } else {
            let stored = DeliveryCoordinator::destinations_for(&settings);
            if stored.is_empty() {
                return Err(Failure::runtime(
                    "No destination configured: set the Obsidian vault in Settings or pass --vault.",
                ));
            }
            stored
        };
        let ids: Vec<String> = targets.iter().map(|t| t.id().to_owned()).collect();
        let dispatcher = super::process::ad_hoc_dispatcher(store.clone(), targets);
        let pipeline = ProcessingPipeline::new(crate::wiring::dependencies(
            store.clone(),
            &settings,
            None,
            None,
            Some(dispatcher),
            None,
            MeetingEventBus::new(),
        )?);
        pipeline
            .redeliver(self.meeting_id)
            .await
            .map_err(Failure::runtime)?;

        let mut failures = Vec::new();
        for delivery in store
            .deliveries(self.meeting_id)
            .map_err(Failure::runtime)?
            .into_iter()
            .filter(|delivery| ids.contains(&delivery.destination_id))
        {
            match &delivery.status {
                DeliveryStatus::Delivered => {
                    let folder = delivery
                        .receipt
                        .as_ref()
                        .map(|receipt| {
                            steno_adapters::runtime::receipt_folder_path(receipt)
                                .to_string_lossy()
                                .into_owned()
                        })
                        .unwrap_or_default();
                    println!("{}\tdelivered\t{folder}", delivery.destination_id);
                }
                DeliveryStatus::Failed(reason) => {
                    println!("{}\tfailed\t{reason}", delivery.destination_id);
                    failures.push(format!("{}: {reason}", delivery.destination_id));
                }
                DeliveryStatus::Pending => println!("{}\tpending", delivery.destination_id),
            }
        }
        if !failures.is_empty() {
            return Err(Failure::runtime(format!("delivery failed: {}", failures.join("; "))));
        }
        Ok(())
    }
}

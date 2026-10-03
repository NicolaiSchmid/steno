//! The command tree. Swift: `Sources/steno/Steno.swift` and `Commands/`.

pub mod deliver;
pub mod dev;
pub mod export;
pub mod process;
pub mod record;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "steno", version, about = "Bot-free meeting recorder.")]
pub struct Steno {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Record a meeting from the microphone and system audio.
    Record(record::Record),
    /// Run the processing pipeline over a WAV file.
    Process(process::Process),
    /// Write a meeting as meeting.json.
    Export(export::Export),
    /// Deliver a processed meeting to every configured destination.
    Deliver(deliver::Deliver),
    /// Developer tools.
    Dev(dev::Dev),
}

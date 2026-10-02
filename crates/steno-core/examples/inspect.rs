//! Opens a Steno database with the Rust store and prints row counts and
//! the first meeting's fields. `scripts/verify-store-against-swift-db.sh`
//! runs it on a copy of a database the Swift app wrote.

use std::process::ExitCode;

use steno_core::Store;

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: inspect <path to steno.sqlite>");
        return ExitCode::from(2);
    };
    match run(&path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let store = Store::open(path)?;
    println!("migrations: {}", store.applied_migrations()?.join(", "));
    for table in [
        "meeting",
        "participant",
        "person",
        "speaker",
        "speakerNameSuggestion",
        "transcriptSegment",
        "meetingTask",
        "decision",
        "audioAsset",
        "delivery",
        "setting",
        "stageRate",
    ] {
        let count: i64 = store.read(|connection| {
            Ok(
                connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })?,
            )
        })?;
        println!("{table}: {count}");
    }
    let meetings = store.meetings(1, 0)?;
    if let Some(meeting) = meetings.first() {
        println!("first meeting:");
        println!("  id: {}", steno_core::json::uuid_string(meeting.id));
        println!("  title: {}", meeting.title);
        println!(
            "  startedAt: {}",
            steno_core::json::format_date(meeting.started_at)
        );
        println!("  duration: {}", meeting.duration);
        println!("  source: {}", meeting.source);
        println!("  state: {:?}", meeting.state);
        println!("  tags: {:?}", meeting.tags);
        println!("  titleOrigin: {}", meeting.title_origin);
        println!("  language: {:?}", meeting.language);
        println!(
            "  summary sections: {}",
            meeting.summary.as_ref().map_or(0, |s| s.sections.len())
        );
        println!("  segments: {}", store.segments(meeting.id)?.len());
        println!("  speakers: {}", store.speakers(meeting.id)?.len());
        println!("  participants: {}", store.participants(meeting.id)?.len());
        println!("  tasks: {}", store.tasks(meeting.id)?.len());
        println!(
            "  asset: {}",
            store
                .asset(meeting.id)?
                .map_or("none".to_owned(), |a| a.format.to_string())
        );
    }
    let settings = store.settings()?;
    println!(
        "settings: provider {}, retention {:?}",
        settings.llm_provider, settings.default_retention
    );
    Ok(())
}

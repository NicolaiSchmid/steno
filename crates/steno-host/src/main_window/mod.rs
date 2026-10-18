//! The main window's view models and their snapshots.

pub mod detail;
pub mod list;
pub mod progress;
pub mod snapshots;

pub use detail::{MeetingDetailViewModel, RecordingStatus};
pub use list::{DayGroup, MeetingListViewModel};
pub use progress::{
    MeetingEvent, Parked, ProcessingProgress, ProcessingProgressModel, ProgressEntry,
};

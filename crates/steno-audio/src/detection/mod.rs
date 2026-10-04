//! Meeting detection: which processes hold the microphone, and the
//! debounced "a call started / ended" events built from it.
//! Swift: `Sources/StenoAudio/Detection/`.

pub mod activity;
pub mod detector;
pub mod live;
pub mod sessions;

pub use activity::{ActivityError, ProcessAudioActivity, ProcessAudioActivitySource};
pub use detector::{MeetingDetector, MeetingEvent};
pub use live::LiveProcessAudioActivity;
pub use sessions::{AudioSessionRecord, EndpointFlow, SessionState, processes_from_sessions};

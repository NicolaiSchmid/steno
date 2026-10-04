//! Capture: the backend seam, the session state machine over it, the
//! stream layout and the device-change comparison, and the macOS live
//! backend. Swift: `Sources/StenoAudio/Capture/`.

pub mod backend;
pub mod configuration;
pub mod layout;
pub mod live;
pub mod nominal_rate;
pub mod session;
pub mod snapshot;
pub mod split_streams;

pub use backend::{CaptureBackend, CaptureStream};
pub use configuration::{
    CaptureConfiguration, CaptureError, CaptureMode, CaptureNotice, CaptureResult, CaptureState,
    CaptureStatistics, DeviceChangeReason, LaneLevel, LaneLevels,
};
pub use layout::{ChannelRef, LaneSource, StreamLayout};
pub use live::LiveCaptureBackend;
pub use nominal_rate::NominalSampleRate;
pub use session::{CaptureSession, RecordingWriterFactory};
pub use snapshot::DeviceSnapshot;
pub use split_streams::{SplitStreamPlan, StreamSource};

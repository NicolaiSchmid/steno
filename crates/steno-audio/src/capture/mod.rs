//! Capture: the backend seam, the session state machine over it, the
//! stream layout, the device-change comparison, the two-stream plan
//! (`split_streams`), the live backends (Core Audio, PipeWire, WASAPI)
//! and how loud their starts log while restarts go on (`start_log`).
//! Swift: `Sources/StenoAudio/Capture/`.

pub mod backend;
pub mod configuration;
pub mod layout;
pub mod live;
pub mod nominal_rate;
pub mod session;
pub mod snapshot;
pub mod split_streams;
pub(crate) mod start_log;

pub use backend::{CaptureBackend, CaptureInput, CaptureStream};
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

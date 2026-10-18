//! Capture: the backend seam, the session state machine over it, the
//! stream layout, the live backends (Core Audio, PipeWire, WASAPI) and the
//! Mac's system audio permission probe.

pub mod backend;
pub mod configuration;
pub mod layout;
pub(crate) mod level_publisher;
pub mod live;
pub mod nominal_rate;
pub mod permission;
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

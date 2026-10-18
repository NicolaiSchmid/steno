//! The input picker's device list on Linux: the sources the capture can
//! record, read from the PipeWire registry over one short connection per
//! call.

use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use super::{Connection, START_TIMEOUT};
use crate::capture::CaptureError;
use crate::capture::live::AudioDeviceInfo;

/// How long [`AudioDevices::inputs`] waits for its thread:
/// [`START_TIMEOUT`] and a scheduling margin.
const LIST_TIMEOUT: Duration = START_TIMEOUT.saturating_add(Duration::from_secs(1));

/// The PipeWire registry's sources, read fresh on every call; the same
/// interface as the other platforms' `AudioDevices`.
pub struct AudioDevices;

impl AudioDevices {
    /// Every source the microphone lane can record (sources, virtual
    /// sources and duplex devices). `uid` is the `node.name`, `name` the
    /// `node.description` (else `node.nick`, else the UID); the rate is 0
    /// and the transport the `media.class`.
    ///
    /// The default is left out on purpose: binding the `default` metadata
    /// before the session manager answered its ping stops its events for
    /// every client on PipeWire before 1.6.9, a recording capture included.
    ///
    /// The connection lives on a thread of its own for the call. An error
    /// when PipeWire does not answer within `START_TIMEOUT`.
    pub fn inputs() -> Result<Vec<AudioDeviceInfo>, CaptureError> {
        let (answer, answered) = sync_channel(1);
        std::thread::Builder::new()
            .name("steno-pw-devs".into())
            .spawn(move || {
                let _ = answer.send(Self::read());
            })
            .map_err(|e| CaptureError::BackendFailed(format!("the PipeWire thread: {e}")))?;
        answered.recv_timeout(LIST_TIMEOUT).unwrap_or_else(|_| {
            Err(CaptureError::BackendFailed(
                "PipeWire did not list the devices".into(),
            ))
        })
    }

    /// One connection's read of the sources, on the calling thread.
    fn read() -> Result<Vec<AudioDeviceInfo>, CaptureError> {
        let deadline = Instant::now() + START_TIMEOUT;
        let connection = Connection::open(false)?;
        connection.roundtrip(deadline)?;
        Ok(connection.shared.graph.borrow().inputs())
    }
}

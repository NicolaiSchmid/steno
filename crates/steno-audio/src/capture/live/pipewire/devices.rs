//! The input picker's device list on Linux: the sources the capture can
//! record, read from the PipeWire registry over one short connection per
//! call. No Swift counterpart (the Swift app is macOS-only); the macOS
//! counterpart is `capture::live::devices::AudioDevices`, the Windows one
//! `capture::live::wasapi::AudioDevices`.

use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use super::{Connection, START_TIMEOUT};
use crate::capture::CaptureError;
use crate::capture::live::AudioDeviceInfo;

/// How long [`AudioDevices::inputs`] waits for its thread: the
/// connection's own deadline, [`START_TIMEOUT`], and a margin for a thread
/// that is slow to get scheduled.
const LIST_TIMEOUT: Duration = START_TIMEOUT.saturating_add(Duration::from_secs(1));

/// The PipeWire registry's sources, read fresh on every call. The names
/// match the other platforms' `AudioDevices` for the call they share,
/// `inputs`; errors are [`CaptureError`].
pub struct AudioDevices;

impl AudioDevices {
    /// Every source the microphone lane can record, as the capture's UID
    /// lookup accepts it: `Audio/Source` nodes, virtual sources (recorded
    /// through their monitor output) and duplex devices, by global id. `uid`
    /// is the `node.name` `Settings` stores, `name` the `node.description`
    /// (else `node.nick`, else the UID), and the default source, from the
    /// `default` metadata, is marked `is_default_input`. The rate is 0
    /// (the adapter resamples whatever the graph runs at) and the transport
    /// is the `media.class`; `is_running_somewhere` is not read.
    ///
    /// PipeWire's objects are single-threaded, so the connection lives on a
    /// thread of its own for the call: it connects, waits two roundtrips
    /// (the globals, then the `default` metadata's properties), and closes.
    /// An error when PipeWire is not running or does not answer within
    /// `START_TIMEOUT` (3 s); a thread that has not answered by then
    /// ends by its own deadline. The second roundtrip also means the
    /// metadata bind's ping was answered before the connection closes: the
    /// daemon holds the client's messages while it waits for the pong
    /// (`pw_impl_client_set_busy` in `global_bind`), so this call leaves no
    /// bind pending (see "A default move can go unreported" in the plan's
    /// Linux list) unless the session manager stalls past the deadline.
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
        let connection = Connection::open()?;
        connection.roundtrip(deadline)?;
        connection.roundtrip(deadline)?;
        Ok(connection.shared.graph.borrow().inputs())
    }
}

//! Audio sessions to process activity: the mapping under the Windows
//! [`LiveProcessAudioActivity`](super::LiveProcessAudioActivity) (WP10a),
//! pure so it is tested on every OS over synthetic sessions. No Swift
//! equivalent; the Core Audio HAL reports per-process input and output
//! flags directly.
//!
//! WASAPI has no per-process "is running input" flag. It has audio sessions
//! per endpoint (`IAudioSessionManager2` and `IAudioSessionEnumerator`), each
//! with a process id and a state. A process counts as running input when
//! one of its sessions on a capture endpoint is active, as running output
//! when one on a render endpoint is. The system-sounds session and pid 0
//! are skipped, expired sessions too (their stream is gone). The "bundle
//! id" on Windows is the executable's file name (`Teams.exe`), the closest
//! stable app identifier the session API leads to.

use std::collections::BTreeMap;

use super::activity::ProcessAudioActivity;

/// Which side of the engine an endpoint is on (`EDataFlow`): what a
/// session records and what the capture backend opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointFlow {
    /// A capture endpoint (a microphone).
    Capture,
    /// A render endpoint (speakers, headphones).
    Render,
}

/// `AudioSessionState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionState {
    /// The session has no running stream.
    Inactive,
    /// At least one stream of the session is running.
    Active,
    /// The session's streams are gone.
    Expired,
}

/// One audio session as the enumeration read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSessionRecord {
    /// `IAudioSessionControl2::GetProcessId`.
    pub pid: u32,
    /// The endpoint's data flow.
    pub flow: EndpointFlow,
    /// `IAudioSessionControl::GetState`.
    pub state: SessionState,
    /// `IAudioSessionControl2::IsSystemSoundsSession`.
    pub system_sounds: bool,
    /// The process's image path, when it could be queried.
    pub image_path: Option<String>,
}

/// Every process with a live session, in pid order, with its input and
/// output state and its executable's file name as the bundle id.
#[must_use]
pub fn processes_from_sessions(records: &[AudioSessionRecord]) -> Vec<ProcessAudioActivity> {
    let mut processes: BTreeMap<i32, ProcessAudioActivity> = BTreeMap::new();
    for record in records {
        if record.system_sounds || record.pid == 0 || record.state == SessionState::Expired {
            continue;
        }
        let Ok(pid) = i32::try_from(record.pid) else {
            continue;
        };
        let process = processes
            .entry(pid)
            .or_insert_with(|| ProcessAudioActivity::new(pid, None, false));
        if process.bundle_id.is_none() {
            process.bundle_id = record
                .image_path
                .as_deref()
                .and_then(image_file_name)
                .map(str::to_owned);
        }
        if record.state == SessionState::Active {
            match record.flow {
                EndpointFlow::Capture => process.is_running_input = true,
                EndpointFlow::Render => process.is_running_output = true,
            }
        }
    }
    processes.into_values().collect()
}

/// The file name of a Windows image path, either form
/// `QueryFullProcessImageNameW` returns (`C:\Program Files\App\App.exe`,
/// `\Device\HarddiskVolume3\...\App.exe`); `None` for an empty name.
#[must_use]
pub fn image_file_name(path: &str) -> Option<&str> {
    let name = path.rsplit(['\\', '/']).next()?.trim();
    (!name.is_empty()).then_some(name)
}

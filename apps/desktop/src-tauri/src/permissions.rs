//! The system permissions per OS, and [`ShellPermissions`], the host's
//! `Permissions` over them. On macOS the microphone and the calendar have
//! a TCC status and a prompt; system audio and the local network have no
//! status API and stay `unknown`. Linux and Windows expose nothing before
//! capture, so every permission is `unknown`, which the host counts as not
//! required.

pub use steno_bridge::{PermissionKind, PermissionState};

/// What TCC records for a permission, whichever framework reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
enum Tcc {
    NotDetermined,
    Restricted,
    Denied,
    Authorized,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl Tcc {
    /// The bridge's state: not determined is `unknown`, authorized (for the
    /// calendar, full access) is `granted`, anything else `denied`.
    fn state(self) -> PermissionState {
        match self {
            Tcc::NotDetermined => PermissionState::Unknown,
            Tcc::Restricted | Tcc::Denied => PermissionState::Denied,
            Tcc::Authorized => PermissionState::Granted,
        }
    }
}

#[cfg_attr(feature = "fixture-host", allow(dead_code))]
pub fn state(kind: PermissionKind) -> PermissionState {
    match kind {
        PermissionKind::Microphone => platform::microphone_state(),
        PermissionKind::Calendar => platform::calendar_state(),
        PermissionKind::SystemAudio | PermissionKind::LocalNetwork => PermissionState::Unknown,
    }
}

/// Prompts where the shell can raise a prompt, blocking until the user
/// answers; elsewhere the state as it stands.
#[cfg_attr(feature = "fixture-host", allow(dead_code))]
pub fn request(kind: PermissionKind) -> PermissionState {
    match kind {
        PermissionKind::Microphone => platform::request_microphone(),
        PermissionKind::Calendar => platform::request_calendar(),
        other => state(other),
    }
}

/// The settings pane for a permission; `None` where the OS has none.
pub fn system_settings_url(kind: PermissionKind) -> Option<String> {
    if cfg!(target_os = "macos") {
        let pane = match kind {
            PermissionKind::Microphone => "Privacy_Microphone",
            PermissionKind::SystemAudio => "Privacy_AudioCapture",
            PermissionKind::Calendar => "Privacy_Calendars",
            PermissionKind::LocalNetwork => "Privacy_LocalNetwork",
        };
        Some(format!(
            "x-apple.systempreferences:com.apple.preference.security?{pane}"
        ))
    } else if cfg!(target_os = "windows") {
        match kind {
            // Windows has one capture permission for both.
            PermissionKind::Microphone | PermissionKind::SystemAudio => {
                Some("ms-settings:privacy-microphone".into())
            }
            PermissionKind::Calendar => Some("ms-settings:privacy-calendar".into()),
            PermissionKind::LocalNetwork => None,
        }
    } else {
        None
    }
}

/// The host's `Permissions` over this module. The host calls `request`
/// with its lock released, on a thread that may wait for the user.
#[cfg(not(feature = "fixture-host"))]
pub struct ShellPermissions {
    pub app: tauri::AppHandle,
}

#[cfg(not(feature = "fixture-host"))]
impl steno_host::services::Permissions for ShellPermissions {
    fn state(&self, kind: PermissionKind) -> PermissionState {
        state(kind)
    }

    fn request(&self, kind: PermissionKind) -> PermissionState {
        request(kind)
    }

    fn open_system_settings(&self, kind: PermissionKind) {
        if let Some(url) = system_settings_url(kind)
            && let Err(error) = crate::dialogs::open_url(&self.app, &url)
        {
            tracing::warn!(?error, %kind, "the settings pane did not open");
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    //! TCC through `AVFoundation` and `steno_macos::calendar`. The access
    //! request's completion block runs on an arbitrary queue, so the
    //! answer crosses a channel back to the caller.

    use std::sync::Mutex;
    use std::sync::mpsc;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
    use steno_macos::calendar::{CalendarAccess, access, request_full_access};

    use super::{PermissionState, Tcc};

    fn media_type() -> &'static objc2_av_foundation::AVMediaType {
        // SAFETY: a constant string AVFoundation exports; present on every
        // macOS the shell targets.
        unsafe { AVMediaTypeAudio }.expect("AVMediaTypeAudio")
    }

    pub fn microphone_state() -> PermissionState {
        // SAFETY: a class method with no preconditions.
        let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type()) };
        match status {
            AVAuthorizationStatus::Authorized => Tcc::Authorized,
            AVAuthorizationStatus::Denied => Tcc::Denied,
            AVAuthorizationStatus::Restricted => Tcc::Restricted,
            _ => Tcc::NotDetermined,
        }
        .state()
    }

    pub fn request_microphone() -> PermissionState {
        let (sender, receiver) = mpsc::channel::<bool>();
        let sender = Mutex::new(Some(sender));
        let handler = RcBlock::new(move |granted: Bool| {
            if let Some(sender) = sender.lock().ok().and_then(|mut slot| slot.take()) {
                let _ = sender.send(granted.as_bool());
            }
        });
        // SAFETY: a class method; the block outlives the call because
        // AVFoundation copies it.
        unsafe {
            AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type(), &handler);
        };
        match receiver.recv() {
            Ok(true) => PermissionState::Granted,
            _ => PermissionState::Denied,
        }
    }

    pub fn calendar_state() -> PermissionState {
        match access() {
            CalendarAccess::FullAccess => Tcc::Authorized,
            CalendarAccess::Denied | CalendarAccess::WriteOnly => Tcc::Denied,
            CalendarAccess::Restricted => Tcc::Restricted,
            CalendarAccess::NotDetermined => Tcc::NotDetermined,
        }
        .state()
    }

    /// The prompt, then the state it left; a refusal or an error is
    /// `denied`.
    pub fn request_calendar() -> PermissionState {
        match request_full_access() {
            Ok(true) => PermissionState::Granted,
            Ok(false) => PermissionState::Denied,
            Err(error) => {
                tracing::warn!(%error, "the calendar access request failed");
                PermissionState::Denied
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    //! Nothing to query before capture, so the honest answer is `unknown`,
    //! not a `granted` the OS may contradict at the first recording.

    use super::PermissionState;

    pub fn microphone_state() -> PermissionState {
        PermissionState::Unknown
    }

    pub fn calendar_state() -> PermissionState {
        PermissionState::Unknown
    }

    pub fn request_microphone() -> PermissionState {
        PermissionState::Unknown
    }

    pub fn request_calendar() -> PermissionState {
        PermissionState::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four TCC statuses map as `Tcc::state` says.
    #[test]
    fn the_four_tcc_statuses_map_to_the_bridges_states() {
        assert_eq!(Tcc::NotDetermined.state(), PermissionState::Unknown);
        assert_eq!(Tcc::Restricted.state(), PermissionState::Denied);
        assert_eq!(Tcc::Denied.state(), PermissionState::Denied);
        assert_eq!(Tcc::Authorized.state(), PermissionState::Granted);
    }

    /// The tap and the local network have no status the shell can read.
    #[test]
    fn system_audio_and_the_local_network_are_never_the_shells() {
        assert_eq!(state(PermissionKind::SystemAudio), PermissionState::Unknown);
        assert_eq!(
            state(PermissionKind::LocalNetwork),
            PermissionState::Unknown
        );
    }

    #[test]
    fn linux_and_windows_know_nothing_ahead_of_capture() {
        if cfg!(target_os = "macos") {
            return;
        }
        for &kind in PermissionKind::ALL {
            assert_eq!(state(kind), PermissionState::Unknown, "{kind}");
            assert_eq!(request(kind), PermissionState::Unknown, "{kind}");
        }
    }

    #[test]
    fn the_settings_panes_are_per_os() {
        for &kind in PermissionKind::ALL {
            let url = system_settings_url(kind);
            if cfg!(target_os = "macos") {
                let url = url.unwrap();
                assert!(url.starts_with("x-apple.systempreferences:"), "{url}");
                assert!(url.contains("Privacy_"), "{url}");
            } else if cfg!(target_os = "windows") {
                assert_eq!(url.is_none(), kind == PermissionKind::LocalNetwork);
            } else {
                assert_eq!(url, None);
            }
        }
    }
}

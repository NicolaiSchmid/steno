//! The system permissions per OS, as far as each exposes them.
//!
//! macOS: the microphone has a TCC status and a prompt (`AVCaptureDevice`);
//! the system audio tap has no status API, so its state is what the audio
//! crate's probe last found (WP5 records it; until then `unknown`), and the
//! prompt is the probe itself; the calendar belongs to the host's calendar
//! service (`EventKit`) and the local network prompt appears on the first
//! Bonjour registration, so both stay `unknown` here. The usage strings the
//! prompts show are in `Info.plist` beside `tauri.conf.json`.
//!
//! Linux and Windows: no permission model the shell can query. `PipeWire`'s
//! portal and WASAPI grant at capture time (the portal shows its own dialog
//! then), so the microphone and system audio report `granted`; the
//! calendar and the local network stay `unknown`.
//!
//! Swift: `PermissionsService.swift`.
//!
//! The shell answers `system.openSystemSettings` from here today; the
//! states and the prompt are the host's to call for its onboarding and
//! Settings snapshots (`WP6b`), so the dead-code lint is off for them.
#![allow(dead_code)]

pub use steno_bridge::{PermissionKind, PermissionState};

/// The state without prompting.
pub fn state(kind: PermissionKind) -> PermissionState {
    match kind {
        PermissionKind::Microphone => platform::microphone_state(),
        PermissionKind::SystemAudio => platform::system_audio_state(),
        PermissionKind::Calendar | PermissionKind::LocalNetwork => PermissionState::Unknown,
    }
}

/// Prompts where the OS has a prompt the shell can raise; elsewhere the
/// state as it stands.
pub async fn request(kind: PermissionKind) -> PermissionState {
    match kind {
        PermissionKind::Microphone => platform::request_microphone().await,
        other => state(other),
    }
}

/// The settings pane for a permission, to open with the opener; `None`
/// where the OS has none the shell can address.
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

#[cfg(target_os = "macos")]
mod platform {
    //! TCC through `AVFoundation`. `requestAccessForMediaType:completionHandler:`
    //! shows the system prompt the first time and answers from the record
    //! after; the completion block runs on an arbitrary queue, so the answer
    //! crosses a channel back to the caller.

    use std::sync::Mutex;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

    use super::PermissionState;

    fn media_type() -> &'static objc2_av_foundation::AVMediaType {
        // SAFETY: a constant string AVFoundation exports; present on every
        // macOS the shell targets.
        unsafe { AVMediaTypeAudio }.expect("AVMediaTypeAudio")
    }

    pub fn microphone_state() -> PermissionState {
        // SAFETY: a class method with no preconditions.
        let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type()) };
        match status {
            AVAuthorizationStatus::Authorized => PermissionState::Granted,
            AVAuthorizationStatus::Denied | AVAuthorizationStatus::Restricted => {
                PermissionState::Denied
            }
            _ => PermissionState::Unknown,
        }
    }

    pub fn system_audio_state() -> PermissionState {
        // No status API; the audio crate's tap probe records the outcome
        // (WP5) and the host reads it. Until then: not yet determined.
        PermissionState::Unknown
    }

    pub async fn request_microphone() -> PermissionState {
        let (sender, receiver) = tokio::sync::oneshot::channel::<bool>();
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
        match receiver.await {
            Ok(true) => PermissionState::Granted,
            _ => PermissionState::Denied,
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    //! No permission model to query: `PipeWire`'s portal and WASAPI grant at
    //! capture time, so the recorder is never blocked ahead of it.

    use super::PermissionState;

    pub fn microphone_state() -> PermissionState {
        PermissionState::Granted
    }

    pub fn system_audio_state() -> PermissionState {
        PermissionState::Granted
    }

    /// `async` for the one signature across platforms; the macOS one awaits
    /// the TCC prompt.
    #[allow(clippy::unused_async)]
    pub async fn request_microphone() -> PermissionState {
        PermissionState::Granted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_calendar_and_the_local_network_are_never_the_shells() {
        assert_eq!(state(PermissionKind::Calendar), PermissionState::Unknown);
        assert_eq!(
            state(PermissionKind::LocalNetwork),
            PermissionState::Unknown
        );
    }

    #[test]
    fn linux_and_windows_grant_capture_ahead_of_time() {
        if cfg!(target_os = "macos") {
            return;
        }
        assert_eq!(state(PermissionKind::Microphone), PermissionState::Granted);
        assert_eq!(state(PermissionKind::SystemAudio), PermissionState::Granted);
        assert_eq!(
            tauri::async_runtime::block_on(request(PermissionKind::Microphone)),
            PermissionState::Granted
        );
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

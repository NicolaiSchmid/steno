//! The host's `Permissions` over the shell's (stable plan S3,
//! `.plans/2026-10-07-stable-promotion.md`). The shell answers what the
//! system says (`apps/desktop/src-tauri/src/permissions.rs`: the
//! microphone and calendar status and prompts on the Mac, `unknown`
//! elsewhere); [`SystemPermissions`] adds the one answer the system does
//! not give, the Mac's system audio permission, from the probe
//! (`steno_audio::capture::permission`) and what it last found.
//! Swift: `PermissionsService` in
//! `apps/macos/Steno/Services/PermissionsService.swift`.
//!
//! | Platform | System audio's state | Its request |
//! |----------|----------------------|-------------|
//! | macOS | `granted` once a probe heard the tone ([`SYSTEM_AUDIO_GRANTED_KEY`]), else `unknown` | the probe, up to 30 s; its answer is remembered |
//! | Linux, Windows | the shell's (`unknown`) | the shell's |
//!
//! What `unknown` means is the host's rule
//! (`steno_host::services::permission_is_missing`): missing on the Mac,
//! not required elsewhere. A probe that heard nothing answers `denied`
//! and forgets an earlier grant, so the next launch reads `unknown` and
//! onboarding offers the test recording again. The flag lives in this
//! app's `preferences.json` and is never copied from the Swift app: macOS
//! keeps the grant per bundle identifier, so the new identifier starts
//! without it.

use std::sync::Arc;

use steno_bridge::{PermissionKind, PermissionState, Platform};
use steno_host::services::{Permissions, Preferences};

/// The `preferences.json` flag set while the last system audio probe
/// heard the tone. Swift: `PermissionsService.systemAudioGrantedKey` in
/// `UserDefaults`.
pub const SYSTEM_AUDIO_GRANTED_KEY: &str = "steno.systemAudioGranted";

/// Runs the system audio probe and answers whether the tap heard the tone.
pub type SystemAudioProbe = Arc<dyn Fn() -> bool + Send + Sync>;

/// The shell's permissions with the Mac's system audio probe; see the
/// module doc.
pub struct SystemPermissions {
    system: Arc<dyn Permissions>,
    preferences: Arc<dyn Preferences>,
    probe: SystemAudioProbe,
    platform: Platform,
}

impl SystemPermissions {
    /// `system`, the shell's, on this platform, with this platform's probe
    /// and `preferences` for its answer.
    #[must_use]
    pub fn new(system: Arc<dyn Permissions>, preferences: Arc<dyn Preferences>) -> Self {
        Self::with_probe(system, preferences, system_probe(), Platform::CURRENT)
    }

    /// [`Self::new`] with the probe and the platform a test gives.
    #[must_use]
    pub fn with_probe(
        system: Arc<dyn Permissions>,
        preferences: Arc<dyn Preferences>,
        probe: SystemAudioProbe,
        platform: Platform,
    ) -> Self {
        Self {
            system,
            preferences,
            probe,
            platform,
        }
    }

    fn probes(&self, kind: PermissionKind) -> bool {
        kind == PermissionKind::SystemAudio && self.platform == Platform::Macos
    }
}

impl Permissions for SystemPermissions {
    fn state(&self, kind: PermissionKind) -> PermissionState {
        if !self.probes(kind) {
            return self.system.state(kind);
        }
        if self.preferences.flag(SYSTEM_AUDIO_GRANTED_KEY) {
            PermissionState::Granted
        } else {
            PermissionState::Unknown
        }
    }

    fn request(&self, kind: PermissionKind) -> PermissionState {
        if !self.probes(kind) {
            return self.system.request(kind);
        }
        let heard = (self.probe)();
        self.preferences.set_flag(SYSTEM_AUDIO_GRANTED_KEY, heard);
        if heard {
            PermissionState::Granted
        } else {
            PermissionState::Denied
        }
    }

    fn open_system_settings(&self, kind: PermissionKind) {
        self.system.open_system_settings(kind);
    }
}

/// The probe this platform has: the tap and a tone on the Mac, for
/// [`PROBE_TIMEOUT`](steno_audio::capture::permission::PROBE_TIMEOUT);
/// elsewhere none is ever run, and it answers `false`.
fn system_probe() -> SystemAudioProbe {
    #[cfg(target_os = "macos")]
    {
        Arc::new(|| {
            steno_audio::capture::permission::probe_system_audio(
                steno_audio::capture::permission::PROBE_TIMEOUT,
            )
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(|| false)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use steno_host::fakes::FakePermissions;

    use super::*;

    #[derive(Default)]
    struct Flags(Mutex<BTreeMap<String, bool>>);

    impl Preferences for Flags {
        fn stored_flag(&self, key: &str) -> Option<bool> {
            self.0.lock().unwrap().get(key).copied()
        }
        fn set_flag(&self, key: &str, value: bool) {
            self.0.lock().unwrap().insert(key.to_owned(), value);
        }
    }

    struct Probe {
        hears: AtomicBool,
        runs: AtomicUsize,
    }

    fn over(
        platform: Platform,
        hears: bool,
    ) -> (
        SystemPermissions,
        Arc<FakePermissions>,
        Arc<Flags>,
        Arc<Probe>,
    ) {
        let system = Arc::new(FakePermissions::all(PermissionState::Unknown));
        let flags = Arc::new(Flags::default());
        let probe = Arc::new(Probe {
            hears: AtomicBool::new(hears),
            runs: AtomicUsize::new(0),
        });
        let runs = probe.clone();
        let permissions = SystemPermissions::with_probe(
            system.clone(),
            flags.clone(),
            Arc::new(move || {
                runs.runs.fetch_add(1, Ordering::SeqCst);
                runs.hears.load(Ordering::SeqCst)
            }),
            platform,
        );
        (permissions, system, flags, probe)
    }

    /// On the Mac system audio is `unknown` until a probe hears the tone,
    /// `granted` after, and a probe that hears nothing is `denied` and
    /// forgets the grant.
    #[test]
    fn the_macs_system_audio_is_what_the_last_probe_heard() {
        let (permissions, _, flags, probe) = over(Platform::Macos, true);
        assert_eq!(
            permissions.state(PermissionKind::SystemAudio),
            PermissionState::Unknown
        );
        assert_eq!(
            permissions.request(PermissionKind::SystemAudio),
            PermissionState::Granted
        );
        assert!(flags.flag(SYSTEM_AUDIO_GRANTED_KEY));
        assert_eq!(
            permissions.state(PermissionKind::SystemAudio),
            PermissionState::Granted
        );

        probe.hears.store(false, Ordering::SeqCst);
        assert_eq!(
            permissions.request(PermissionKind::SystemAudio),
            PermissionState::Denied
        );
        assert_eq!(
            permissions.state(PermissionKind::SystemAudio),
            PermissionState::Unknown,
            "the next read offers the test recording again"
        );
        assert_eq!(probe.runs.load(Ordering::SeqCst), 2);
    }

    /// Everything else, and system audio off the Mac, is the shell's
    /// answer, and no probe runs.
    #[test]
    fn the_rest_is_the_shells() {
        for platform in [Platform::Macos, Platform::Linux, Platform::Windows] {
            let (permissions, system, flags, probe) = over(platform, true);
            system.set_state(PermissionKind::Microphone, PermissionState::Denied);
            system.set_answer(PermissionKind::Calendar, PermissionState::Granted);
            assert_eq!(
                permissions.state(PermissionKind::Microphone),
                PermissionState::Denied
            );
            assert_eq!(
                permissions.request(PermissionKind::Calendar),
                PermissionState::Granted
            );
            permissions.open_system_settings(PermissionKind::SystemAudio);
            assert_eq!(
                *system.opened_panes.lock().unwrap(),
                [PermissionKind::SystemAudio]
            );
            if platform != Platform::Macos {
                flags.set_flag(SYSTEM_AUDIO_GRANTED_KEY, true);
                assert_eq!(
                    permissions.state(PermissionKind::SystemAudio),
                    PermissionState::Unknown,
                    "{platform:?}"
                );
                permissions.request(PermissionKind::SystemAudio);
                assert_eq!(probe.runs.load(Ordering::SeqCst), 0, "{platform:?}");
            }
        }
    }
}

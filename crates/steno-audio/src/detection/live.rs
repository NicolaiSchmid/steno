//! The live [process-activity source](super::ProcessAudioActivitySource).
//! On macOS the HAL's process objects with their PID, bundle id and
//! `IsRunningInput` flag, and listeners on
//! `kAudioDevicePropertyDeviceIsRunningSomewhere` for every input device.
//! Swift: `LiveProcessAudioActivity` in
//! `Sources/StenoAudio/Detection/ProcessAudioActivity.swift`.
//!
//! On Windows the audio sessions of every active endpoint, mapped by
//! [`processes_from_sessions`](super::processes_from_sessions), with
//! endpoint and session notifications (WP10a, not run on hardware; see
//! `capture::live::wasapi`). No Swift counterpart.
//!
//! On Linux the PipeWire registry's stream nodes, their links and clients,
//! watched by one PipeWire thread per source (`super::pipewire`). On other
//! targets the type exists so callers compile and reports no processes.

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
use std::sync::mpsc::Receiver;

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
use super::activity::{ActivityError, ProcessAudioActivity, ProcessAudioActivitySource};

#[cfg(target_os = "linux")]
pub use super::pipewire::LiveProcessAudioActivity;
#[cfg(target_os = "macos")]
pub use macos::LiveProcessAudioActivity;
#[cfg(windows)]
pub use wasapi::LiveProcessAudioActivity;

/// The stub on other targets: lists no processes.
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
#[derive(Debug, Default)]
pub struct LiveProcessAudioActivity;

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
impl LiveProcessAudioActivity {
    /// The stub.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
impl ProcessAudioActivitySource for LiveProcessAudioActivity {
    fn snapshot(&self) -> Result<Vec<ProcessAudioActivity>, ActivityError> {
        Ok(Vec::new())
    }

    fn changes(&self) -> Receiver<()> {
        // No notifications; the detector's poll still runs.
        std::sync::mpsc::channel().1
    }
}

#[cfg(windows)]
mod wasapi {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use crate::capture::live::wasapi::com::{
        Apartment, ComError, Enumerator, SessionManagerRegistration, SessionRegistration,
    };
    use crate::detection::{
        ActivityError, EndpointFlow, ProcessAudioActivity, ProcessAudioActivitySource,
        processes_from_sessions,
    };

    impl From<ComError> for ActivityError {
        fn from(error: ComError) -> Self {
            ActivityError::Failed(error.to_string())
        }
    }

    /// One `changes()` call: the notification thread and its stop flag.
    struct Registration {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// The session-backed source. Each `changes()` call runs one thread
    /// that holds the COM registrations for as long as the source lives,
    /// or, once its receiver is dropped, until the next notification: a
    /// detector started many times holds that many threads until then.
    #[derive(Default)]
    pub struct LiveProcessAudioActivity {
        registrations: Mutex<Vec<Registration>>,
    }

    impl std::fmt::Debug for LiveProcessAudioActivity {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("LiveProcessAudioActivity")
                .finish_non_exhaustive()
        }
    }

    impl LiveProcessAudioActivity {
        /// No notifications registered until `changes()`.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }
    }

    /// What the notification callbacks tell the notification thread.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Notice {
        /// A session's state changed.
        Changed,
        /// Sessions may have appeared (one was created, or a device
        /// changed): register for every session's state again.
        Reregister,
    }

    fn notify(sender: &Sender<Notice>, notice: Notice) -> Box<dyn Fn() + Send + Sync> {
        let sender = sender.clone();
        Box::new(move || {
            let _ = sender.send(notice);
        })
    }

    /// The per-endpoint and per-session registrations on every active
    /// capture endpoint, made afresh after every session creation and
    /// every endpoint change.
    struct SessionWatch {
        _sessions: Vec<SessionRegistration>,
        _managers: Vec<SessionManagerRegistration>,
    }

    impl SessionWatch {
        fn register(enumerator: &Enumerator, sender: &Sender<Notice>) -> Self {
            let mut sessions = Vec::new();
            let mut managers = Vec::new();
            for endpoint in enumerator
                .active_endpoints(EndpointFlow::Capture)
                .unwrap_or_default()
            {
                let Ok(manager) = endpoint.session_manager() else {
                    continue;
                };
                // Enumerating first also makes the manager deliver
                // `OnSessionCreated` (see `SessionManager::sessions`).
                for session in manager.sessions().unwrap_or_default() {
                    if let Ok(registration) =
                        session.register_events(notify(sender, Notice::Changed))
                    {
                        sessions.push(registration);
                    }
                }
                if let Ok(registration) =
                    manager.register_created(notify(sender, Notice::Reregister))
                {
                    managers.push(registration);
                }
            }
            Self {
                _sessions: sessions,
                _managers: managers,
            }
        }
    }

    /// The notification thread: registers, forwards every notice to the
    /// detector, re-registers the session events when sessions appear or
    /// devices change, until stopped or the detector's receiver is gone.
    fn run_notifications(changes: &Sender<()>, stop: &AtomicBool) {
        let Ok(apartment) = Apartment::enter() else {
            return;
        };
        let Ok(enumerator) = Enumerator::new() else {
            return;
        };
        let (sender, notices) = channel();
        // A device change can bring capture endpoints with sessions.
        let endpoints = enumerator
            .register(notify(&sender, Notice::Reregister))
            .ok();
        let mut sessions = Some(SessionWatch::register(&enumerator, &sender));
        let _ = changes.send(());
        while !stop.load(Ordering::Acquire) {
            match notices.recv_timeout(Duration::from_millis(250)) {
                Ok(notice) => {
                    if notice == Notice::Reregister {
                        // Unregister before registering again.
                        drop(sessions.take());
                        sessions = Some(SessionWatch::register(&enumerator, &sender));
                    }
                    if changes.send(()).is_err() {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        drop(sessions);
        drop(endpoints);
        drop(enumerator);
        drop(apartment);
    }

    impl ProcessAudioActivitySource for LiveProcessAudioActivity {
        /// Every session on every active capture and render endpoint; an
        /// endpoint whose sessions cannot be read is skipped.
        fn snapshot(&self) -> Result<Vec<ProcessAudioActivity>, ActivityError> {
            let apartment = Apartment::enter()?;
            let enumerator = Enumerator::new()?;
            let mut records = Vec::new();
            for flow in [EndpointFlow::Capture, EndpointFlow::Render] {
                for endpoint in enumerator.active_endpoints(flow)? {
                    let Ok(manager) = endpoint.session_manager() else {
                        continue;
                    };
                    for session in manager.sessions().unwrap_or_default() {
                        records.extend(session.record(flow));
                    }
                }
            }
            drop(enumerator);
            drop(apartment);
            Ok(processes_from_sessions(&records))
        }

        /// One message per endpoint notification, session creation and
        /// capture-session state change, plus one right away.
        fn changes(&self) -> Receiver<()> {
            let (sender, receiver) = channel();
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = Arc::clone(&stop);
            let thread = std::thread::Builder::new()
                .name("steno-sessions".into())
                .spawn(move || run_notifications(&sender, &thread_stop))
                .map_err(|error| tracing::warn!("no session notifications: {error}"))
                .ok();
            self.registrations
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(Registration { stop, thread });
            receiver
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::Mutex;
    use std::sync::mpsc::{Receiver, Sender, channel};

    use objc2_core_audio::{
        kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioHardwarePropertyDevices,
        kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyScopeGlobal,
        kAudioProcessPropertyBundleID, kAudioProcessPropertyIsRunningInput,
        kAudioProcessPropertyIsRunningOutput, kAudioProcessPropertyPID,
    };

    use crate::capture::live::AudioDevices;
    use crate::capture::live::hal::{self, Id, ListenerHandler, PropertyListener, SYSTEM};
    use crate::detection::{ActivityError, ProcessAudioActivity, ProcessAudioActivitySource};

    /// Holds the listener registrations of every `changes()` call for as
    /// long as the source lives.
    #[derive(Default)]
    pub struct LiveProcessAudioActivity {
        registrations: Mutex<Vec<Registration>>,
    }

    /// Kept only for its drop: the listeners unregister with it.
    struct Registration {
        _fixed: Vec<PropertyListener>,
        _devices: Vec<PropertyListener>,
        _sender: Sender<()>,
    }

    impl std::fmt::Debug for LiveProcessAudioActivity {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("LiveProcessAudioActivity")
                .finish_non_exhaustive()
        }
    }

    impl LiveProcessAudioActivity {
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Every process object with its activity, in HAL order.
        pub fn process_objects() -> Result<Vec<Id>, ActivityError> {
            hal::read_array::<Id>(
                SYSTEM,
                kAudioHardwarePropertyProcessObjectList,
                kAudioObjectPropertyScopeGlobal,
            )
            .map_err(|e| ActivityError::Failed(e.to_string()))
        }

        #[must_use]
        pub fn activity(object: Id) -> Option<ProcessAudioActivity> {
            let pid: i32 = hal::read_pod(
                object,
                kAudioProcessPropertyPID,
                kAudioObjectPropertyScopeGlobal,
                None,
            )
            .ok()?;
            let bundle = hal::read_string(
                object,
                kAudioProcessPropertyBundleID,
                kAudioObjectPropertyScopeGlobal,
            )
            .ok()
            .filter(|b| !b.is_empty());
            Some(ProcessAudioActivity {
                pid,
                bundle_id: bundle,
                is_running_input: hal::read_bool(object, kAudioProcessPropertyIsRunningInput),
                is_running_output: hal::read_bool(object, kAudioProcessPropertyIsRunningOutput),
            })
        }

        fn device_listeners(sender: &Sender<()>) -> Vec<PropertyListener> {
            AudioDevices::inputs()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|device| {
                    PropertyListener::add(
                        device.id,
                        kAudioDevicePropertyDeviceIsRunningSomewhere,
                        kAudioObjectPropertyScopeGlobal,
                        notify(sender),
                    )
                    .ok()
                })
                .collect()
        }
    }

    /// A listener handler that fires `sender` once per notification.
    fn notify(sender: &Sender<()>) -> ListenerHandler {
        let sender = sender.clone();
        Box::new(move |_| {
            let _ = sender.send(());
        })
    }

    impl ProcessAudioActivitySource for LiveProcessAudioActivity {
        fn snapshot(&self) -> Result<Vec<ProcessAudioActivity>, ActivityError> {
            Ok(Self::process_objects()?
                .into_iter()
                .filter_map(Self::activity)
                .collect())
        }

        fn changes(&self) -> Receiver<()> {
            let (sender, receiver) = channel();
            // Input devices come and go: the device-list listener fires a
            // change, and `snapshot()` reads the processes afresh; the
            // per-device running listeners are rebuilt lazily on the next
            // `changes()` call rather than from inside a listener callback.
            let fixed: Vec<PropertyListener> = [
                kAudioHardwarePropertyProcessObjectList,
                kAudioHardwarePropertyDevices,
            ]
            .into_iter()
            .filter_map(|selector| {
                PropertyListener::add(
                    SYSTEM,
                    selector,
                    kAudioObjectPropertyScopeGlobal,
                    notify(&sender),
                )
                .ok()
            })
            .collect();
            let devices = Self::device_listeners(&sender);
            let _ = sender.send(());
            self.registrations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(Registration {
                    _fixed: fixed,
                    _devices: devices,
                    _sender: sender,
                });
            receiver
        }
    }
}

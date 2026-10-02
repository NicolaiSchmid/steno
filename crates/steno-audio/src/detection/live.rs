//! The HAL-backed [`ProcessAudioActivitySource`]: process objects with
//! their PID, bundle id and `IsRunningInput` flag, and listeners on
//! `kAudioDevicePropertyDeviceIsRunningSomewhere` for every input device.
//! Swift: `LiveProcessAudioActivity` in
//! `Sources/StenoAudio/Detection/ProcessAudioActivity.swift`.
//!
//! On Linux and Windows the type exists so callers compile and reports no
//! processes: PipeWire's node graph (WP5b) and WASAPI's session manager
//! (WP10) fill it in.

#[cfg(not(target_os = "macos"))]
use std::sync::mpsc::Receiver;

#[cfg(not(target_os = "macos"))]
use super::activity::{ActivityError, ProcessAudioActivity, ProcessAudioActivitySource};

#[cfg(target_os = "macos")]
pub use macos::LiveProcessAudioActivity;

#[cfg(not(target_os = "macos"))]
#[derive(Debug, Default)]
pub struct LiveProcessAudioActivity;

#[cfg(not(target_os = "macos"))]
impl LiveProcessAudioActivity {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(target_os = "macos"))]
impl ProcessAudioActivitySource for LiveProcessAudioActivity {
    fn snapshot(&self) -> Result<Vec<ProcessAudioActivity>, ActivityError> {
        Ok(Vec::new())
    }

    fn changes(&self) -> Receiver<()> {
        // No notifications; the detector's poll still runs.
        std::sync::mpsc::channel().1
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

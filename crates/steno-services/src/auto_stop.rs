//! The auto-stop after a call ends: a call recording during which another
//! app held the microphone stops on its own [`AUTO_STOP_GRACE`] after that
//! app released it, unless the microphone opens again or the user presses
//! "Keep recording". While the capture recovers the countdown ends nothing,
//! but a fresh one caps the wait at two graces after the release.

use std::time::Duration;

use steno_audio::clock::Cancel;
use steno_core::RecordingEndReason;
use steno_host::services::AutoStopStatus;

/// How long a call recording runs on after the call app closed the
/// microphone: long enough for a reconnect.
pub const AUTO_STOP_GRACE: Duration = Duration::from_secs(90);

/// Another app's microphone activity while Steno records, the app's bundle
/// id already resolved to a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicrophoneActivity {
    /// Another app opened the microphone.
    Opened {
        /// The app's name; `None` when it could not be named.
        app_name: Option<String>,
    },
    /// The app that held the microphone let go.
    Released,
}

/// The armed countdown.
#[derive(Debug)]
struct AutoStop {
    app_name: Option<String>,
    /// On the recorder's clock.
    deadline: Duration,
    cancel: Cancel,
    number: u64,
    /// The release armed it: while the capture recovers it is not shown, and a
    /// resume or its own end then puts a fresh, non-postponable one in its
    /// place, which ends the call whatever the capture does.
    postponable: bool,
}

/// What [`CallWatch::released`], [`CallWatch::resume`] or
/// [`CallWatch::postpone`] armed: the recorder sleeps [`AUTO_STOP_GRACE`]
/// unless `cancel` is raised, then calls back with `number`.
#[derive(Debug)]
pub(crate) struct Countdown {
    pub(crate) number: u64,
    pub(crate) cancel: Cancel,
}

/// The auto-stop policy of one recorder. The caller holds the recorder's
/// lock and checks its state: `opened` counts while a recording starts or
/// runs, `released`, `resume` and `postpone` only while a call records.
#[derive(Debug, Default)]
pub(crate) struct CallWatch {
    /// Another app held the microphone during this recording; only then does
    /// a release mean a call ended.
    saw_foreign: bool,
    /// The app the recording is attributed to: the prompt's, or the last
    /// one that opened the microphone.
    call_app: Option<String>,
    armed: Option<AutoStop>,
    /// How many countdowns were armed, so a callback that lost a race with a
    /// disarm finds it is not the armed one.
    arms: u64,
}

impl CallWatch {
    /// A recording starts; `call_app`, the app the detection prompt named,
    /// counts as a call already seen.
    pub(crate) fn begin(&mut self, call_app: Option<&str>) {
        self.reset();
        self.saw_foreign = call_app.is_some();
        self.call_app = call_app.map(str::to_owned);
    }

    /// Any stop: the countdown goes and the call is forgotten.
    pub(crate) fn reset(&mut self) {
        self.disarm();
        self.saw_foreign = false;
        self.call_app = None;
    }

    /// The app the recording is attributed to.
    pub(crate) fn call_app(&self) -> Option<&str> {
        self.call_app.as_deref()
    }

    /// Another app opened the microphone: a call is on, and a countdown goes.
    pub(crate) fn opened(&mut self, app_name: Option<String>) {
        self.saw_foreign = true;
        self.call_app = app_name;
        self.disarm();
    }

    /// The microphone was released while a call records: a postponable
    /// countdown armed at `now` when a call was seen and none is armed.
    pub(crate) fn released(&mut self, now: Duration) -> Option<Countdown> {
        if self.armed.is_some() {
            return None;
        }
        self.arm(now, true)
    }

    /// The capture is back after recovering: the release's countdown is
    /// replaced by a fresh one at `now`; one that replaced it already stays.
    pub(crate) fn resume(&mut self, now: Duration) -> Option<Countdown> {
        if !self.armed.as_ref().is_some_and(|armed| armed.postponable) {
            return None;
        }
        self.disarm();
        self.arm(now, false)
    }

    /// The countdown `number` ran out at `now`: while the capture is
    /// `recovering`, the release's countdown is replaced by a fresh one, the
    /// cap on the wait; else nothing, and [`Self::elapsed`] decides.
    pub(crate) fn postpone(
        &mut self,
        number: u64,
        now: Duration,
        recovering: bool,
    ) -> Option<Countdown> {
        let postponable = self
            .armed
            .as_ref()
            .is_some_and(|armed| armed.number == number && armed.postponable);
        if !(recovering && postponable) {
            return None;
        }
        self.disarm();
        self.arm(now, false)
    }

    /// Arms the countdown at `now`, when a call was seen.
    fn arm(&mut self, now: Duration, postponable: bool) -> Option<Countdown> {
        if !self.saw_foreign {
            return None;
        }
        self.arms += 1;
        let cancel = Cancel::new();
        self.armed = Some(AutoStop {
            app_name: self.call_app.clone(),
            deadline: now + AUTO_STOP_GRACE,
            cancel: cancel.clone(),
            number: self.arms,
            postponable,
        });
        Some(Countdown {
            number: self.arms,
            cancel,
        })
    }

    /// "Keep recording": the countdown goes until the next call is seen.
    /// `true` when it disarmed.
    pub(crate) fn keep_recording(&mut self) -> bool {
        if self.armed.is_none() {
            return false;
        }
        self.disarm();
        self.saw_foreign = false;
        true
    }

    /// The end reason to stop with, when `number` is still the armed
    /// countdown.
    pub(crate) fn elapsed(&mut self, number: u64) -> Option<RecordingEndReason> {
        let armed = self.armed.take_if(|armed| armed.number == number)?;
        Some(RecordingEndReason::CallEnded {
            app_name: armed.app_name,
        })
    }

    /// The armed countdown as the status shows it at `now`, the seconds left
    /// rounded up. None for the release's countdown while the capture is
    /// `recovering`.
    pub(crate) fn status(&self, now: Duration, recovering: bool) -> Option<AutoStopStatus> {
        self.armed
            .as_ref()
            .filter(|armed| !(recovering && armed.postponable))
            .map(|armed| AutoStopStatus {
                app_name: armed.app_name.clone(),
                remaining_seconds: armed.deadline.saturating_sub(now).as_secs_f64().ceil(),
                total_seconds: AUTO_STOP_GRACE.as_secs_f64(),
            })
    }

    fn disarm(&mut self) {
        if let Some(armed) = self.armed.take() {
            armed.cancel.cancel();
        }
    }
}

/// A dropped recorder lets its countdown's thread go.
impl Drop for CallWatch {
    fn drop(&mut self) {
        self.disarm();
    }
}

#[cfg(test)]
mod tests;

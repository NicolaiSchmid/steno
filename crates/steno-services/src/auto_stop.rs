//! The auto-stop after a call ends: a call recording during which another
//! app held the microphone stops on its own [`AUTO_STOP_GRACE`] after that
//! app released it, unless the microphone opens again (the call is back
//! on) or the user presses "Keep recording". In-person recordings and
//! calls nobody else joined never arm it.
//!
//! The policy is `CallWatch`, a value the recorder keeps under its lock and
//! feeds through
//! [`CaptureRecorder::microphone_activity`](crate::recorder::CaptureRecorder::microphone_activity).
//! The detection controller of [`crate::detection`] forwards the
//! detector's events there while a recording runs. When the grace runs
//! out, the recorder stops through the same stop as the Stop button and
//! saves the recording with [`RecordingEndReason::CallEnded`], a value the
//! Swift app's `RecordingEndReason` decodes; any other stop (Stop, quit, a
//! failure) disarms it first and keeps its own end reason.
//!
//! A capture that recovers from a lost device (coreaudiod restarting
//! empties the detector's process list too) must not end the call it is
//! keeping alive. `CallWatch` remembers that the call app let go, and
//! [`CaptureRecorder::resume_auto_stop`](crate::recorder::CaptureRecorder::resume_auto_stop),
//! once audio is delivered again, arms a fresh countdown when the app
//! still holds no microphone.
//!
//! Swift: the "Auto-stop after the call ends" part of
//! `apps/macos/Steno/Recording/RecordingController.swift` and
//! `apps/macos/Steno/Recording/AutoStop.swift`.

use std::time::Duration;

use steno_audio::clock::Cancel;
use steno_core::RecordingEndReason;
use steno_host::services::AutoStopStatus;

/// How long a call recording runs on after the call app closed the
/// microphone before it stops on its own: long enough for a reconnect.
/// Swift: `RecordingController.autoStopGrace`.
pub const AUTO_STOP_GRACE: Duration = Duration::from_secs(90);

/// Another app's microphone activity while Steno records, as the detection
/// controller forwards it, the app's bundle id already resolved to a name
/// (`None` when it could not be). Swift:
/// `RecordingController.MicrophoneActivity`.
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

/// The armed countdown. Swift: `AutoStop` (`AutoStop.swift`).
#[derive(Debug)]
struct AutoStop {
    app_name: Option<String>,
    /// On the recorder's clock.
    deadline: Duration,
    cancel: Cancel,
    number: u64,
}

/// What [`CallWatch::released`] or [`CallWatch::resume`] armed: the
/// recorder sleeps [`AUTO_STOP_GRACE`] on its clock unless `cancel` is
/// raised, then calls back with `number`. Swift: the `Countdown` of
/// `Countdown.swift`.
#[derive(Debug)]
pub(crate) struct Countdown {
    pub(crate) number: u64,
    pub(crate) cancel: Cancel,
}

/// The auto-stop policy of one recorder; see the module doc. The caller
/// holds the recorder's lock and checks the recorder's state: `opened`
/// counts while a recording starts or runs, `released` and `resume` only
/// while a call records. Swift: `RecordingController`'s `autoStop` and
/// `microphoneActivity`.
#[derive(Debug, Default)]
pub(crate) struct CallWatch {
    /// Another app held the microphone during this recording (the
    /// prompt's app at the start, or the first `opened` since); only then
    /// does a release mean a call ended.
    saw_foreign: bool,
    /// The call app let go of the microphone and has not opened it since,
    /// so [`Self::resume`] arms again. Rust only: the detector reports
    /// only changes, so a release a recovering capture set aside is not
    /// reported twice.
    released: bool,
    /// The app the recording is attributed to: the prompt's, or the last
    /// one that opened the microphone.
    call_app: Option<String>,
    armed: Option<AutoStop>,
    /// How many countdowns were armed, so a countdown's callback that
    /// lost a race with a disarm finds it is not the armed one.
    arms: u64,
}

impl CallWatch {
    /// A recording starts; `call_app` is the app the detection prompt
    /// named, which counts as a call already seen, so its first release
    /// arms without another `opened`.
    pub(crate) fn begin(&mut self, call_app: Option<&str>) {
        self.reset();
        self.saw_foreign = call_app.is_some();
        self.call_app = call_app.map(str::to_owned);
    }

    /// Any stop: the countdown goes and the call is forgotten.
    pub(crate) fn reset(&mut self) {
        self.disarm();
        self.saw_foreign = false;
        self.released = false;
        self.call_app = None;
    }

    /// The app the recording is attributed to.
    pub(crate) fn call_app(&self) -> Option<&str> {
        self.call_app.as_deref()
    }

    /// Another app opened the microphone while a recording starts or
    /// runs: a call is on, and a countdown goes.
    pub(crate) fn opened(&mut self, app_name: Option<String>) {
        self.saw_foreign = true;
        self.released = false;
        self.call_app = app_name;
        self.disarm();
    }

    /// The microphone was released while a call records: remembered, and
    /// the countdown armed at `now` when a call was seen and none is
    /// armed.
    pub(crate) fn released(&mut self, now: Duration) -> Option<Countdown> {
        self.released = true;
        if self.armed.is_some() {
            return None;
        }
        self.arm(now)
    }

    /// Audio is delivered again after the capture recovered: when the
    /// call app let go and has not opened the microphone since, a fresh
    /// countdown is armed at `now`, in place of one armed before (which
    /// may have run out while the capture recovered); else nothing.
    pub(crate) fn resume(&mut self, now: Duration) -> Option<Countdown> {
        if !self.released {
            return None;
        }
        self.disarm();
        self.arm(now)
    }

    /// Arms the countdown at `now`, when a call was seen.
    fn arm(&mut self, now: Duration) -> Option<Countdown> {
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
        });
        Some(Countdown {
            number: self.arms,
            cancel,
        })
    }

    /// "Keep recording": the countdown goes and does not come back until
    /// the next call is seen (`opened`, then `released`). Swift:
    /// `RecordingController.keepRecording`. A click that
    /// lands once nothing is armed (the app opened the microphone again, a
    /// double click) changes nothing; `true` when it disarmed.
    pub(crate) fn keep_recording(&mut self) -> bool {
        if self.armed.is_none() {
            return false;
        }
        self.disarm();
        self.saw_foreign = false;
        true
    }

    /// The countdown `number` ran out: the end reason to stop with, when it
    /// is still the armed one.
    pub(crate) fn elapsed(&mut self, number: u64) -> Option<RecordingEndReason> {
        let armed = self.armed.take_if(|armed| armed.number == number)?;
        Some(RecordingEndReason::CallEnded {
            app_name: armed.app_name,
        })
    }

    /// The armed countdown as the status shows it at `now`: the whole
    /// seconds left, rounded up as Swift's one-second ticks counted.
    pub(crate) fn status(&self, now: Duration) -> Option<AutoStopStatus> {
        self.armed.as_ref().map(|armed| AutoStopStatus {
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

#[cfg(test)]
mod tests;

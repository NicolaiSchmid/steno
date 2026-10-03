//! A scripted [`ProcessAudioActivitySource`] for the detector tests.
//! Swift: `Sources/StenoAudio/Testing/FakeProcessAudioActivity.swift`.
//!
//! `set` replaces the snapshot and fires a change, `set_silently` replaces
//! it without one (the poll must notice), `set_failure` makes `snapshot()`
//! fail.

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use crate::detection::{ActivityError, ProcessAudioActivity, ProcessAudioActivitySource};

/// A scripted [`ProcessAudioActivitySource`]: tests set the processes and
/// watch what the detector asks for.
#[derive(Debug, Default, Clone)]
pub struct FakeProcessAudioActivity {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    activities: Vec<ProcessAudioActivity>,
    listeners: Vec<Sender<()>>,
    snapshot_count: usize,
    failure: Option<String>,
}

impl FakeProcessAudioActivity {
    /// Starts with `initial`.
    #[must_use]
    pub fn new(initial: Vec<ProcessAudioActivity>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                activities: initial,
                ..Inner::default()
            })),
        }
    }

    /// How often the detector asked.
    #[must_use]
    pub fn snapshot_count(&self) -> usize {
        self.lock().snapshot_count
    }

    /// Makes `snapshot` fail with `failure` until cleared with `None`.
    pub fn set_failure(&self, failure: Option<&str>) {
        self.lock().failure = failure.map(str::to_owned);
    }

    /// Replaces the processes and notifies every listener.
    pub fn set(&self, activities: Vec<ProcessAudioActivity>) {
        let listeners = {
            let mut inner = self.lock();
            inner.activities = activities;
            inner.listeners.clone()
        };
        for listener in listeners {
            let _ = listener.send(());
        }
    }

    /// Replaces the processes without a notification; the poll must find it.
    pub fn set_silently(&self, activities: Vec<ProcessAudioActivity>) {
        self.lock().activities = activities;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ProcessAudioActivitySource for FakeProcessAudioActivity {
    fn snapshot(&self) -> Result<Vec<ProcessAudioActivity>, ActivityError> {
        let mut inner = self.lock();
        inner.snapshot_count += 1;
        match &inner.failure {
            Some(failure) => Err(ActivityError::Failed(failure.clone())),
            None => Ok(inner.activities.clone()),
        }
    }

    fn changes(&self) -> std::sync::mpsc::Receiver<()> {
        let (sender, receiver) = channel();
        self.lock().listeners.push(sender);
        receiver
    }
}

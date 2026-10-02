//! What a fake was asked to do.
//! Swift: `CallLog` in `Sources/StenoCore/Testing/FakeSpeech.swift`.

use std::sync::{Mutex, PoisonError};

/// Records what a fake was asked to do, from any thread. Fakes expose one
/// named for what it records (`transcriptions`, `summaries`, `admissions`).
#[derive(Debug)]
pub struct CallLog<Entry> {
    entries: Mutex<Vec<Entry>>,
}

impl<Entry> Default for CallLog<Entry> {
    fn default() -> Self {
        CallLog {
            entries: Mutex::new(Vec::new()),
        }
    }
}

impl<Entry> CallLog<Entry> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, entry: Entry) {
        self.lock().push(entry);
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.lock().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// A poisoned log is still a log: a fake that panicked mid-call keeps
    /// the entries recorded before.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<Entry: Clone> CallLog<Entry> {
    /// Every entry so far, in call order.
    #[must_use]
    pub fn entries(&self) -> Vec<Entry> {
        self.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_keeps_call_order() {
        let log = CallLog::new();
        assert!(log.is_empty());
        log.record("a");
        log.record("b");
        assert_eq!(log.count(), 2);
        assert_eq!(log.entries(), vec!["a", "b"]);
    }
}

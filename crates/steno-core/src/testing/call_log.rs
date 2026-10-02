//! What a fake was asked to do.
//! Swift: `CallLog` in `Sources/StenoCore/Testing/FakeSpeech.swift`.

use std::sync::Mutex;

use super::lock;

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
        lock(&self.entries).push(entry);
    }

    #[must_use]
    pub fn count(&self) -> usize {
        lock(&self.entries).len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        lock(&self.entries).is_empty()
    }
}

impl<Entry: Clone> CallLog<Entry> {
    /// Every entry so far, in call order.
    #[must_use]
    pub fn entries(&self) -> Vec<Entry> {
        lock(&self.entries).clone()
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

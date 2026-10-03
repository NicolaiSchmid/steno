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

    /// Appends `entry` and returns the count including it, so a fake that
    /// numbers its calls reads the number from the same lock.
    pub fn record(&self, entry: Entry) -> usize {
        let mut entries = lock(&self.entries);
        entries.push(entry);
        entries.len()
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
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// Panics on its first clone, inside the lock `entries()` holds, and
    /// clones quietly after that.
    struct Volatile(Arc<AtomicBool>);

    impl Clone for Volatile {
        fn clone(&self) -> Self {
            assert!(!self.0.swap(false, Ordering::SeqCst), "first clone");
            Volatile(Arc::clone(&self.0))
        }
    }

    #[test]
    fn a_poisoned_log_still_records_and_reads() {
        let log = CallLog::new();
        log.record(Volatile(Arc::new(AtomicBool::new(true))));
        let panicked = catch_unwind(AssertUnwindSafe(|| log.entries())).is_err();
        assert!(panicked);
        assert!(log.entries.is_poisoned());
        assert_eq!(log.count(), 1);
        assert_eq!(log.record(Volatile(Arc::new(AtomicBool::new(false)))), 2);
        assert_eq!(log.entries().len(), 2);
    }

    #[test]
    fn a_log_keeps_call_order() {
        let log = CallLog::new();
        assert!(log.is_empty());
        assert_eq!(log.record("a"), 1);
        assert_eq!(log.record("b"), 2);
        assert_eq!(log.count(), 2);
        assert_eq!(log.entries(), vec!["a", "b"]);
    }
}

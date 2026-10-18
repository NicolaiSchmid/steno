//! What a fake was asked to do.

use std::sync::Mutex;

use super::lock;

/// Records what a fake was asked to do, from any thread.
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

    /// Appends `entry` and returns the count including it, read under the same
    /// lock.
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

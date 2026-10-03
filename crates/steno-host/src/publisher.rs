//! The one publishing loop behind the host, after `Web/TopicPublisher.swift`:
//! owns the page's readiness, the coalescing and the throttle. A command
//! asks for the topics it changed ([`TopicPublisher::schedule`]); asks
//! before the host flushes fold into one publish per topic, in the order
//! the window publishes them; a topic with a minimum interval (`recording`
//! at 20 Hz) waits out the rest of its interval and the host flushes it on
//! its next [`TopicPublisher::take_due`]. Nothing is sent before
//! `page.ready`: the page has no `window.steno` yet, so the host builds
//! nothing and `page.ready` then publishes every topic once.
//!
//! Where Swift re-published through observation tracking (a change to
//! anything a snapshot read), the Rust host names the topics each command
//! changes; the snapshots stay full states, so arrival order only decides
//! which full state the page shows last.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use steno_bridge::BridgeTopic;

/// 20 Hz: the level meter's rate on the page. Swift: `recordingInterval`.
pub const RECORDING_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub struct TopicPublisher {
    /// Every topic the host publishes, in the order `page.ready` sends them.
    topics: Vec<BridgeTopic>,
    pending: BTreeSet<BridgeTopic>,
    page_ready: bool,
    minimum_interval: BTreeMap<BridgeTopic, Duration>,
    last_publish: BTreeMap<BridgeTopic, Instant>,
}

impl TopicPublisher {
    #[must_use]
    pub fn new(
        topics: Vec<BridgeTopic>,
        minimum_interval: BTreeMap<BridgeTopic, Duration>,
    ) -> Self {
        TopicPublisher {
            topics,
            pending: BTreeSet::new(),
            page_ready: false,
            minimum_interval,
            last_publish: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn topics(&self) -> &[BridgeTopic] {
        &self.topics
    }

    /// True from `page.ready`.
    #[must_use]
    pub fn is_page_ready(&self) -> bool {
        self.page_ready
    }

    /// Asks for a publish of `topic`; a second ask before the flush folds
    /// into the first.
    pub fn schedule(&mut self, topic: BridgeTopic) {
        if self.topics.contains(&topic) {
            self.pending.insert(topic);
        }
    }

    pub fn schedule_all(&mut self) {
        for topic in &self.topics {
            self.pending.insert(*topic);
        }
    }

    /// `page.ready`: from now on snapshots reach the sink, starting with
    /// every topic once, in order.
    pub fn page_did_become_ready(&mut self) {
        self.page_ready = true;
        self.schedule_all();
    }

    #[must_use]
    pub fn pending(&self) -> &BTreeSet<BridgeTopic> {
        &self.pending
    }

    /// The pending topics whose interval has passed at `now`, in publish
    /// order, stamped as published; a throttled topic stays pending until
    /// its interval is up.
    pub fn take_due(&mut self, now: Instant) -> Vec<BridgeTopic> {
        let mut due = Vec::new();
        for topic in &self.topics {
            if !self.pending.contains(topic) {
                continue;
            }
            if let (Some(interval), Some(last)) = (
                self.minimum_interval.get(topic),
                self.last_publish.get(topic),
            ) && now.duration_since(*last) < *interval
            {
                continue;
            }
            due.push(*topic);
        }
        for topic in &due {
            self.pending.remove(topic);
            if self.minimum_interval.contains_key(topic) {
                self.last_publish.insert(*topic, now);
            }
        }
        due
    }

    /// How long until the earliest throttled topic may go out; `None` when
    /// nothing is waiting on its interval.
    #[must_use]
    pub fn next_due(&self, now: Instant) -> Option<Duration> {
        self.pending
            .iter()
            .filter_map(|topic| {
                let interval = self.minimum_interval.get(topic)?;
                let last = self.last_publish.get(topic)?;
                Some(interval.saturating_sub(now.duration_since(*last)))
            })
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_fold_and_flush_in_window_order() {
        let mut publisher = TopicPublisher::new(
            vec![
                BridgeTopic::App,
                BridgeTopic::Recording,
                BridgeTopic::MeetingsList,
            ],
            BTreeMap::new(),
        );
        publisher.schedule(BridgeTopic::MeetingsList);
        publisher.schedule(BridgeTopic::App);
        publisher.schedule(BridgeTopic::App);
        publisher.schedule(BridgeTopic::Onboarding);
        assert_eq!(
            publisher.take_due(Instant::now()),
            vec![BridgeTopic::App, BridgeTopic::MeetingsList]
        );
        assert_eq!(
            publisher.take_due(Instant::now()),
            [] as [steno_bridge::BridgeTopic; 0]
        );
        publisher.page_did_become_ready();
        assert_eq!(publisher.take_due(Instant::now()).len(), 3);
    }

    #[test]
    fn a_throttled_topic_waits_out_its_interval() {
        let mut publisher = TopicPublisher::new(
            vec![BridgeTopic::Recording],
            BTreeMap::from([(BridgeTopic::Recording, RECORDING_INTERVAL)]),
        );
        let start = Instant::now();
        publisher.schedule(BridgeTopic::Recording);
        assert_eq!(publisher.take_due(start), vec![BridgeTopic::Recording]);
        publisher.schedule(BridgeTopic::Recording);
        assert_eq!(
            publisher.take_due(start + Duration::from_millis(10)),
            [] as [steno_bridge::BridgeTopic; 0]
        );
        assert_eq!(
            publisher.next_due(start + Duration::from_millis(10)),
            Some(Duration::from_millis(40))
        );
        assert_eq!(
            publisher.take_due(start + RECORDING_INTERVAL),
            vec![BridgeTopic::Recording]
        );
        assert_eq!(publisher.next_due(start + RECORDING_INTERVAL), None);
    }
}

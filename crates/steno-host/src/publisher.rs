//! The one publishing loop behind the host, after `Web/TopicPublisher.swift`:
//! owns the page's readiness and the coalescing. A command asks for the
//! topics it changed ([`TopicPublisher::schedule`]); asks before the host
//! publishes fold into one publish per topic, in the order the window
//! publishes them. Nothing is sent before `page.ready`: the page has no
//! `window.steno` yet, so the host builds nothing and `page.ready` then
//! publishes every topic once. The `recording` topic and its 20 Hz
//! throttle are not here: the host's
//! [`recording_feed`](crate::recording_feed) sends it.
//!
//! Where Swift re-published through observation tracking (a change to
//! anything a snapshot read), the Rust host names the topics each command
//! changes; the snapshots stay full states, so arrival order only decides
//! which full state the page shows last.

use std::collections::BTreeSet;

use steno_bridge::BridgeTopic;

#[derive(Debug)]
pub struct TopicPublisher {
    /// Every topic the publisher publishes, in the order `page.ready`
    /// sends them.
    topics: Vec<BridgeTopic>,
    pending: BTreeSet<BridgeTopic>,
    page_ready: bool,
}

impl TopicPublisher {
    #[must_use]
    pub fn new(topics: Vec<BridgeTopic>) -> Self {
        TopicPublisher {
            topics,
            pending: BTreeSet::new(),
            page_ready: false,
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

    /// Asks for a publish of `topic`; a second ask before the publish
    /// folds into the first. A topic the publisher does not publish is
    /// ignored.
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

    /// The pending topics in publish order, no longer pending.
    pub fn take_due(&mut self) -> Vec<BridgeTopic> {
        let due: Vec<BridgeTopic> = self
            .topics
            .iter()
            .filter(|topic| self.pending.contains(topic))
            .copied()
            .collect();
        self.pending.clear();
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_fold_and_flush_in_window_order() {
        let mut publisher = TopicPublisher::new(vec![
            BridgeTopic::App,
            BridgeTopic::Progress,
            BridgeTopic::MeetingsList,
        ]);
        publisher.schedule(BridgeTopic::MeetingsList);
        publisher.schedule(BridgeTopic::App);
        publisher.schedule(BridgeTopic::App);
        publisher.schedule(BridgeTopic::Onboarding);
        assert_eq!(
            publisher.take_due(),
            vec![BridgeTopic::App, BridgeTopic::MeetingsList]
        );
        assert_eq!(publisher.take_due(), [] as [BridgeTopic; 0]);
        publisher.page_did_become_ready();
        assert_eq!(publisher.take_due().len(), 3);
    }
}

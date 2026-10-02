//! The bus the pipeline posts [`MeetingEvent`]s on and the app subscribes
//! to. Swift: `MeetingEventBus`. A `tokio` broadcast channel: every
//! subscriber sees every event posted after it subscribed; a subscriber
//! that falls behind by more than the capacity skips the oldest events,
//! which the progress model tolerates (every event is a full state).

use steno_core::MeetingEvent;
use tokio::sync::broadcast;

/// The events of one pipeline (and whatever else posts on it).
#[derive(Debug, Clone)]
pub struct MeetingEventBus {
    sender: broadcast::Sender<MeetingEvent>,
}

/// One subscription; `recv().await` yields the next event.
pub type EventReceiver = broadcast::Receiver<MeetingEvent>;

impl Default for MeetingEventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl MeetingEventBus {
    /// Events kept for a slow subscriber before it starts skipping.
    pub const CAPACITY: usize = 1024;

    #[must_use]
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(Self::CAPACITY);
        MeetingEventBus { sender }
    }

    /// Delivers `event` to every current subscriber; nobody listening is
    /// not an error.
    pub fn post(&self, event: MeetingEvent) {
        let _ = self.sender.send(event);
    }

    #[must_use]
    pub fn subscribe(&self) -> EventReceiver {
        self.sender.subscribe()
    }

    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

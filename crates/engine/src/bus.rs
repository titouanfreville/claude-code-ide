//! The event bus — the **only** engine→UI channel (architecture: "the event bus
//! is the only engine→UI channel"). Backed by [`tokio::sync::broadcast`] so any
//! number of UI/observer tasks can subscribe to the same fan-out stream.

use tokio::sync::broadcast;

use crate::EngineEvent;

/// Fan-out channel carrying [`EngineEvent`] facts from the engine to every
/// subscribed consumer (the UI, audit sink, tests, …).
///
/// Cheaply cloneable: a clone shares the same underlying channel (the inner
/// [`broadcast::Sender`] is itself `Arc`-backed), so the supervisor and any
/// number of producers can each hold a handle.
///
/// **Drop-oldest semantics.** The channel is bounded by `capacity`. A subscriber
/// that falls behind by more than `capacity` messages does *not* stall or crash
/// the engine: the oldest undelivered messages are dropped and that receiver's
/// next `recv()` yields [`broadcast::error::RecvError::Lagged`] reporting how many
/// it missed. Consumers must tolerate gaps; the engine never blocks on a slow UI.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<EngineEvent>,
}

impl EventBus {
    /// Create a bus whose channel buffers up to `capacity` undelivered events per
    /// receiver before lagging (drop-oldest).
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publish a fact to all current subscribers.
    ///
    /// Returns the number of receivers the event reached. A send with no live
    /// receivers is *not* an error here — the engine publishes facts whether or
    /// not anyone is currently listening, so the lone `Err` case (no receivers)
    /// is intentionally collapsed to `0`.
    pub fn publish(&self, event: EngineEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// Subscribe a new receiver. It observes only events published *after* this
    /// call.
    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.tx.subscribe()
    }

    /// Number of live subscribers (useful for diagnostics/HUD).
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonlight_domain::ids::SessionId;
    use moonlight_domain::session::SessionStatus;

    fn sample(id: &str) -> EngineEvent {
        EngineEvent::SessionStateChanged {
            session: SessionId::new(id),
            status: SessionStatus::Running,
        }
    }

    #[tokio::test]
    async fn delivers_to_all_subscribers() {
        let bus = EventBus::new(8);
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();

        let reached = bus.publish(sample("s1"));
        assert_eq!(reached, 2);

        for rx in [&mut a, &mut b] {
            match rx.recv().await.expect("event delivered") {
                EngineEvent::SessionStateChanged { session, .. } => {
                    assert_eq!(session.as_str(), "s1");
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn publish_without_subscribers_is_not_fatal() {
        let bus = EventBus::new(4);
        assert_eq!(bus.publish(sample("s1")), 0);
    }

    #[tokio::test]
    async fn slow_subscriber_lags_instead_of_crashing() {
        let bus = EventBus::new(2);
        let mut rx = bus.subscribe();

        // Overflow the capacity-2 buffer; the engine keeps publishing regardless.
        for i in 0..5 {
            bus.publish(sample(&format!("s{i}")));
        }

        // The lagging receiver reports the gap rather than panicking the engine.
        match rx.recv().await {
            Err(broadcast::error::RecvError::Lagged(missed)) => assert!(missed >= 1),
            other => panic!("expected Lagged, got {other:?}"),
        }
        // …and it can still recover and read the most recent retained events.
        assert!(rx.recv().await.is_ok());
    }
}

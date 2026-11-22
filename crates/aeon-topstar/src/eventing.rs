use std::collections::{HashMap, VecDeque};

use thiserror::Error;
use uuid::Uuid;

use crate::{Event, Journal, PersistenceError};

pub type SubscriptionId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EventRecord {
    pub sequence: u64,
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFilter {
    pub event_type: Option<String>,
    pub actor: Option<Uuid>,
}

impl EventFilter {
    pub fn all() -> Self {
        Self {
            event_type: None,
            actor: None,
        }
    }

    pub fn matches(&self, event: &Event) -> bool {
        self.event_type
            .as_ref()
            .is_none_or(|event_type| event.event_type == *event_type)
            && self.actor.is_none_or(|actor| event.actor == actor)
    }
}

#[derive(Debug)]
struct Subscription {
    filter: EventFilter,
    queue: VecDeque<EventRecord>,
    capacity: usize,
}

#[derive(Debug, Error)]
pub enum EventingError {
    #[error("subscription not found: {0}")]
    SubscriptionNotFound(SubscriptionId),
    #[error("subscription queue is full: {0}")]
    QueueFull(SubscriptionId),
    #[error("subscription capacity must be greater than zero")]
    InvalidCapacity,
    #[error("event persistence failed: {0}")]
    Persistence(#[from] PersistenceError),
}

#[derive(Debug)]
pub struct EventBroker {
    subscriptions: HashMap<SubscriptionId, Subscription>,
    sequence: u64,
    journal: Option<Journal>,
}

impl Default for EventBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBroker {
    pub fn new() -> Self {
        Self {
            subscriptions: HashMap::new(),
            sequence: 0,
            journal: None,
        }
    }

    pub fn with_journal(journal: Journal) -> Result<Self, EventingError> {
        let sequence = journal.next_sequence();
        Ok(Self {
            subscriptions: HashMap::new(),
            sequence,
            journal: Some(journal),
        })
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn subscribe(
        &mut self,
        filter: EventFilter,
        capacity: usize,
    ) -> Result<SubscriptionId, EventingError> {
        if capacity == 0 {
            return Err(EventingError::InvalidCapacity);
        }

        let id = Uuid::new_v4();
        self.subscriptions.insert(
            id,
            Subscription {
                filter,
                queue: VecDeque::with_capacity(capacity),
                capacity,
            },
        );
        Ok(id)
    }

    pub fn unsubscribe(&mut self, id: SubscriptionId) -> Result<(), EventingError> {
        self.subscriptions
            .remove(&id)
            .map(|_| ())
            .ok_or(EventingError::SubscriptionNotFound(id))
    }

    pub fn publish(&mut self, event: Event) -> Result<EventRecord, EventingError> {
        let record = EventRecord {
            sequence: self.sequence,
            event,
        };

        for (id, subscription) in &self.subscriptions {
            if subscription.filter.matches(&record.event)
                && subscription.queue.len() >= subscription.capacity
            {
                return Err(EventingError::QueueFull(*id));
            }
        }

        if let Some(journal) = self.journal.as_mut() {
            journal.append(&record)?;
        }

        for subscription in self.subscriptions.values_mut() {
            if !subscription.filter.matches(&record.event) {
                continue;
            }
            subscription.queue.push_back(record.clone());
        }

        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("event sequence exhausted");

        Ok(record)
    }

    pub fn poll(
        &mut self,
        id: SubscriptionId,
        limit: usize,
    ) -> Result<Vec<EventRecord>, EventingError> {
        let subscription = self
            .subscriptions
            .get_mut(&id)
            .ok_or(EventingError::SubscriptionNotFound(id))?;

        let count = limit.min(subscription.queue.len());
        Ok(subscription.queue.drain(..count).collect())
    }

    pub fn pending(&self, id: SubscriptionId) -> Result<usize, EventingError> {
        self.subscriptions
            .get(&id)
            .map(|subscription| subscription.queue.len())
            .ok_or(EventingError::SubscriptionNotFound(id))
    }

    pub fn replay<T>(&self) -> Result<Vec<crate::JournalRecord<T>>, EventingError>
    where
        T: serde::de::DeserializeOwned,
    {
        self.journal
            .as_ref()
            .map(Journal::read_all::<T>)
            .transpose()?
            .ok_or(EventingError::Persistence(PersistenceError::InvalidHeader))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;

    fn event(event_type: &str) -> Event {
        Event::new(
            Identity::new("agent").id,
            None,
            event_type,
            1,
            serde_json::json!({"value": 1}),
        )
    }

    #[test]
    fn publishes_only_matching_events() {
        let mut broker = EventBroker::new();
        let subscription = broker
            .subscribe(
                EventFilter {
                    event_type: Some("action.completed".into()),
                    actor: None,
                },
                4,
            )
            .expect("subscription should succeed");

        broker
            .publish(event("action.started"))
            .expect("publish should succeed");
        broker
            .publish(event("action.completed"))
            .expect("publish should succeed");

        let records = broker.poll(subscription, 10).expect("poll should succeed");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].event.event_type, "action.completed");
        assert_eq!(records[0].sequence, 1);
    }

    #[test]
    fn queue_backpressure_is_explicit() {
        let mut broker = EventBroker::new();
        let subscription = broker
            .subscribe(EventFilter::all(), 1)
            .expect("subscription should succeed");

        broker
            .publish(event("first"))
            .expect("first publish should succeed");
        assert!(matches!(
            broker.publish(event("second")),
            Err(EventingError::QueueFull(id)) if id == subscription
        ));
    }

    #[test]
    fn unsubscribe_removes_subscription() {
        let mut broker = EventBroker::new();
        let subscription = broker
            .subscribe(EventFilter::all(), 1)
            .expect("subscription should succeed");
        broker
            .unsubscribe(subscription)
            .expect("unsubscribe should succeed");
        assert!(matches!(
            broker.pending(subscription),
            Err(EventingError::SubscriptionNotFound(id)) if id == subscription
        ));
    }
}

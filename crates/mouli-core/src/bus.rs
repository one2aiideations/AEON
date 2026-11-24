//! In-process event bus with topic prefixes and bounded subscriber queues.
//!
//! Slow subscribers lose their oldest events (counted) rather than block
//! publishers.

use std::collections::{BTreeMap, VecDeque};

/// An event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Dotted topic, e.g. "files.created".
    pub topic: String,
    /// Payload.
    pub data: String,
    /// Publish sequence number.
    pub seq: u64,
}

/// Subscriber handle.
pub type SubId = u32;

#[derive(Debug, Clone)]
struct Sub {
    prefix: String,
    queue: VecDeque<Event>,
    dropped: u64,
}

/// The bus.
#[derive(Debug, Clone)]
pub struct EventBus {
    subs: BTreeMap<SubId, Sub>,
    next_sub: SubId,
    seq: u64,
    capacity: usize,
}

fn matches(prefix: &str, topic: &str) -> bool {
    prefix.is_empty()
        || topic == prefix
        || topic
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.'))
}

impl EventBus {
    /// Bus whose subscriber queues hold `capacity` events.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            subs: BTreeMap::new(),
            next_sub: 1,
            seq: 0,
            capacity: capacity.max(1),
        }
    }

    /// Subscribe to `prefix` ("" = everything, "files" = "files" and "files.*").
    pub fn subscribe(&mut self, prefix: &str) -> SubId {
        let id = self.next_sub;
        self.next_sub += 1;
        self.subs.insert(
            id,
            Sub {
                prefix: prefix.to_owned(),
                queue: VecDeque::new(),
                dropped: 0,
            },
        );
        id
    }

    /// Remove a subscriber.
    pub fn unsubscribe(&mut self, id: SubId) -> bool {
        self.subs.remove(&id).is_some()
    }

    /// Publish; returns how many subscribers received it.
    pub fn publish(&mut self, topic: &str, data: &str) -> usize {
        self.seq += 1;
        let ev = Event {
            topic: topic.to_owned(),
            data: data.to_owned(),
            seq: self.seq,
        };
        let mut n = 0;
        for s in self.subs.values_mut().filter(|s| matches(&s.prefix, topic)) {
            if s.queue.len() == self.capacity {
                s.queue.pop_front();
                s.dropped += 1;
            }
            s.queue.push_back(ev.clone());
            n += 1;
        }
        n
    }

    /// Take all queued events for `id`.
    pub fn drain(&mut self, id: SubId) -> Vec<Event> {
        self.subs
            .get_mut(&id)
            .map(|s| s.queue.drain(..).collect())
            .unwrap_or_default()
    }

    /// Events dropped for `id` because its queue was full.
    #[must_use]
    pub fn dropped(&self, id: SubId) -> u64 {
        self.subs.get(&id).map_or(0, |s| s.dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_routing_and_overflow() {
        let mut bus = EventBus::new(2);
        let files = bus.subscribe("files");
        let all = bus.subscribe("");
        assert_eq!(bus.publish("files.created", "a.txt"), 2);
        assert_eq!(bus.publish("filesystem.full", "/"), 1);
        assert_eq!(bus.publish("files", "root"), 2);
        assert_eq!(bus.publish("net.up", "wlan0"), 1);
        let f: Vec<String> = bus.drain(files).into_iter().map(|e| e.data).collect();
        assert_eq!(f, vec!["a.txt", "root"]);
        assert_eq!(bus.drain(all).len(), 2);
        assert_eq!(bus.dropped(all), 2);
        assert!(bus.unsubscribe(files));
        assert!(bus.drain(files).is_empty());
    }
}

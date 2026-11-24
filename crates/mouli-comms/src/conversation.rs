//! Ordered, de-duplicated conversations with delivery receipts.
//!
//! Each device keeps a Lamport clock. Messages sort by (clock, sender,
//! id), so every device shows the same order regardless of arrival order.

use std::collections::BTreeMap;

/// Device or user id.
pub type PeerId = u32;

/// Delivery state, as seen by the sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Delivery {
    /// Queued locally.
    Pending,
    /// Accepted by the transport.
    Sent,
    /// Stored on the recipient's device.
    Delivered,
    /// Shown to the recipient.
    Read,
}

/// A message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Globally unique id (sender, per-sender counter).
    pub id: (PeerId, u64),
    /// Lamport timestamp.
    pub clock: u64,
    /// Author.
    pub sender: PeerId,
    /// Text body.
    pub body: String,
    /// Delivery state (meaningful for own messages).
    pub delivery: Delivery,
}

/// One conversation on one device.
#[derive(Debug, Clone)]
pub struct Conversation {
    me: PeerId,
    clock: u64,
    next_local: u64,
    messages: BTreeMap<(u64, PeerId, u64), Message>,
    index: BTreeMap<(PeerId, u64), (u64, PeerId, u64)>,
}

impl Conversation {
    /// Conversation as seen by `me`.
    #[must_use]
    pub fn new(me: PeerId) -> Self {
        Self {
            me,
            clock: 0,
            next_local: 0,
            messages: BTreeMap::new(),
            index: BTreeMap::new(),
        }
    }

    fn insert(&mut self, m: Message) -> bool {
        if self.index.contains_key(&m.id) {
            return false;
        }
        let key = (m.clock, m.sender, m.id.1);
        self.index.insert(m.id, key);
        self.messages.insert(key, m);
        true
    }

    /// Compose a message; returns the copy to send.
    pub fn compose(&mut self, body: &str) -> Message {
        self.clock += 1;
        let m = Message {
            id: (self.me, self.next_local),
            clock: self.clock,
            sender: self.me,
            body: body.to_owned(),
            delivery: Delivery::Pending,
        };
        self.next_local += 1;
        self.insert(m.clone());
        m
    }

    /// Accept a message from the network. Returns false for duplicates.
    pub fn receive(&mut self, mut m: Message) -> bool {
        self.clock = self.clock.max(m.clock) + 1;
        m.delivery = Delivery::Delivered;
        self.insert(m)
    }

    /// Update an own message's delivery state (never moves backwards).
    pub fn mark(&mut self, id: (PeerId, u64), state: Delivery) -> bool {
        let Some(key) = self.index.get(&id) else {
            return false;
        };
        match self.messages.get_mut(key) {
            Some(m) if m.sender == self.me && state > m.delivery => {
                m.delivery = state;
                true
            }
            _ => false,
        }
    }

    /// Own messages not yet accepted by the transport (the outbox).
    pub fn outbox(&self) -> impl Iterator<Item = &Message> {
        self.messages
            .values()
            .filter(move |m| m.sender == self.me && m.delivery == Delivery::Pending)
    }

    /// Messages in display order.
    pub fn timeline(&self) -> impl Iterator<Item = &Message> {
        self.messages.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_order_on_both_devices() {
        let mut a = Conversation::new(1);
        let mut b = Conversation::new(2);
        let a1 = a.compose("hi");
        let b1 = b.compose("hello"); // concurrent with a1
        let a2 = a.compose("how are you?");
        // Deliver in different orders.
        assert!(b.receive(a2.clone()));
        assert!(b.receive(a1.clone()));
        assert!(a.receive(b1.clone()));
        let order = |c: &Conversation| c.timeline().map(|m| m.body.clone()).collect::<Vec<_>>();
        assert_eq!(order(&a), order(&b));
        assert!(!b.receive(a1), "duplicate ignored");
        let b2 = b.compose("fine");
        assert!(b2.clock > a2.clock, "reply sorts after what was seen");
    }

    #[test]
    fn receipts_and_outbox() {
        let mut a = Conversation::new(1);
        let m = a.compose("x");
        assert_eq!(a.outbox().count(), 1);
        assert!(a.mark(m.id, Delivery::Sent));
        assert_eq!(a.outbox().count(), 0);
        assert!(a.mark(m.id, Delivery::Read));
        assert!(!a.mark(m.id, Delivery::Delivered), "never backwards");
        assert!(!a.mark((9, 9), Delivery::Read));
    }
}

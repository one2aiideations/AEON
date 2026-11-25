//! The play queue: what comes next, what came before, shuffle and repeat.

use crate::source::Source;

/// Most items the queue holds.
pub const MAX_QUEUE: usize = 1000;
/// Longest title kept, in characters.
pub const MAX_TITLE: usize = 120;

/// One thing in the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// What to play.
    pub source: Source,
    /// A name to show; the source's own label is used when there is none.
    pub title: Option<String>,
    /// Move on after this many seconds even if the item is longer (a preview). `None` plays it all.
    pub max_secs: Option<u32>,
}

impl Item {
    /// An item that plays to the end.
    pub fn new(source: Source) -> Self {
        Self {
            source,
            title: None,
            max_secs: None,
        }
    }

    /// The name to show.
    pub fn name(&self) -> String {
        self.title.clone().unwrap_or_else(|| self.source.label())
    }
}

/// What happens at the end of the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    /// Stop after the last item.
    Off,
    /// Play the current item again.
    One,
    /// Start again from the beginning.
    All,
}

/// The queue and the position in it.
#[derive(Debug, Clone)]
pub struct Queue {
    items: Vec<Item>,
    /// Item indexes in play order.
    order: Vec<usize>,
    /// Position in `order`.
    pos: Option<usize>,
    repeat: Repeat,
    shuffle: bool,
    seed: u64,
}

impl Default for Queue {
    fn default() -> Self {
        Self::with_seed(0x9E37_79B9_7F4A_7C15)
    }
}

impl Queue {
    /// An empty queue whose shuffle starts from `seed` (the same seed gives the same order).
    pub fn with_seed(seed: u64) -> Self {
        Self {
            items: Vec::new(),
            order: Vec::new(),
            pos: None,
            repeat: Repeat::Off,
            shuffle: false,
            seed: seed | 1,
        }
    }

    fn rand(&mut self, below: usize) -> usize {
        // xorshift64: small, fast and good enough for shuffling a playlist.
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        (self.seed % below.max(1) as u64) as usize
    }

    /// Number of items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when there are no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The items in the order they were added.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// The repeat mode.
    pub fn repeat(&self) -> Repeat {
        self.repeat
    }

    /// Sets the repeat mode.
    pub fn set_repeat(&mut self, r: Repeat) {
        self.repeat = r;
    }

    /// True when shuffle is on.
    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    /// Adds an item at the end. Returns false when the queue is full.
    pub fn push(&mut self, mut item: Item) -> bool {
        if self.items.len() >= MAX_QUEUE {
            return false;
        }
        if let Some(t) = &item.title {
            item.title = Some(
                t.chars()
                    .filter(|c| !c.is_control())
                    .take(MAX_TITLE)
                    .collect(),
            );
        }
        self.items.push(item);
        let idx = self.items.len() - 1;
        if self.shuffle && !self.order.is_empty() {
            // A new item lands somewhere after the current one.
            let from = self.pos.map_or(0, |p| p + 1);
            let at = from + self.rand(self.order.len() - from + 1);
            self.order.insert(at.min(self.order.len()), idx);
        } else {
            self.order.push(idx);
        }
        true
    }

    /// Turns shuffle on or off. The current item stays current; the rest are reordered.
    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        self.shuffle = on;
        let current = self.current_index();
        self.rebuild_order(current);
    }

    fn rebuild_order(&mut self, keep_first: Option<usize>) {
        let mut idx: Vec<usize> = (0..self.items.len()).collect();
        if self.shuffle {
            // Fisher-Yates over everything except the current item, which goes first.
            if let Some(k) = keep_first {
                idx.retain(|&i| i != k);
            }
            for i in (1..idx.len()).rev() {
                let j = self.rand(i + 1);
                idx.swap(i, j);
            }
            if let Some(k) = keep_first {
                idx.insert(0, k);
            }
            self.pos = keep_first.map(|_| 0);
        } else {
            self.pos = keep_first.and_then(|k| idx.iter().position(|&i| i == k));
        }
        self.order = idx;
    }

    fn current_index(&self) -> Option<usize> {
        self.pos.and_then(|p| self.order.get(p).copied())
    }

    /// The item being played, if any.
    pub fn current(&self) -> Option<&Item> {
        self.current_index().and_then(|i| self.items.get(i))
    }

    /// The position of the current item in the order it was added.
    pub fn current_item_index(&self) -> Option<usize> {
        self.current_index()
    }

    /// Makes the first item in play order current (used to start).
    pub fn start(&mut self) -> Option<&Item> {
        if self.order.is_empty() {
            self.pos = None;
        } else {
            self.pos = Some(0);
        }
        self.current()
    }

    /// Makes item number `i` (in the order added) current.
    pub fn jump(&mut self, i: usize) -> Option<&Item> {
        let p = self.order.iter().position(|&x| x == i)?;
        self.pos = Some(p);
        self.current()
    }

    /// Moves to the next item that `skip` does not reject, wrapping when repeat-all is on.
    /// Returns `None` at the end (the position is then left on the last item played), or when
    /// every item is rejected.
    pub fn next(&mut self, skip: &dyn Fn(&Item) -> bool) -> Option<&Item> {
        let n = self.order.len();
        if n == 0 {
            return None;
        }
        let mut p = self.pos;
        for _ in 0..n {
            let candidate = match p {
                None => 0,
                Some(x) => x + 1,
            };
            let candidate = if candidate >= n {
                if self.repeat == Repeat::All {
                    if self.shuffle {
                        // A new round in a new order, never starting with the item just played.
                        let last = self.current_index();
                        self.rebuild_order(None);
                        if n > 1 && self.order.first().copied() == last {
                            self.order.swap(0, 1);
                        }
                    }
                    0
                } else {
                    return None;
                }
            } else {
                candidate
            };
            p = Some(candidate);
            let idx = self.order[candidate];
            if !skip(&self.items[idx]) {
                self.pos = p;
                return self.current();
            }
        }
        None
    }

    /// Moves back one item (stays put at the start unless repeat-all wraps to the end).
    pub fn prev(&mut self) -> Option<&Item> {
        let n = self.order.len();
        if n == 0 {
            return None;
        }
        let p = match self.pos {
            Some(0) | None => {
                if self.repeat == Repeat::All {
                    n - 1
                } else {
                    return None;
                }
            }
            Some(x) => x - 1,
        };
        self.pos = Some(p);
        self.current()
    }

    /// Removes item number `i` (in the order added).
    pub fn remove(&mut self, i: usize) -> bool {
        if i >= self.items.len() {
            return false;
        }
        let was_current = self.current_index() == Some(i);
        let cur = if was_current {
            None
        } else {
            self.current_index()
        };
        self.items.remove(i);
        self.order.retain(|&x| x != i);
        for x in &mut self.order {
            if *x > i {
                *x -= 1;
            }
        }
        self.pos = match cur {
            Some(c) => {
                let c = if c > i { c - 1 } else { c };
                self.order.iter().position(|&x| x == c)
            }
            None => None,
        };
        true
    }

    /// Empties the queue.
    pub fn clear(&mut self) {
        self.items.clear();
        self.order.clear();
        self.pos = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{Source, YtKind};

    fn item(n: usize) -> Item {
        Item::new(Source::File(format!("/m/{n}.mp4")))
    }

    fn short(n: usize) -> Item {
        Item::new(Source::YouTube {
            id: format!("{n:011}"),
            kind: YtKind::Short,
        })
    }

    fn name(q: &Queue) -> String {
        q.current().map(|i| i.source.label()).unwrap_or_default()
    }

    fn never(_: &Item) -> bool {
        false
    }

    fn queue(n: usize) -> Queue {
        let mut q = Queue::default();
        for i in 0..n {
            q.push(item(i));
        }
        q
    }

    #[test]
    fn playing_goes_in_order_and_stops_at_the_end() {
        let mut q = queue(3);
        assert!(q.current().is_none());
        q.start();
        assert_eq!(name(&q), "0.mp4");
        assert_eq!(q.next(&never).unwrap().source.label(), "1.mp4");
        assert_eq!(q.next(&never).unwrap().source.label(), "2.mp4");
        assert!(q.next(&never).is_none());
        assert_eq!(name(&q), "2.mp4", "stays on the last item");
    }

    #[test]
    fn repeat_all_wraps_and_repeat_off_does_not() {
        let mut q = queue(2);
        q.start();
        q.next(&never);
        q.set_repeat(Repeat::All);
        assert_eq!(q.next(&never).unwrap().source.label(), "0.mp4");
        q.set_repeat(Repeat::Off);
        q.next(&never);
        assert!(q.next(&never).is_none());
    }

    #[test]
    fn previous_goes_back_and_stops_at_the_start() {
        let mut q = queue(3);
        q.start();
        assert!(q.prev().is_none());
        q.next(&never);
        q.next(&never);
        assert_eq!(q.prev().unwrap().source.label(), "1.mp4");
        assert_eq!(q.prev().unwrap().source.label(), "0.mp4");
        assert!(q.prev().is_none());
        q.set_repeat(Repeat::All);
        assert_eq!(q.prev().unwrap().source.label(), "2.mp4");
    }

    #[test]
    fn rejected_items_are_skipped_and_all_rejected_means_none() {
        let mut q = Queue::default();
        q.push(short(1));
        q.push(item(2));
        q.push(short(3));
        q.start();
        let not_short = |i: &Item| !i.source.is_short();
        assert_eq!(
            q.next(&not_short).unwrap().source.label(),
            "YouTube Short 00000000003"
        );
        let all = |_: &Item| true;
        q.set_repeat(Repeat::All);
        assert!(
            q.next(&all).is_none(),
            "no endless loop when everything is rejected"
        );
    }

    #[test]
    fn shuffle_plays_every_item_once_keeps_the_current_one_and_is_repeatable() {
        let mut a = queue(20);
        a.start();
        a.next(&never);
        a.set_shuffle(true);
        assert_eq!(name(&a), "1.mp4", "the current item stays current");
        let mut seen = vec![name(&a)];
        while let Some(i) = a.next(&never) {
            seen.push(i.source.label());
        }
        assert_eq!(
            seen.len(),
            20,
            "the current item first, then every other item once"
        );
        let mut s = seen.clone();
        s.sort();
        s.dedup();
        assert_eq!(s.len(), 20, "no item twice");
        let mut b = Queue::with_seed(5);
        let mut c = Queue::with_seed(5);
        for i in 0..10 {
            b.push(item(i));
            c.push(item(i));
        }
        b.set_shuffle(true);
        c.set_shuffle(true);
        b.start();
        c.start();
        let run = |q: &mut Queue| {
            let mut v = vec![name(q)];
            while let Some(i) = q.next(&never) {
                v.push(i.source.label());
            }
            v
        };
        assert_eq!(run(&mut b), run(&mut c), "same seed, same order");
        let mut plain = queue(10);
        plain.start();
        assert_ne!(
            run(&mut b.clone()),
            run(&mut plain),
            "and it is not just the plain order"
        );
    }

    #[test]
    fn a_shuffled_repeat_all_round_never_starts_with_the_item_just_played() {
        for seed in 1..60u64 {
            let mut q = Queue::with_seed(seed);
            for i in 0..4 {
                q.push(item(i));
            }
            q.set_repeat(Repeat::All);
            q.set_shuffle(true);
            q.start();
            let mut last = name(&q);
            for _ in 0..30 {
                let now = q.next(&never).unwrap().source.label();
                assert_ne!(now, last, "seed {seed}: the same item twice in a row");
                last = now;
            }
        }
        let mut one = queue(1);
        one.set_repeat(Repeat::All);
        one.start();
        assert_eq!(
            one.next(&never).unwrap().source.label(),
            "0.mp4",
            "a single item loops"
        );
    }

    #[test]
    fn turning_shuffle_off_keeps_the_current_item() {
        let mut q = queue(6);
        q.start();
        q.set_shuffle(true);
        q.next(&never);
        let now = name(&q);
        q.set_shuffle(false);
        assert_eq!(name(&q), now);
        let idx = q.current_item_index().unwrap();
        assert_eq!(
            q.next(&never).map(|i| i.source.label()),
            (idx + 1 < 6).then(|| format!("{}.mp4", idx + 1))
        );
    }

    #[test]
    fn jump_remove_and_clear_keep_the_position_right() {
        let mut q = queue(5);
        assert_eq!(q.jump(3).unwrap().source.label(), "3.mp4");
        assert!(q.jump(9).is_none());
        assert!(q.remove(1));
        assert_eq!(
            name(&q),
            "3.mp4",
            "removing an earlier item keeps the same item current"
        );
        assert!(q.remove(2)); // the current one (was 3.mp4, now at index 2)
        assert!(q.current().is_none());
        assert!(!q.remove(99));
        assert_eq!(q.len(), 3);
        q.clear();
        assert!(
            q.is_empty() && q.current().is_none() && q.next(&never).is_none() && q.prev().is_none()
        );
    }

    #[test]
    fn the_queue_has_a_size_limit_and_titles_are_cleaned() {
        let mut q = Queue::default();
        for i in 0..MAX_QUEUE {
            assert!(q.push(item(i)));
        }
        assert!(!q.push(item(0)));
        let mut q = Queue::default();
        let mut it = item(1);
        it.title = Some(format!("a\nb{}", "x".repeat(500)));
        q.push(it);
        let t = q.items()[0].title.clone().unwrap();
        assert!(t.chars().count() <= MAX_TITLE && !t.contains('\n'));
        assert_eq!(q.items()[0].name(), t);
        assert_eq!(item(7).name(), "7.mp4");
    }

    #[test]
    fn items_added_while_shuffled_still_get_played() {
        let mut q = queue(4);
        q.set_shuffle(true);
        q.start();
        q.push(item(100));
        let mut seen = vec![name(&q)];
        while let Some(i) = q.next(&never) {
            seen.push(i.source.label());
        }
        assert_eq!(seen.len(), 5);
        assert!(seen.contains(&"100.mp4".to_string()));
    }
}

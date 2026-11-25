//! A plain copy of the player's state, for screens and status lines.

use crate::engine::{Backend, Player, State};
use crate::queue::Repeat;
use crate::source::Source;

/// One queue row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The name to show.
    pub title: String,
    /// `short`, `video`, `file` or `link`.
    pub kind: &'static str,
    /// True for the item that is current.
    pub current: bool,
}

/// What a screen needs to draw the player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// `idle`, `loading`, `playing`, `paused`, `ended` or `failed`.
    pub state: &'static str,
    /// Why it failed, when it did.
    pub failure: Option<String>,
    /// The name of what is playing, when something is.
    pub now: Option<String>,
    /// Seconds into it.
    pub position: u32,
    /// Its length, when known.
    pub duration: Option<u32>,
    /// Volume, 0 to 100.
    pub volume: u8,
    /// True when muted.
    pub muted: bool,
    /// Speed in percent.
    pub speed: u16,
    /// True when the next item starts by itself.
    pub autoplay: bool,
    /// True in hands-free Shorts mode.
    pub shorts: bool,
    /// True when shuffle is on.
    pub shuffle: bool,
    /// `off`, `one` or `all`.
    pub repeat: &'static str,
    /// The queue in the order added.
    pub queue: Vec<Row>,
}

fn kind(s: &Source) -> &'static str {
    match s {
        Source::YouTube { .. } if s.is_short() => "short",
        Source::YouTube { .. } => "video",
        Source::File(_) => "file",
        Source::Url(_) => "link",
    }
}

impl<B: Backend> Player<B> {
    /// A copy of the state for drawing.
    pub fn snapshot(&self) -> Snapshot {
        let cur = self.queue().current_item_index();
        let playing = matches!(
            self.state(),
            State::Loading | State::Playing | State::Paused
        );
        Snapshot {
            state: match self.state() {
                State::Idle => "idle",
                State::Loading => "loading",
                State::Playing => "playing",
                State::Paused => "paused",
                State::Ended => "ended",
                State::Failed(_) => "failed",
            },
            failure: match self.state() {
                State::Failed(m) => Some(m.clone()),
                _ => None,
            },
            now: self.now_playing().map(|i| i.name()),
            position: self.position(),
            duration: self.duration(),
            volume: self.volume(),
            muted: self.muted(),
            speed: self.speed(),
            autoplay: self.autoplay(),
            shorts: self.shorts_mode(),
            shuffle: self.queue().shuffle(),
            repeat: match self.queue().repeat() {
                Repeat::Off => "off",
                Repeat::One => "one",
                Repeat::All => "all",
            },
            queue: self
                .queue()
                .items()
                .iter()
                .enumerate()
                .map(|(i, it)| Row {
                    title: it.name(),
                    kind: kind(&it.source),
                    current: playing && cur == Some(i),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Command, Event};
    use crate::queue::Item;
    use crate::source::YtKind;

    struct Quiet;
    impl Backend for Quiet {
        fn load(&mut self, _: &Source) -> Result<(), String> {
            Ok(())
        }
        fn pause(&mut self, _: bool) {}
        fn seek(&mut self, _: u32) {}
        fn set_volume(&mut self, _: u8) {}
        fn set_speed(&mut self, _: u16) {}
        fn stop(&mut self) {}
    }

    #[test]
    fn the_snapshot_describes_the_player_and_marks_only_what_is_playing() {
        let mut p = Player::new(Quiet);
        let s = p.snapshot();
        assert_eq!(
            (s.state, s.now.clone(), s.queue.len(), s.repeat),
            ("idle", None, 0, "off")
        );
        p.queue_mut().push(Item::new(Source::YouTube {
            id: "dQw4w9WgXcQ".into(),
            kind: YtKind::Short,
        }));
        p.queue_mut()
            .push(Item::new(Source::File("/m/a.mp4".into())));
        p.queue_mut()
            .push(Item::new(Source::Url("https://x.org/b.webm".into())));
        p.queue_mut().push(Item::new(Source::YouTube {
            id: "aaaaaaaaaaa".into(),
            kind: YtKind::Video,
        }));
        assert!(
            p.snapshot().queue.iter().all(|r| !r.current),
            "nothing is current before play"
        );
        p.command(Command::Play);
        p.on_event(Event::Loaded(Some(60)));
        p.on_event(Event::Position(12));
        let s = p.snapshot();
        assert_eq!((s.state, s.position, s.duration), ("playing", 12, Some(60)));
        assert_eq!(
            s.queue.iter().map(|r| r.kind).collect::<Vec<_>>(),
            ["short", "file", "link", "video"]
        );
        assert_eq!(s.queue.iter().filter(|r| r.current).count(), 1);
        assert!(s.queue[0].current && s.now.as_deref() == Some("YouTube Short dQw4w9WgXcQ"));
        p.command(Command::TogglePause);
        assert_eq!(p.snapshot().state, "paused");
        p.command(Command::Stop);
        assert!(p.snapshot().queue.iter().all(|r| !r.current));
    }

    #[test]
    fn failures_and_flags_show_in_the_snapshot() {
        let mut p = Player::new(Quiet);
        p.queue_mut()
            .push(Item::new(Source::File("/m/a.mp4".into())));
        p.command(Command::ShortsMode(true));
        p.command(Command::Shuffle(true));
        p.command(Command::SetRepeat(Repeat::All));
        p.command(Command::ToggleMute);
        p.command(Command::Play);
        // With repeat-all the one item is retried, but only a few times, then the player gives up.
        for _ in 0..4 {
            p.on_event(Event::Error("disk gone".into()));
        }
        let s = p.snapshot();
        assert_eq!(s.state, "failed");
        assert!(
            s.failure
                .as_deref()
                .is_some_and(|m| m.contains("disk gone")),
            "{:?}",
            s.failure
        );
        assert!(s.shorts && s.shuffle && s.autoplay && s.muted);
        assert_eq!(s.repeat, "all");
    }
}

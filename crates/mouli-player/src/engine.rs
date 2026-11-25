//! The player: a state machine over a queue and a backend.
//!
//! The backend (mpv for files and URLs, YouTube's own player for YouTube items) does the actual
//! playing. The engine decides what to load next, so a feed of Shorts plays hands-free: when one
//! ends, the next starts, failures are skipped, and it can never loop forever over a queue of
//! items that all fail.

use crate::queue::{Item, Queue, Repeat};
use crate::source::Source;

/// Volume steps are this big.
pub const VOLUME_STEP: u8 = 5;
/// Seconds moved by one seek key.
pub const SEEK_STEP: u32 = 5;
/// Consecutive failed items skipped before the player gives up.
pub const MAX_SKIPS: u32 = 3;
/// Playback speeds, in percent.
pub const SPEEDS: [u16; 7] = [50, 75, 100, 125, 150, 175, 200];

/// What the engine asks of whatever does the playing.
pub trait Backend {
    /// Starts playing `source`, replacing what was playing.
    ///
    /// # Errors
    /// A message when this backend cannot play it.
    fn load(&mut self, source: &Source) -> Result<(), String>;
    /// Pauses (true) or resumes (false).
    fn pause(&mut self, paused: bool);
    /// Jumps to `secs` from the start.
    fn seek(&mut self, secs: u32);
    /// Sets the volume, 0 to 100.
    fn set_volume(&mut self, percent: u8);
    /// Sets the speed in percent (100 is normal).
    fn set_speed(&mut self, percent: u16);
    /// Stops and clears what was playing.
    fn stop(&mut self);
}

impl<T: Backend + ?Sized> Backend for Box<T> {
    fn load(&mut self, source: &Source) -> Result<(), String> {
        (**self).load(source)
    }
    fn pause(&mut self, paused: bool) {
        (**self).pause(paused);
    }
    fn seek(&mut self, secs: u32) {
        (**self).seek(secs);
    }
    fn set_volume(&mut self, percent: u8) {
        (**self).set_volume(percent);
    }
    fn set_speed(&mut self, percent: u16) {
        (**self).set_speed(percent);
    }
    fn stop(&mut self) {
        (**self).stop();
    }
}

/// Where the player stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Nothing loaded.
    Idle,
    /// Asked the backend to play; waiting for it to start.
    Loading,
    /// Playing.
    Playing,
    /// Paused.
    Paused,
    /// The queue finished.
    Ended,
    /// Gave up after repeated failures; the message says why.
    Failed(String),
}

/// What the person can ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Start the queue from the beginning (or the current item).
    Play,
    /// Pause when playing, resume when paused.
    TogglePause,
    /// The next item.
    Next,
    /// The previous item.
    Prev,
    /// Jump this many seconds (negative goes back).
    SeekBy(i32),
    /// Jump to this many seconds.
    SeekTo(u32),
    /// Louder or quieter by this many percent.
    VolumeBy(i8),
    /// Mute or unmute.
    ToggleMute,
    /// Faster or slower by one step.
    SpeedStep(i8),
    /// Stop and forget the current item.
    Stop,
    /// Start the next item by itself when one ends.
    Autoplay(bool),
    /// Hands-free Shorts: autoplay on, and only Shorts and files are played (YouTube videos are skipped).
    ShortsMode(bool),
    /// Mark the current item good, bad or reportable.
    Rate(Feedback),
    /// Play queue item number `i` (counting from 0 in the order added).
    Jump(usize),
    /// Take queue item number `i` out. Taking out what is playing stops it.
    Remove(usize),
    /// Empty the queue and stop.
    Clear,
    /// Shuffle on or off.
    Shuffle(bool),
    /// What happens at the end of the queue.
    SetRepeat(Repeat),
}

/// What the backend reports.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The item started (the length, when known).
    Loaded(Option<u32>),
    /// The length became known.
    Duration(u32),
    /// The item is now at this many seconds.
    Position(u32),
    /// The item is playing.
    Playing,
    /// The item is paused.
    Paused,
    /// The item reached its end.
    Ended,
    /// The item could not be played.
    Error(String),
}

/// A person's verdict on an item, kept for curation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feedback {
    /// Good.
    Up,
    /// Not wanted: skip it.
    Down,
    /// Unsuitable: skip it and flag it.
    Report,
}

/// The player.
pub struct Player<B: Backend> {
    backend: B,
    queue: Queue,
    state: State,
    position: u32,
    duration: Option<u32>,
    volume: u8,
    muted: bool,
    speed_index: usize,
    autoplay: bool,
    shorts_mode: bool,
    errors_in_row: u32,
    feedback: Vec<(Source, Feedback)>,
}

impl<B: Backend> Player<B> {
    /// A player over `backend` with an empty queue.
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            queue: Queue::default(),
            state: State::Idle,
            position: 0,
            duration: None,
            volume: 80,
            muted: false,
            speed_index: 2,
            autoplay: true,
            shorts_mode: false,
            errors_in_row: 0,
            feedback: Vec::new(),
        }
    }

    /// The backend, for tests and status.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// The queue.
    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    /// The queue, to add and remove items.
    pub fn queue_mut(&mut self) -> &mut Queue {
        &mut self.queue
    }

    /// The state.
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Seconds into the current item.
    pub fn position(&self) -> u32 {
        self.position
    }

    /// Length of the current item, when known.
    pub fn duration(&self) -> Option<u32> {
        self.duration
    }

    /// The volume, 0 to 100.
    pub fn volume(&self) -> u8 {
        self.volume
    }

    /// True when muted.
    pub fn muted(&self) -> bool {
        self.muted
    }

    /// The speed in percent.
    pub fn speed(&self) -> u16 {
        SPEEDS[self.speed_index]
    }

    /// True when the next item starts by itself.
    pub fn autoplay(&self) -> bool {
        self.autoplay
    }

    /// True in hands-free Shorts mode.
    pub fn shorts_mode(&self) -> bool {
        self.shorts_mode
    }

    /// The verdicts given so far.
    pub fn feedback(&self) -> &[(Source, Feedback)] {
        &self.feedback
    }

    /// What is playing now.
    pub fn now_playing(&self) -> Option<&Item> {
        match self.state {
            State::Idle | State::Ended | State::Failed(_) => None,
            _ => self.queue.current(),
        }
    }

    fn skip_rule(&self) -> impl Fn(&Item) -> bool {
        let shorts = self.shorts_mode;
        // In Shorts mode ordinary YouTube videos are passed over; Shorts and local items play.
        move |i: &Item| shorts && i.source.is_youtube() && !i.source.is_short()
    }

    fn effective_volume(&self) -> u8 {
        if self.muted {
            0
        } else {
            self.volume
        }
    }

    fn load_current(&mut self) {
        let Some(item) = self.queue.current().cloned() else {
            self.finish();
            return;
        };
        self.position = 0;
        self.duration = None;
        match self.backend.load(&item.source) {
            Ok(()) => {
                self.state = State::Loading;
                self.backend.set_volume(self.effective_volume());
                self.backend.set_speed(self.speed());
            }
            Err(e) => self.item_failed(&e),
        }
    }

    fn finish(&mut self) {
        self.backend.stop();
        self.state = State::Ended;
        self.position = 0;
        self.duration = None;
    }

    /// Goes to the next item; at the end of the queue the player stops.
    fn advance(&mut self) {
        let skip = self.skip_rule();
        if self.queue.next(&skip).is_some() {
            self.load_current();
        } else {
            self.finish();
        }
    }

    fn item_failed(&mut self, why: &str) {
        self.errors_in_row += 1;
        if self.errors_in_row > MAX_SKIPS {
            self.backend.stop();
            self.state = State::Failed(format!(
                "stopped after {MAX_SKIPS} failed items in a row: {why}"
            ));
            return;
        }
        let skip = self.skip_rule();
        if self.queue.next(&skip).is_some() {
            self.load_current();
        } else {
            self.backend.stop();
            self.state = State::Failed(why.to_string());
        }
    }

    /// Carries out a command. Commands that do not apply in the current state are ignored.
    pub fn command(&mut self, cmd: Command) {
        match cmd {
            Command::Play => match self.state {
                State::Paused => {
                    self.backend.pause(false);
                    self.state = State::Playing;
                }
                State::Playing | State::Loading => {}
                State::Idle | State::Ended | State::Failed(_) => {
                    self.errors_in_row = 0;
                    let skip = self.skip_rule();
                    // Start from the first item that may be played in the current mode.
                    let first_ok = self.queue.start().map(|i| !skip(i));
                    let found = match first_ok {
                        Some(true) => true,
                        Some(false) => self.queue.next(&skip).is_some(),
                        None => false,
                    };
                    if found {
                        self.load_current();
                    }
                }
            },
            Command::TogglePause => match self.state {
                State::Playing => {
                    self.backend.pause(true);
                    self.state = State::Paused;
                }
                State::Paused => {
                    self.backend.pause(false);
                    self.state = State::Playing;
                }
                _ => {}
            },
            Command::Next => {
                self.errors_in_row = 0;
                if self.queue.current().is_some() || !self.queue.is_empty() {
                    self.advance();
                }
            }
            Command::Prev => {
                self.errors_in_row = 0;
                // Past the first seconds, "previous" restarts the item, as players usually do.
                if self.position > 3 && matches!(self.state, State::Playing | State::Paused) {
                    self.backend.seek(0);
                    self.position = 0;
                } else if self.queue.prev().is_some() {
                    self.load_current();
                }
            }
            Command::SeekBy(d) => {
                if matches!(self.state, State::Playing | State::Paused) {
                    let target = (i64::from(self.position) + i64::from(d)).max(0) as u32;
                    self.command(Command::SeekTo(target));
                }
            }
            Command::SeekTo(t) => {
                if matches!(self.state, State::Playing | State::Paused) {
                    let t = self.duration.map_or(t, |d| t.min(d));
                    self.backend.seek(t);
                    self.position = t;
                }
            }
            Command::VolumeBy(d) => {
                self.volume = (i16::from(self.volume) + i16::from(d)).clamp(0, 100) as u8;
                if self.volume > 0 {
                    self.muted = false;
                }
                self.backend.set_volume(self.effective_volume());
            }
            Command::ToggleMute => {
                self.muted = !self.muted;
                self.backend.set_volume(self.effective_volume());
            }
            Command::SpeedStep(d) => {
                let i = (self.speed_index as i64 + i64::from(d)).clamp(0, SPEEDS.len() as i64 - 1);
                self.speed_index = i as usize;
                self.backend.set_speed(self.speed());
            }
            Command::Stop => {
                self.backend.stop();
                self.state = State::Idle;
                self.position = 0;
                self.duration = None;
            }
            Command::Autoplay(on) => {
                self.autoplay = on;
                if !on {
                    self.shorts_mode = false;
                }
            }
            Command::ShortsMode(on) => {
                self.shorts_mode = on;
                if on {
                    self.autoplay = true;
                }
            }
            Command::Jump(i) => {
                self.errors_in_row = 0;
                if self.queue.jump(i).is_some() {
                    self.load_current();
                }
            }
            Command::Remove(i) => {
                if i < self.queue.len() {
                    let playing_now = self.queue.current_item_index() == Some(i)
                        && matches!(self.state, State::Loading | State::Playing | State::Paused);
                    if playing_now {
                        self.backend.stop();
                        self.state = State::Idle;
                        self.position = 0;
                        self.duration = None;
                    }
                    self.queue.remove(i);
                }
            }
            Command::Clear => {
                self.queue.clear();
                self.backend.stop();
                self.state = State::Idle;
                self.position = 0;
                self.duration = None;
            }
            Command::Shuffle(on) => self.queue.set_shuffle(on),
            Command::SetRepeat(r) => self.queue.set_repeat(r),
            Command::Rate(f) => {
                if let Some(item) = self.queue.current().cloned() {
                    if matches!(self.state, State::Loading | State::Playing | State::Paused) {
                        self.feedback.push((item.source, f));
                        if f != Feedback::Up {
                            self.errors_in_row = 0;
                            self.advance();
                        }
                    }
                }
            }
        }
    }

    /// Takes in something the backend reported.
    pub fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Loaded(d) => {
                if self.state == State::Loading {
                    self.state = State::Playing;
                    self.errors_in_row = 0;
                    self.duration = d;
                }
            }
            Event::Duration(d) => {
                if matches!(self.state, State::Loading | State::Playing | State::Paused) {
                    self.duration = Some(d);
                }
            }
            Event::Playing => {
                if matches!(self.state, State::Loading | State::Paused) {
                    self.state = State::Playing;
                    self.errors_in_row = 0;
                }
            }
            Event::Paused => {
                if self.state == State::Playing {
                    self.state = State::Paused;
                }
            }
            Event::Position(p) => {
                if matches!(self.state, State::Playing | State::Paused) {
                    self.position = p;
                    // A preview moves on after its time even if the item is longer.
                    let limit = self.queue.current().and_then(|i| i.max_secs);
                    if self.state == State::Playing
                        && limit.is_some_and(|m| p >= m)
                        && self.autoplay
                    {
                        self.advance();
                    }
                }
            }
            Event::Ended => {
                if matches!(self.state, State::Loading | State::Playing | State::Paused) {
                    self.errors_in_row = 0;
                    if self.queue.repeat() == Repeat::One {
                        self.load_current();
                    } else if self.autoplay {
                        self.advance();
                    } else {
                        self.state = State::Ended;
                    }
                }
            }
            Event::Error(m) => {
                if matches!(self.state, State::Loading | State::Playing | State::Paused) {
                    self.item_failed(&m);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::Item;
    use crate::source::{Source, YtKind};

    #[derive(Default)]
    struct Fake {
        loaded: Vec<String>,
        fail: Vec<String>,
        paused: Vec<bool>,
        seeks: Vec<u32>,
        volumes: Vec<u8>,
        speeds: Vec<u16>,
        stops: u32,
    }

    impl Backend for Fake {
        fn load(&mut self, s: &Source) -> Result<(), String> {
            let l = s.label();
            if self.fail.contains(&l) {
                return Err(format!("cannot play {l}"));
            }
            self.loaded.push(l);
            Ok(())
        }
        fn pause(&mut self, p: bool) {
            self.paused.push(p);
        }
        fn seek(&mut self, s: u32) {
            self.seeks.push(s);
        }
        fn set_volume(&mut self, v: u8) {
            self.volumes.push(v);
        }
        fn set_speed(&mut self, v: u16) {
            self.speeds.push(v);
        }
        fn stop(&mut self) {
            self.stops += 1;
        }
    }

    fn file(n: usize) -> Item {
        Item::new(Source::File(format!("/m/{n}.mp4")))
    }

    fn yt(n: usize, kind: YtKind) -> Item {
        Item::new(Source::YouTube {
            id: format!("{n:011}"),
            kind,
        })
    }

    fn player(n: usize) -> Player<Fake> {
        let mut p = Player::new(Fake::default());
        for i in 0..n {
            p.queue_mut().push(file(i));
        }
        p
    }

    fn started(p: &mut Player<Fake>) {
        p.command(Command::Play);
        p.on_event(Event::Loaded(Some(30)));
    }

    #[test]
    fn play_loads_the_first_item_and_the_backend_confirms() {
        let mut p = player(2);
        assert_eq!(*p.state(), State::Idle);
        p.command(Command::Play);
        assert_eq!(*p.state(), State::Loading);
        assert_eq!(p.backend().loaded, vec!["0.mp4"]);
        p.on_event(Event::Loaded(Some(42)));
        assert_eq!(*p.state(), State::Playing);
        assert_eq!(p.duration(), Some(42));
        assert_eq!(p.now_playing().unwrap().name(), "0.mp4");
    }

    #[test]
    fn when_an_item_ends_the_next_one_starts_by_itself() {
        let mut p = player(3);
        started(&mut p);
        for expect in ["1.mp4", "2.mp4"] {
            p.on_event(Event::Ended);
            assert_eq!(p.backend().loaded.last().unwrap(), expect);
            assert_eq!(*p.state(), State::Loading);
            p.on_event(Event::Loaded(None));
        }
        p.on_event(Event::Ended);
        assert_eq!(*p.state(), State::Ended, "the queue is finished");
        assert!(p.now_playing().is_none());
        assert_eq!(p.backend().stops, 1);
    }

    #[test]
    fn with_autoplay_off_it_stops_at_the_end_of_each_item() {
        let mut p = player(3);
        p.command(Command::Autoplay(false));
        started(&mut p);
        p.on_event(Event::Ended);
        assert_eq!(*p.state(), State::Ended);
        assert_eq!(p.backend().loaded.len(), 1);
        p.command(Command::Next);
        assert_eq!(
            p.backend().loaded.last().unwrap(),
            "1.mp4",
            "the person can still go on"
        );
    }

    #[test]
    fn repeat_one_plays_the_same_item_again_and_repeat_all_loops_the_queue() {
        let mut p = player(2);
        p.queue_mut().set_repeat(Repeat::One);
        started(&mut p);
        p.on_event(Event::Ended);
        p.on_event(Event::Loaded(None));
        assert_eq!(p.backend().loaded, vec!["0.mp4", "0.mp4"]);
        p.queue_mut().set_repeat(Repeat::All);
        for _ in 0..4 {
            p.on_event(Event::Ended);
            p.on_event(Event::Loaded(None));
        }
        assert_eq!(p.backend().loaded.len(), 6, "keeps going round");
        assert_eq!(*p.state(), State::Playing);
    }

    #[test]
    fn a_failing_item_is_skipped_but_never_forever() {
        let mut p = player(8);
        p.backend.fail = (1..8).map(|i| format!("{i}.mp4")).collect();
        started(&mut p);
        p.on_event(Event::Ended); // 1,2,3 fail to load (3 skips), the 4th failure gives up
        assert!(
            matches!(p.state(), State::Failed(m) if m.contains("failed items in a row")),
            "{:?}",
            p.state()
        );
        assert_eq!(p.backend().loaded, vec!["0.mp4"]);
        // The person can try again from the start.
        p.backend.fail.clear();
        p.command(Command::Play);
        assert_eq!(*p.state(), State::Loading);
    }

    #[test]
    fn a_backend_error_during_playback_skips_to_the_next_and_a_good_item_resets_the_count() {
        let mut p = player(6);
        started(&mut p);
        p.on_event(Event::Error("decode".into()));
        assert_eq!(p.backend().loaded.last().unwrap(), "1.mp4");
        p.on_event(Event::Loaded(None));
        for _ in 0..3 {
            p.on_event(Event::Error("decode".into()));
            p.on_event(Event::Loaded(None));
        }
        assert_eq!(
            *p.state(),
            State::Playing,
            "each success resets the failure count"
        );
        let last = player(1);
        let mut last = last;
        started(&mut last);
        last.on_event(Event::Error("gone".into()));
        assert!(matches!(last.state(), State::Failed(m) if m == "gone"));
    }

    #[test]
    fn shorts_mode_plays_only_shorts_and_files_and_never_needs_scrolling() {
        let mut p = Player::new(Fake::default());
        p.queue_mut().push(yt(1, YtKind::Short));
        p.queue_mut().push(yt(2, YtKind::Video));
        p.queue_mut().push(yt(3, YtKind::Short));
        p.queue_mut().push(file(4));
        p.command(Command::Autoplay(false));
        p.command(Command::ShortsMode(true));
        assert!(p.autoplay() && p.shorts_mode());
        started(&mut p);
        let mut seen = vec![p.backend().loaded.last().unwrap().clone()];
        for _ in 0..2 {
            p.on_event(Event::Ended);
            p.on_event(Event::Loaded(None));
            seen.push(p.backend().loaded.last().unwrap().clone());
        }
        assert_eq!(
            seen,
            vec![
                "YouTube Short 00000000001",
                "YouTube Short 00000000003",
                "4.mp4"
            ]
        );
        p.on_event(Event::Ended);
        assert_eq!(*p.state(), State::Ended);
        p.command(Command::Autoplay(false));
        assert!(!p.shorts_mode(), "turning autoplay off ends Shorts mode");
    }

    #[test]
    fn shorts_mode_with_repeat_all_is_an_endless_feed() {
        let mut p = Player::new(Fake::default());
        for i in 0..3 {
            p.queue_mut().push(yt(i, YtKind::Short));
        }
        p.queue_mut().set_repeat(Repeat::All);
        p.command(Command::ShortsMode(true));
        started(&mut p);
        for _ in 0..20 {
            p.on_event(Event::Ended);
            p.on_event(Event::Loaded(None));
        }
        assert_eq!(*p.state(), State::Playing);
        assert_eq!(p.backend().loaded.len(), 21);
    }

    #[test]
    fn a_preview_item_moves_on_after_its_time() {
        let mut p = Player::new(Fake::default());
        let mut a = file(0);
        a.max_secs = Some(30);
        p.queue_mut().push(a);
        p.queue_mut().push(file(1));
        started(&mut p);
        p.on_event(Event::Position(29));
        assert_eq!(p.backend().loaded.len(), 1);
        p.on_event(Event::Position(30));
        assert_eq!(p.backend().loaded.last().unwrap(), "1.mp4");
    }

    #[test]
    fn pause_resume_and_the_events_that_do_not_apply_are_handled() {
        let mut p = player(1);
        p.command(Command::TogglePause);
        assert_eq!(*p.state(), State::Idle, "nothing to pause");
        started(&mut p);
        p.command(Command::TogglePause);
        assert_eq!(*p.state(), State::Paused);
        p.command(Command::TogglePause);
        assert_eq!(*p.state(), State::Playing);
        assert_eq!(p.backend().paused, vec![true, false]);
        p.on_event(Event::Paused);
        assert_eq!(*p.state(), State::Paused);
        p.on_event(Event::Playing);
        assert_eq!(*p.state(), State::Playing);
        let mut idle = player(1);
        idle.on_event(Event::Ended);
        idle.on_event(Event::Error("x".into()));
        idle.on_event(Event::Position(5));
        assert_eq!(*idle.state(), State::Idle, "stray events change nothing");
    }

    #[test]
    fn seeking_is_clamped_to_the_item() {
        let mut p = player(1);
        started(&mut p); // 30 seconds
        p.on_event(Event::Position(10));
        p.command(Command::SeekBy(5));
        p.command(Command::SeekBy(-100));
        p.command(Command::SeekTo(999));
        assert_eq!(p.backend().seeks, vec![15, 0, 30]);
        assert_eq!(p.position(), 30);
        let mut loading = player(1);
        loading.command(Command::Play);
        loading.command(Command::SeekBy(5));
        assert!(
            loading.backend().seeks.is_empty(),
            "no seeking before it has started"
        );
    }

    #[test]
    fn volume_mute_and_speed_stay_in_range() {
        let mut p = player(1);
        for _ in 0..30 {
            p.command(Command::VolumeBy(VOLUME_STEP as i8));
        }
        assert_eq!(p.volume(), 100);
        for _ in 0..30 {
            p.command(Command::VolumeBy(-(VOLUME_STEP as i8)));
        }
        assert_eq!(p.volume(), 0);
        p.command(Command::VolumeBy(20));
        p.command(Command::ToggleMute);
        assert!(p.muted());
        assert_eq!(*p.backend().volumes.last().unwrap(), 0);
        p.command(Command::VolumeBy(5));
        assert!(!p.muted(), "changing the volume unmutes");
        for _ in 0..20 {
            p.command(Command::SpeedStep(1));
        }
        assert_eq!(p.speed(), 200);
        for _ in 0..20 {
            p.command(Command::SpeedStep(-1));
        }
        assert_eq!(p.speed(), 50);
    }

    #[test]
    fn a_new_item_starts_at_the_current_volume_and_speed() {
        let mut p = player(2);
        p.command(Command::VolumeBy(-30));
        p.command(Command::SpeedStep(2));
        p.command(Command::ToggleMute);
        p.command(Command::Play);
        assert_eq!(*p.backend().volumes.last().unwrap(), 0, "muted");
        assert_eq!(*p.backend().speeds.last().unwrap(), 150);
    }

    #[test]
    fn previous_restarts_after_a_few_seconds_and_goes_back_near_the_start() {
        let mut p = player(3);
        started(&mut p);
        p.on_event(Event::Ended);
        p.on_event(Event::Loaded(None));
        p.on_event(Event::Position(10));
        p.command(Command::Prev);
        assert_eq!(p.backend().seeks, vec![0]);
        assert_eq!(p.backend().loaded.last().unwrap(), "1.mp4");
        p.on_event(Event::Position(1));
        p.command(Command::Prev);
        assert_eq!(p.backend().loaded.last().unwrap(), "0.mp4");
    }

    #[test]
    fn next_on_the_last_item_ends_and_stop_goes_idle() {
        let mut p = player(2);
        started(&mut p);
        p.command(Command::Next);
        p.on_event(Event::Loaded(None));
        p.command(Command::Next);
        assert_eq!(*p.state(), State::Ended);
        let mut q = player(1);
        started(&mut q);
        q.command(Command::Stop);
        assert_eq!(*q.state(), State::Idle);
        assert_eq!(q.position(), 0);
    }

    #[test]
    fn rating_down_or_reporting_skips_and_up_keeps_playing_and_all_are_remembered() {
        let mut p = player(4);
        started(&mut p);
        p.command(Command::Rate(Feedback::Up));
        assert_eq!(p.backend().loaded.len(), 1);
        p.command(Command::Rate(Feedback::Down));
        assert_eq!(p.backend().loaded.last().unwrap(), "1.mp4");
        p.on_event(Event::Loaded(None));
        p.command(Command::Rate(Feedback::Report));
        assert_eq!(p.backend().loaded.last().unwrap(), "2.mp4");
        let f: Vec<_> = p.feedback().iter().map(|(s, f)| (s.label(), *f)).collect();
        assert_eq!(
            f,
            vec![
                ("0.mp4".into(), Feedback::Up),
                ("0.mp4".into(), Feedback::Down),
                ("1.mp4".into(), Feedback::Report)
            ]
        );
        let mut idle = player(1);
        idle.command(Command::Rate(Feedback::Down));
        assert!(
            idle.feedback().is_empty(),
            "nothing to rate when nothing plays"
        );
    }

    #[test]
    fn an_empty_queue_never_panics() {
        let mut p = Player::new(Fake::default());
        for c in [
            Command::Play,
            Command::Next,
            Command::Prev,
            Command::TogglePause,
            Command::Stop,
            Command::SeekBy(3),
            Command::Rate(Feedback::Down),
        ] {
            p.command(c);
        }
        assert!(matches!(p.state(), State::Idle | State::Ended));
        assert!(p.backend().loaded.is_empty());
    }

    #[test]
    fn play_after_the_queue_ended_starts_again_and_play_while_paused_resumes() {
        let mut p = player(1);
        started(&mut p);
        p.on_event(Event::Ended);
        assert_eq!(*p.state(), State::Ended);
        p.command(Command::Play);
        assert_eq!(p.backend().loaded.len(), 2);
        p.on_event(Event::Loaded(None));
        p.command(Command::TogglePause);
        p.command(Command::Play);
        assert_eq!(*p.state(), State::Playing);
        assert_eq!(p.backend().loaded.len(), 2, "resuming does not reload");
    }
}

#[cfg(test)]
mod more_commands {
    use super::*;
    use crate::queue::Item;

    #[derive(Default)]
    struct Fake {
        loaded: Vec<String>,
        stops: u32,
    }
    impl Backend for Fake {
        fn load(&mut self, s: &Source) -> Result<(), String> {
            self.loaded.push(s.label());
            Ok(())
        }
        fn pause(&mut self, _: bool) {}
        fn seek(&mut self, _: u32) {}
        fn set_volume(&mut self, _: u8) {}
        fn set_speed(&mut self, _: u16) {}
        fn stop(&mut self) {
            self.stops += 1;
        }
    }

    fn player(n: usize) -> Player<Fake> {
        let mut p = Player::new(Fake::default());
        for i in 0..n {
            p.queue_mut()
                .push(Item::new(Source::File(format!("/m/{i}.mp4"))));
        }
        p
    }

    #[test]
    fn jump_plays_the_chosen_item_and_ignores_a_bad_number() {
        let mut p = player(4);
        p.command(Command::Jump(2));
        assert_eq!(p.backend().loaded, vec!["2.mp4"]);
        assert_eq!(*p.state(), State::Loading);
        p.command(Command::Jump(99));
        assert_eq!(p.backend().loaded.len(), 1, "no such item: nothing happens");
        p.on_event(Event::Loaded(None));
        p.on_event(Event::Ended);
        assert_eq!(
            p.backend().loaded.last().unwrap(),
            "3.mp4",
            "play goes on from there"
        );
    }

    #[test]
    fn removing_what_plays_stops_it_and_removing_another_keeps_playing() {
        let mut p = player(4);
        p.command(Command::Play);
        p.on_event(Event::Loaded(None));
        p.command(Command::Remove(3));
        assert_eq!(*p.state(), State::Playing);
        assert_eq!(p.queue().len(), 3);
        p.command(Command::Remove(0));
        assert_eq!(*p.state(), State::Idle, "the playing item was taken out");
        assert_eq!(p.backend().stops, 1);
        assert_eq!(p.queue().len(), 2);
        p.command(Command::Remove(50));
        assert_eq!(p.queue().len(), 2, "no such item");
    }

    #[test]
    fn clear_empties_and_stops_and_shuffle_and_repeat_pass_through() {
        let mut p = player(3);
        p.command(Command::Shuffle(true));
        p.command(Command::SetRepeat(Repeat::All));
        assert!(p.queue().shuffle());
        assert_eq!(p.queue().repeat(), Repeat::All);
        p.command(Command::Play);
        p.on_event(Event::Loaded(None));
        p.command(Command::Clear);
        assert!(p.queue().is_empty());
        assert_eq!(*p.state(), State::Idle);
        assert!(p.now_playing().is_none());
        p.command(Command::Play);
        assert_eq!(*p.state(), State::Idle, "nothing to play");
    }

    #[test]
    fn a_boxed_backend_works_as_a_backend() {
        let b: Box<dyn Backend> = Box::new(Fake::default());
        let mut p = Player::new(b);
        p.queue_mut()
            .push(Item::new(Source::File("/m/a.mp4".into())));
        p.command(Command::Play);
        assert_eq!(*p.state(), State::Loading);
    }
}

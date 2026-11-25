//! The video and YouTube player as capabilities.
//!
//! Every control of the player is a capability of the governed workspace, so the chat, the player
//! screen and a model all use the same gate (policy, audit) and nothing plays by a side door.
//! Adding an address only parses it (see `mouli_player::parse`): YouTube is played by YouTube's
//! own player, files and direct media addresses by mpv, and nothing is downloaded.

use mouli_interface::{Application, Call, CapabilitySpec, Error, Output, Result, Risk};
use mouli_player::{parse, Backend, Command, Feedback, Item, Player, Repeat};
use std::cell::RefCell;
use std::rc::Rc;

/// The player, shared between the adapter and the screen.
pub type SharedPlayer = Rc<RefCell<Player<Box<dyn Backend>>>>;

/// Longest seek step, in seconds (a day).
const MAX_SEEK: i64 = 86_400;

/// The player application.
pub struct PlayerApp {
    player: SharedPlayer,
}

impl PlayerApp {
    /// An application controlling `player`.
    pub fn new(player: SharedPlayer) -> Self {
        Self { player }
    }

    /// A new player over `backend` with an empty queue.
    pub fn new_player(backend: Box<dyn Backend>) -> SharedPlayer {
        Rc::new(RefCell::new(Player::new(backend)))
    }
}

fn bad(msg: impl Into<String>) -> Error {
    Error::Handler(msg.into())
}

fn arg<'a>(call: &'a Call, name: &str) -> Result<&'a str> {
    call.args
        .get(name)
        .map(String::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| bad(format!("{name} is required")))
}

fn whole(call: &Call, name: &str, min: i64, max: i64) -> Result<i64> {
    let v = arg(call, name)?;
    let n: i64 = v
        .trim()
        .parse()
        .map_err(|_| bad(format!("{name} must be a whole number")))?;
    if (min..=max).contains(&n) {
        Ok(n)
    } else {
        Err(bad(format!("{name} must be between {min} and {max}")))
    }
}

fn on_off(call: &Call, name: &str) -> Result<bool> {
    match arg(call, name)?.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        other => Err(bad(format!("{name} must be on or off, not '{other}'"))),
    }
}

fn clock(secs: u32) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

fn status_line(p: &Player<Box<dyn Backend>>) -> String {
    let s = p.snapshot();
    let mut t = match (&s.now, s.state) {
        (Some(n), st) => {
            let at = match s.duration {
                Some(d) => format!("{} / {}", clock(s.position), clock(d)),
                None => clock(s.position),
            };
            format!("{st}: {n} {at}")
        }
        (None, "failed") => format!("failed: {}", s.failure.clone().unwrap_or_default()),
        (None, st) => st.to_string(),
    };
    t.push_str(&format!(
        ", volume {}%{}, speed {}.{:02}x, shorts feed {}, autoplay {}, shuffle {}, repeat {}, {} in queue",
        s.volume,
        if s.muted { " (muted)" } else { "" },
        s.speed / 100,
        s.speed % 100,
        if s.shorts { "on" } else { "off" },
        if s.autoplay { "on" } else { "off" },
        if s.shuffle { "on" } else { "off" },
        s.repeat,
        s.queue.len()
    ));
    t
}

impl Application for PlayerApp {
    fn name(&self) -> &str {
        "player"
    }

    fn capabilities(&self) -> Vec<CapabilitySpec> {
        let w = |n: &str, d: &str| CapabilitySpec::new(n, d, Risk::Write);
        vec![
            CapabilitySpec::new("player.status", "what is playing, and the player's settings", Risk::Read),
            CapabilitySpec::new("player.queue", "list the queue", Risk::Read),
            w("player.add", "add a YouTube link, a media file path or a media address to the queue (url=.. [title=..] [max=SECONDS])").requires(&["url"]),
            w("player.play", "start the queue, or resume when paused"),
            w("player.pause", "pause or resume"),
            w("player.next", "the next item"),
            w("player.prev", "the previous item, or the start of this one"),
            w("player.stop", "stop playing"),
            w("player.seek", "jump forward or back (by=SECONDS, negative goes back)").requires(&["by"]),
            w("player.volume", "louder or quieter (by=-100..100)").requires(&["by"]),
            w("player.mute", "mute or unmute"),
            w("player.speed", "faster or slower by steps (by=-6..6)").requires(&["by"]),
            w("player.shuffle", "shuffle on or off (on=true|false)").requires(&["on"]),
            w("player.repeat", "at the end of the queue (mode=off|one|all)").requires(&["mode"]),
            w("player.autoplay", "start the next item by itself (on=true|false)").requires(&["on"]),
            w("player.shorts", "hands-free Shorts: autoplay and only Shorts and files (on=true|false)").requires(&["on"]),
            w("player.rate", "say how the item was (verdict=up|down|report); down and report skip it").requires(&["verdict"]),
            w("player.jump", "play queue item number (index=0 is the first)").requires(&["index"]),
            w("player.remove", "take a queue item out (index=..)").requires(&["index"]),
            w("player.clear", "empty the queue and stop"),
        ]
    }

    fn invoke(&mut self, call: &Call) -> Result<Output> {
        let mut p = self.player.borrow_mut();
        let text = match call.capability.as_str() {
            "player.status" => status_line(&p),
            "player.queue" => {
                let s = p.snapshot();
                if s.queue.is_empty() {
                    "The queue is empty.".to_string()
                } else {
                    s.queue
                        .iter()
                        .enumerate()
                        .map(|(i, r)| {
                            format!(
                                "{}{i}  [{}] {}",
                                if r.current { ">" } else { " " },
                                r.kind,
                                r.title
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            "player.add" => {
                let src = parse(arg(call, "url")?).map_err(|e| bad(e.to_string()))?;
                let mut item = Item::new(src);
                item.title = call
                    .args
                    .get("title")
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty());
                if call.args.contains_key("max") {
                    item.max_secs = Some(whole(call, "max", 1, MAX_SEEK)? as u32);
                }
                if !p.queue_mut().push(item) {
                    return Err(bad("the queue is full"));
                }
                // The queue cleans and shortens titles, so report the name it kept.
                let name = p.queue().items().last().map(Item::name).unwrap_or_default();
                format!("Added {name} ({} in the queue).", p.queue().len())
            }
            "player.play" => {
                p.command(Command::Play);
                status_line(&p)
            }
            "player.pause" => {
                p.command(Command::TogglePause);
                status_line(&p)
            }
            "player.next" => {
                p.command(Command::Next);
                status_line(&p)
            }
            "player.prev" => {
                p.command(Command::Prev);
                status_line(&p)
            }
            "player.stop" => {
                p.command(Command::Stop);
                "Stopped.".to_string()
            }
            "player.seek" => {
                p.command(Command::SeekBy(
                    whole(call, "by", -MAX_SEEK, MAX_SEEK)? as i32
                ));
                status_line(&p)
            }
            "player.volume" => {
                p.command(Command::VolumeBy(whole(call, "by", -100, 100)? as i8));
                format!("Volume {}%.", p.volume())
            }
            "player.mute" => {
                p.command(Command::ToggleMute);
                if p.muted() {
                    "Muted.".into()
                } else {
                    "Sound on.".into()
                }
            }
            "player.speed" => {
                p.command(Command::SpeedStep(whole(call, "by", -6, 6)? as i8));
                format!("Speed {}.{:02}x.", p.speed() / 100, p.speed() % 100)
            }
            "player.shuffle" => {
                let on = on_off(call, "on")?;
                p.command(Command::Shuffle(on));
                format!("Shuffle {}.", if on { "on" } else { "off" })
            }
            "player.repeat" => {
                let r = match arg(call, "mode")?.trim().to_ascii_lowercase().as_str() {
                    "off" => Repeat::Off,
                    "one" => Repeat::One,
                    "all" => Repeat::All,
                    other => {
                        return Err(bad(format!("mode must be off, one or all, not '{other}'")))
                    }
                };
                p.command(Command::SetRepeat(r));
                format!("Repeat {}.", p.snapshot().repeat)
            }
            "player.autoplay" => {
                let on = on_off(call, "on")?;
                p.command(Command::Autoplay(on));
                format!("Autoplay {}.", if on { "on" } else { "off" })
            }
            "player.shorts" => {
                let on = on_off(call, "on")?;
                p.command(Command::ShortsMode(on));
                format!("Shorts feed {}.", if on { "on" } else { "off" })
            }
            "player.rate" => {
                let f = match arg(call, "verdict")?.trim().to_ascii_lowercase().as_str() {
                    "up" => Feedback::Up,
                    "down" => Feedback::Down,
                    "report" => Feedback::Report,
                    other => {
                        return Err(bad(format!(
                            "verdict must be up, down or report, not '{other}'"
                        )))
                    }
                };
                if p.now_playing().is_none() {
                    return Err(bad("nothing is playing to rate"));
                }
                p.command(Command::Rate(f));
                format!("Noted ({} verdict(s) so far).", p.feedback().len())
            }
            "player.jump" => {
                let i = whole(call, "index", 0, 1_000_000)? as usize;
                if i >= p.queue().len() {
                    return Err(bad(format!(
                        "there is no item {i} (the queue has {})",
                        p.queue().len()
                    )));
                }
                p.command(Command::Jump(i));
                status_line(&p)
            }
            "player.remove" => {
                let i = whole(call, "index", 0, 1_000_000)? as usize;
                if i >= p.queue().len() {
                    return Err(bad(format!(
                        "there is no item {i} (the queue has {})",
                        p.queue().len()
                    )));
                }
                p.command(Command::Remove(i));
                format!("Removed item {i} ({} left).", p.queue().len())
            }
            "player.clear" => {
                p.command(Command::Clear);
                "The queue is empty.".to_string()
            }
            other => return Err(bad(format!("unknown capability {other}"))),
        };
        Ok(Output {
            text,
            artifacts: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mouli_interface::{Permissions, Policy, Reply, Workspace};
    use mouli_player::{Event, Source};
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Log(Rc<RefCell<Vec<String>>>);
    impl Backend for Log {
        fn load(&mut self, s: &Source) -> std::result::Result<(), String> {
            self.0.borrow_mut().push(format!("load {}", s.label()));
            Ok(())
        }
        fn pause(&mut self, p: bool) {
            self.0.borrow_mut().push(format!("pause {p}"));
        }
        fn seek(&mut self, s: u32) {
            self.0.borrow_mut().push(format!("seek {s}"));
        }
        fn set_volume(&mut self, v: u8) {
            self.0.borrow_mut().push(format!("vol {v}"));
        }
        fn set_speed(&mut self, v: u16) {
            self.0.borrow_mut().push(format!("speed {v}"));
        }
        fn stop(&mut self) {
            self.0.borrow_mut().push("stop".into());
        }
    }

    fn setup() -> (Workspace, SharedPlayer, Rc<RefCell<Vec<String>>>, String) {
        let log = Rc::new(RefCell::new(Vec::new()));
        let player = PlayerApp::new_player(Box::new(Log(log.clone())));
        let mut ws = Workspace::new(Policy::default(), Default::default()).unwrap();
        ws.add_role(Permissions::new(
            "viewer",
            &["player.status", "player.queue"],
        ));
        ws.add_role(Permissions::new("member", &["player.*"]));
        ws.register_app(Box::new(PlayerApp::new(player.clone())))
            .unwrap();
        let s = ws.open_session("acme", "ann", "member").unwrap();
        (ws, player, log, s)
    }

    fn call(ws: &mut Workspace, s: &str, cap: &str, args: &[(&str, &str)]) -> Result<String> {
        let a: BTreeMap<String, String> = args
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        match ws.call("acme", s, cap, a)? {
            Reply::Done(o) => Ok(o.text),
            other => panic!("{other:?}"),
        }
    }

    const SHORT: &str = "https://www.youtube.com/shorts/dQw4w9WgXcQ";

    #[test]
    fn a_queue_is_built_played_and_controlled_through_the_gate() {
        let (mut ws, p, log, s) = setup();
        assert_eq!(
            call(&mut ws, &s, "player.queue", &[]).unwrap(),
            "The queue is empty."
        );
        assert!(call(&mut ws, &s, "player.add", &[("url", SHORT)])
            .unwrap()
            .contains("1 in the queue"));
        call(
            &mut ws,
            &s,
            "player.add",
            &[
                ("url", "/m/film.mkv"),
                ("title", "Film night"),
                ("max", "30"),
            ],
        )
        .unwrap();
        let q = call(&mut ws, &s, "player.queue", &[]).unwrap();
        assert!(
            q.contains("[short] YouTube Short dQw4w9WgXcQ") && q.contains("[file] Film night"),
            "{q}"
        );
        assert!(call(&mut ws, &s, "player.play", &[])
            .unwrap()
            .starts_with("loading: YouTube Short"));
        p.borrow_mut().on_event(Event::Loaded(Some(65)));
        p.borrow_mut().on_event(Event::Position(7));
        let st = call(&mut ws, &s, "player.status", &[]).unwrap();
        assert!(
            st.starts_with("playing: YouTube Short dQw4w9WgXcQ 0:07 / 1:05"),
            "{st}"
        );
        assert!(call(&mut ws, &s, "player.pause", &[])
            .unwrap()
            .starts_with("paused:"));
        call(&mut ws, &s, "player.pause", &[]).unwrap();
        call(&mut ws, &s, "player.seek", &[("by", "10")]).unwrap();
        assert_eq!(
            call(&mut ws, &s, "player.volume", &[("by", "-30")]).unwrap(),
            "Volume 50%."
        );
        assert_eq!(call(&mut ws, &s, "player.mute", &[]).unwrap(), "Muted.");
        assert_eq!(
            call(&mut ws, &s, "player.speed", &[("by", "1")]).unwrap(),
            "Speed 1.25x."
        );
        let l = log.borrow().clone();
        assert!(
            l.contains(&"load YouTube Short dQw4w9WgXcQ".to_string())
                && l.contains(&"seek 17".to_string())
                && l.contains(&"vol 50".to_string())
                && l.contains(&"speed 125".to_string()),
            "{l:?}"
        );
        assert!(ws.audit().verify().is_ok());
        assert!(
            ws.audit()
                .records()
                .iter()
                .any(|r| r.action == "player.add" && r.outcome == "ok"),
            "every call is audited"
        );
    }

    #[test]
    fn modes_jump_remove_rate_and_clear() {
        let (mut ws, p, _log, s) = setup();
        for i in 0..3 {
            call(
                &mut ws,
                &s,
                "player.add",
                &[("url", &format!("/m/{i}.mp4"))],
            )
            .unwrap();
        }
        assert_eq!(
            call(&mut ws, &s, "player.shuffle", &[("on", "true")]).unwrap(),
            "Shuffle on."
        );
        assert_eq!(
            call(&mut ws, &s, "player.repeat", &[("mode", "ALL")]).unwrap(),
            "Repeat all."
        );
        assert_eq!(
            call(&mut ws, &s, "player.autoplay", &[("on", "off")]).unwrap(),
            "Autoplay off."
        );
        assert_eq!(
            call(&mut ws, &s, "player.shorts", &[("on", "yes")]).unwrap(),
            "Shorts feed on."
        );
        assert!(p.borrow().autoplay() && p.borrow().shorts_mode() && p.borrow().queue().shuffle());
        call(&mut ws, &s, "player.jump", &[("index", "2")]).unwrap();
        assert!(call(&mut ws, &s, "player.rate", &[("verdict", "up")])
            .unwrap()
            .contains("1 verdict"));
        assert!(call(&mut ws, &s, "player.remove", &[("index", "0")])
            .unwrap()
            .contains("2 left"));
        assert_eq!(
            call(&mut ws, &s, "player.clear", &[]).unwrap(),
            "The queue is empty."
        );
        assert!(call(&mut ws, &s, "player.rate", &[("verdict", "down")])
            .unwrap_err()
            .to_string()
            .contains("nothing is playing"));
        assert_eq!(call(&mut ws, &s, "player.stop", &[]).unwrap(), "Stopped.");
    }

    #[test]
    fn bad_input_is_refused_with_a_reason_and_changes_nothing() {
        let (mut ws, p, log, s) = setup();
        for (cap, args, want) in [
            (
                "player.add",
                vec![("url", "https://youtube.com.evil.com/watch?v=dQw4w9WgXcQ")],
                "not a YouTube",
            ),
            (
                "player.add",
                vec![("url", "ytdl://dQw4w9WgXcQ")],
                "not allowed",
            ),
            ("player.add", vec![("url", "relative.mp4")], "full path"),
            (
                "player.add",
                vec![("url", "/m/a.mp4"), ("max", "0")],
                "between 1 and",
            ),
            (
                "player.add",
                vec![("url", "/m/a.mp4"), ("max", "abc")],
                "whole number",
            ),
            ("player.add", vec![("url", "  ")], "required"),
            ("player.seek", vec![("by", "999999999")], "between"),
            ("player.seek", vec![("by", "1.5")], "whole number"),
            ("player.volume", vec![("by", "101")], "between"),
            ("player.speed", vec![("by", "9")], "between"),
            ("player.shuffle", vec![("on", "maybe")], "on or off"),
            ("player.repeat", vec![("mode", "twice")], "off, one or all"),
            (
                "player.rate",
                vec![("verdict", "meh")],
                "up, down or report",
            ),
            ("player.jump", vec![("index", "5")], "no item 5"),
            ("player.remove", vec![("index", "-1")], "between"),
        ] {
            let e = call(&mut ws, &s, cap, &args).unwrap_err().to_string();
            assert!(e.contains(want), "{cap} {args:?}: {e}");
        }
        assert!(
            p.borrow().queue().is_empty() && log.borrow().is_empty(),
            "nothing changed"
        );
    }

    #[test]
    fn a_viewer_can_look_but_not_touch() {
        let (mut ws, _p, log, _s) = setup();
        let v = ws.open_session("acme", "vic", "viewer").unwrap();
        assert!(call(&mut ws, &v, "player.status", &[]).is_ok());
        assert!(call(&mut ws, &v, "player.queue", &[]).is_ok());
        for cap in [
            "player.add",
            "player.play",
            "player.next",
            "player.clear",
            "player.volume",
        ] {
            let r = ws.call(
                "acme",
                &v,
                cap,
                [
                    ("url".to_string(), "/m/a.mp4".to_string()),
                    ("by".to_string(), "1".to_string()),
                ]
                .into_iter()
                .collect(),
            );
            assert!(!matches!(r, Ok(Reply::Done(_))), "{cap} was allowed: {r:?}");
        }
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn the_queue_limit_is_reported_and_status_reads_well_when_idle_or_failed() {
        let (mut ws, p, _log, s) = setup();
        assert!(call(&mut ws, &s, "player.status", &[])
            .unwrap()
            .starts_with("idle, volume 80%, speed 1.00x"));
        for i in 0..mouli_player::queue::MAX_QUEUE {
            p.borrow_mut()
                .queue_mut()
                .push(Item::new(Source::File(format!("/m/{i}.mp4"))));
        }
        assert!(call(&mut ws, &s, "player.add", &[("url", "/m/x.mp4")])
            .unwrap_err()
            .to_string()
            .contains("queue is full"));
        assert_eq!(clock(59), "0:59");
        assert_eq!(clock(3661), "1:01:01");
    }

    #[test]
    fn a_long_title_is_cleaned_by_the_queue_and_never_spreads_into_the_conversation() {
        let (mut ws, _p, _l, s) = setup();
        let big = "t".repeat(5000);
        let out = call(
            &mut ws,
            &s,
            "player.add",
            &[("url", "/m/a.mp4"), ("title", &big)],
        )
        .unwrap();
        assert!(out.len() < 300, "{}", out.len());
        let last = ws
            .session("acme", &s)
            .unwrap()
            .messages()
            .last()
            .unwrap()
            .content
            .clone();
        assert!(last.len() < 400, "{}", last.len());
    }
}

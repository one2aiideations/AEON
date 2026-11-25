# mouli-player

The core of MOULI's video and YouTube player, in plain Rust with no dependencies. It decides what to
play and when; the programs that actually decode video and draw it are added by the OS image.

| Module | What it does |
|---|---|
| `source` | Reads what a person types or pastes: YouTube video, Short or embed links, `yt:ID`, an absolute media path, or an `https` media address. Strict: exact YouTube hosts only (no look-alike domains, user names or ports), 11-character ids, and no `ytdl://`-style schemes. |
| `queue` | The play queue (up to 1000 items) with shuffle, repeat off/one/all and previous. A shuffled repeat-all round never starts with the item just played. |
| `engine` | The player state machine. When an item ends the next one starts by itself; failed items are skipped but after three in a row it stops instead of looping; per-item time limits for previews; volume, mute, speed, seek; thumbs up, down and report. |
| `mpv` | The `mpv` control protocol (one JSON message per line): commands out, events in, with a small bounded JSON reader. `mpv_args` starts mpv with `--ytdl=no`, no scripts and no config. |

## Shorts without scrolling

`Command::ShortsMode(true)` turns on autoplay and passes over ordinary YouTube videos, so a queue of
Shorts plays one after another with no input; with repeat-all it is an endless feed. Down and report
skip the item and are remembered for curation.

## YouTube: what is and is not done

YouTube items are only ever played by YouTube's own web player, with its ads and its rules. This
crate never fetches, extracts or decodes YouTube video, and mpv refuses to (it is started with its
YouTube hook off). **Not built yet:** the part that runs YouTube's player (a small local page driven by
the engine, shown in an embedded browser such as WPE WebKit with `cog`), the mpv and browser packages in
the boot image, sound and video output, and the player screen. Until then YouTube items cannot actually
be shown; the engine and the parsing are tested with fake backends only.

## Tests

`cargo test -p mouli-player` (49 tests: address parsing against look-alike and hostile input, queue and
shuffle properties, the state machine including endless-failure protection, the mpv message reader
against malformed and over-deep input, and a player driven end to end by mpv-style messages).

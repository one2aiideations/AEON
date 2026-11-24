//! Raw keyboard input from a terminal.

use mouli_screen::key::{Key, KeyParser};
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, SetArg, Termios};
use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::time::Duration;

fn to_io(e: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}

/// Puts a terminal into raw mode (no line editing, no echo, no signal keys, so Ctrl-C arrives as
/// a key press) and restores it when dropped.
#[derive(Debug)]
pub struct RawTty {
    fd: OwnedFd,
    saved: Termios,
}

impl RawTty {
    /// Switches the terminal behind `fd` to raw mode.
    pub fn enter(fd: &impl AsFd) -> io::Result<Self> {
        let owned = fd.as_fd().try_clone_to_owned()?;
        let saved = tcgetattr(&owned).map_err(to_io)?;
        let mut raw = saved.clone();
        cfmakeraw(&mut raw);
        tcsetattr(&owned, SetArg::TCSANOW, &raw).map_err(to_io)?;
        Ok(Self { fd: owned, saved })
    }
}

impl Drop for RawTty {
    fn drop(&mut self) {
        let _ = tcsetattr(&self.fd, SetArg::TCSANOW, &self.saved);
    }
}

/// Reads key presses from a file descriptor (a terminal in raw mode).
#[derive(Debug)]
pub struct TtyKeys<F: AsFd> {
    fd: F,
    parser: KeyParser,
}

impl<F: AsFd> TtyKeys<F> {
    /// A key reader on `fd`.
    pub fn new(fd: F) -> Self {
        Self {
            fd,
            parser: KeyParser::new(),
        }
    }

    /// Waits up to `timeout` for input and returns the keys it completed. When the wait ends with
    /// nothing new, a held-back lone Escape becomes a real Escape. An error of kind
    /// `UnexpectedEof` means the terminal closed.
    pub fn next(&mut self, timeout: Duration) -> io::Result<Vec<Key>> {
        let ms = u16::try_from(timeout.as_millis()).unwrap_or(u16::MAX);
        let ready = {
            let mut fds = [PollFd::new(self.fd.as_fd(), PollFlags::POLLIN)];
            poll(&mut fds, PollTimeout::from(ms)).map_err(to_io)?;
            fds[0].revents().is_some_and(|r| {
                r.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
            })
        };
        if !ready {
            return Ok(self.parser.flush());
        }
        let mut buf = [0u8; 256];
        let n = nix::unistd::read(self.fd.as_fd().as_raw_fd(), &mut buf).map_err(to_io)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the terminal closed",
            ));
        }
        Ok(self.parser.feed(&buf[..n]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::pty::openpty;
    use std::io::Write;

    fn pair() -> (std::fs::File, OwnedFd) {
        let p = openpty(None, None).unwrap();
        (std::fs::File::from(p.master), p.slave)
    }

    const SHORT: Duration = Duration::from_millis(30);

    #[test]
    fn keys_typed_on_the_master_arrive_as_keys() {
        let (mut master, slave) = pair();
        let _raw = RawTty::enter(&slave).unwrap();
        let mut keys = TtyKeys::new(slave);
        master.write_all(b"hi\r\x1b[A\x1b[[B").unwrap();
        let got = keys.next(Duration::from_millis(500)).unwrap();
        assert_eq!(
            got,
            vec![
                Key::Char('h'),
                Key::Char('i'),
                Key::Enter,
                Key::Up,
                Key::F(2)
            ]
        );
    }

    #[test]
    fn no_input_means_no_keys_after_the_timeout() {
        let (_master, slave) = pair();
        let _raw = RawTty::enter(&slave).unwrap();
        let mut keys = TtyKeys::new(slave);
        let start = std::time::Instant::now();
        assert_eq!(keys.next(SHORT).unwrap(), Vec::<Key>::new());
        assert!(start.elapsed() >= Duration::from_millis(25));
    }

    #[test]
    fn a_lone_escape_becomes_a_key_after_a_quiet_moment() {
        let (mut master, slave) = pair();
        let _raw = RawTty::enter(&slave).unwrap();
        let mut keys = TtyKeys::new(slave);
        master.write_all(b"\x1b").unwrap();
        assert_eq!(
            keys.next(Duration::from_millis(200)).unwrap(),
            Vec::<Key>::new(),
            "held back: it may start a sequence"
        );
        assert_eq!(keys.next(SHORT).unwrap(), vec![Key::Esc]);
    }

    #[test]
    fn a_sequence_split_across_reads_is_reassembled() {
        let (mut master, slave) = pair();
        let _raw = RawTty::enter(&slave).unwrap();
        let mut keys = TtyKeys::new(slave);
        master.write_all(b"\x1b[").unwrap();
        assert!(keys.next(Duration::from_millis(100)).unwrap().is_empty());
        master.write_all(b"B").unwrap();
        assert_eq!(
            keys.next(Duration::from_millis(500)).unwrap(),
            vec![Key::Down]
        );
    }

    #[test]
    fn raw_mode_delivers_ctrl_c_as_a_key_not_a_signal() {
        let (mut master, slave) = pair();
        let _raw = RawTty::enter(&slave).unwrap();
        let mut keys = TtyKeys::new(slave);
        master.write_all(&[0x03]).unwrap();
        assert_eq!(
            keys.next(Duration::from_millis(500)).unwrap(),
            vec![Key::Ctrl('c')]
        );
    }

    #[test]
    fn input_is_not_held_back_until_enter() {
        // Without raw mode the line discipline would wait for a newline before delivering.
        let (mut master, slave) = pair();
        let _raw = RawTty::enter(&slave).unwrap();
        let mut keys = TtyKeys::new(slave);
        master.write_all(b"x").unwrap();
        assert_eq!(
            keys.next(Duration::from_millis(500)).unwrap(),
            vec![Key::Char('x')]
        );
    }

    #[test]
    fn dropping_the_guard_restores_the_terminal_settings() {
        let (_master, slave) = pair();
        let before = tcgetattr(&slave).unwrap();
        {
            let _raw = RawTty::enter(&slave).unwrap();
            let during = tcgetattr(&slave).unwrap();
            assert_ne!(before.local_flags, during.local_flags);
        }
        assert_eq!(tcgetattr(&slave).unwrap().local_flags, before.local_flags);
    }

    #[test]
    fn a_closed_terminal_is_reported() {
        let (master, slave) = pair();
        let _raw = RawTty::enter(&slave).unwrap();
        let mut keys = TtyKeys::new(slave);
        drop(master);
        let err = keys.next(Duration::from_millis(500)).unwrap_err();
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::UnexpectedEof | io::ErrorKind::Other
            ) || err.raw_os_error().is_some(),
            "{err:?}"
        );
    }

    #[test]
    fn entering_raw_mode_on_something_that_is_not_a_terminal_fails() {
        let f = std::fs::File::open("/dev/null").unwrap();
        assert!(RawTty::enter(&f).is_err());
        let _ = f.as_raw_fd();
    }
}

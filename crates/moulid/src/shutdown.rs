//! Catching "please stop" signals (SIGTERM, SIGHUP) without unsafe code.
//!
//! The signals are blocked before any thread exists, so every thread inherits the block; a
//! watcher thread then reads them from a signal file descriptor and runs a callback, which asks
//! the desktop to leave cleanly (so the console is put back).

use nix::sys::signal::{SigSet, Signal};
use nix::sys::signalfd::SignalFd;

fn mask_of(signals: &[Signal]) -> SigSet {
    let mut m = SigSet::empty();
    for s in signals {
        m.add(*s);
    }
    m
}

/// The signals that mean "stop".
pub const STOP_SIGNALS: [Signal; 2] = [Signal::SIGTERM, Signal::SIGHUP];

/// Blocks the stop signals for this thread and every thread started after it. Call first thing
/// in `main`, before any thread is started; otherwise a signal can still kill the program.
///
/// # Errors
/// A message when the signal mask cannot be changed.
pub fn block_stop_signals() -> Result<(), String> {
    mask_of(&STOP_SIGNALS)
        .thread_block()
        .map_err(|e| e.to_string())
}

/// Something that waits for blocked signals.
pub struct Watch {
    fd: SignalFd,
}

impl Watch {
    /// Starts watching `signals` (they must be blocked, see [`block_stop_signals`]).
    ///
    /// # Errors
    /// A message when the signal descriptor cannot be made.
    pub fn new(signals: &[Signal]) -> Result<Self, String> {
        let mask = mask_of(signals);
        mask.thread_block().map_err(|e| e.to_string())?;
        Ok(Self {
            fd: SignalFd::new(&mask).map_err(|e| e.to_string())?,
        })
    }

    /// Waits for the next signal and returns its number; `None` when the descriptor fails.
    pub fn wait(&mut self) -> Option<i32> {
        loop {
            match self.fd.read_signal() {
                Ok(Some(info)) => return i32::try_from(info.ssi_signo).ok(),
                Ok(None) => {}
                Err(nix::errno::Errno::EINTR) => {}
                Err(_) => return None,
            }
        }
    }
}

/// Runs `on_stop` once, on its own thread, when a stop signal arrives.
///
/// # Errors
/// A message when the watcher cannot start.
pub fn on_stop_signal(on_stop: impl FnOnce() + Send + 'static) -> Result<(), String> {
    let mut watch = Watch::new(&STOP_SIGNALS)?;
    std::thread::Builder::new()
        .name("stop-signal".into())
        .spawn(move || {
            if watch.wait().is_some() {
                on_stop();
            }
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::signal::raise;

    #[test]
    fn a_blocked_signal_is_read_from_the_descriptor_instead_of_killing_the_program() {
        // SIGUSR1 stands in for SIGTERM; raise() targets this thread, which blocked it, so the
        // test cannot hurt other threads.
        let mut w = Watch::new(&[Signal::SIGUSR1, Signal::SIGUSR2]).unwrap();
        raise(Signal::SIGUSR2).unwrap();
        assert_eq!(w.wait(), Some(Signal::SIGUSR2 as i32));
        raise(Signal::SIGUSR1).unwrap();
        raise(Signal::SIGUSR1).unwrap();
        assert_eq!(w.wait(), Some(Signal::SIGUSR1 as i32));
    }

    #[test]
    fn the_stop_signals_are_term_and_hup() {
        assert!(STOP_SIGNALS.contains(&Signal::SIGTERM));
        assert!(STOP_SIGNALS.contains(&Signal::SIGHUP));
        assert!(block_stop_signals().is_ok());
    }
}

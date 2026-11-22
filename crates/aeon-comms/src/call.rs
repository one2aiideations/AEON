//! Call signalling state machine (voice and video).
//!
//! Signals carry session descriptions and network candidates as opaque
//! strings; the media engine interprets them. If both sides call each other
//! at once ("glare"), the side with the lower peer id keeps its offer.

/// Peer id.
pub type PeerId = u32;

/// Media wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Media {
    /// Audio on.
    pub audio: bool,
    /// Video on.
    pub video: bool,
}

/// Signalling message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// Start a call with a session offer.
    Offer {
        /// Call id.
        call: u64,
        /// Media requested.
        media: Media,
        /// Session description.
        sdp: String,
    },
    /// Accept with a session answer.
    Answer {
        /// Call id.
        call: u64,
        /// Session description.
        sdp: String,
    },
    /// Extra network candidate.
    Candidate {
        /// Call id.
        call: u64,
        /// Candidate line.
        candidate: String,
    },
    /// Refuse or end.
    Hangup {
        /// Call id.
        call: u64,
        /// Reason.
        reason: String,
    },
}

/// Call state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// No call.
    Idle,
    /// We offered, waiting for an answer since `since`.
    Calling {
        /// Call id.
        call: u64,
        /// Start time.
        since: u64,
    },
    /// Peer offered; waiting for the user since `since`.
    Ringing {
        /// Call id.
        call: u64,
        /// Offer description.
        sdp: String,
        /// Requested media.
        media: Media,
        /// Start time.
        since: u64,
    },
    /// Media flowing.
    Connected {
        /// Call id.
        call: u64,
    },
    /// Finished with a reason.
    Ended {
        /// Reason.
        reason: String,
    },
}

/// One side of a call with one peer.
#[derive(Debug, Clone)]
pub struct CallSession {
    me: PeerId,
    peer: PeerId,
    state: State,
    /// Candidates received for the current call.
    pub candidates: Vec<String>,
    /// Seconds before an unanswered call ends.
    pub ring_timeout: u64,
}

impl CallSession {
    /// Idle session between `me` and `peer`.
    #[must_use]
    pub fn new(me: PeerId, peer: PeerId) -> Self {
        Self {
            me,
            peer,
            state: State::Idle,
            candidates: Vec::new(),
            ring_timeout: 45,
        }
    }

    /// Current state.
    #[must_use]
    pub fn state(&self) -> &State {
        &self.state
    }

    fn idle(&self) -> bool {
        matches!(self.state, State::Idle | State::Ended { .. })
    }

    fn current(&self) -> Option<u64> {
        match self.state {
            State::Calling { call, .. }
            | State::Ringing { call, .. }
            | State::Connected { call } => Some(call),
            _ => None,
        }
    }

    /// Place a call; returns the offer to send.
    pub fn dial(&mut self, call: u64, media: Media, sdp: &str, now: u64) -> Option<Signal> {
        if !self.idle() {
            return None;
        }
        self.candidates.clear();
        self.state = State::Calling { call, since: now };
        Some(Signal::Offer {
            call,
            media,
            sdp: sdp.to_owned(),
        })
    }

    /// User accepts a ringing call; returns the answer to send.
    pub fn accept(&mut self, sdp: &str) -> Option<Signal> {
        let State::Ringing { call, .. } = self.state else {
            return None;
        };
        self.state = State::Connected { call };
        Some(Signal::Answer {
            call,
            sdp: sdp.to_owned(),
        })
    }

    /// Hang up or decline; returns the signal to send.
    pub fn hangup(&mut self, reason: &str) -> Option<Signal> {
        let call = self.current()?;
        self.state = State::Ended {
            reason: reason.to_owned(),
        };
        Some(Signal::Hangup {
            call,
            reason: reason.to_owned(),
        })
    }

    /// Handle a signal from the peer. May return a reply to send.
    pub fn on_signal(&mut self, s: Signal, now: u64) -> Option<Signal> {
        match s {
            Signal::Offer { call, media, sdp } => match self.state {
                State::Idle | State::Ended { .. } => {
                    self.candidates.clear();
                    self.state = State::Ringing {
                        call,
                        sdp,
                        media,
                        since: now,
                    };
                    None
                }
                // Glare: lower id keeps its own offer; higher id yields.
                State::Calling { .. } if self.me > self.peer => {
                    self.candidates.clear();
                    self.state = State::Ringing {
                        call,
                        sdp,
                        media,
                        since: now,
                    };
                    None
                }
                _ => Some(Signal::Hangup {
                    call,
                    reason: "busy".into(),
                }),
            },
            Signal::Answer { call, .. } => {
                if matches!(self.state, State::Calling { call: c, .. } if c == call) {
                    self.state = State::Connected { call };
                }
                None
            }
            Signal::Candidate { call, candidate } => {
                if self.current() == Some(call) {
                    self.candidates.push(candidate);
                }
                None
            }
            Signal::Hangup { call, reason } => {
                if self.current() == Some(call) {
                    self.state = State::Ended { reason };
                }
                None
            }
        }
    }

    /// End unanswered calls older than `ring_timeout`; returns the hangup to send.
    pub fn tick(&mut self, now: u64) -> Option<Signal> {
        let since = match self.state {
            State::Calling { since, .. } | State::Ringing { since, .. } => since,
            _ => return None,
        };
        if now.saturating_sub(since) >= self.ring_timeout {
            self.hangup("no answer")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AV: Media = Media {
        audio: true,
        video: true,
    };

    #[test]
    fn call_flow() {
        let mut a = CallSession::new(1, 2);
        let mut b = CallSession::new(2, 1);
        let offer = a.dial(7, AV, "sdp-a", 0).unwrap();
        assert!(b.on_signal(offer, 0).is_none());
        assert!(matches!(b.state(), State::Ringing { call: 7, .. }));
        b.on_signal(
            Signal::Candidate {
                call: 7,
                candidate: "c1".into(),
            },
            1,
        );
        let answer = b.accept("sdp-b").unwrap();
        a.on_signal(answer, 2);
        assert_eq!(a.state(), &State::Connected { call: 7 });
        assert_eq!(b.candidates, vec!["c1".to_string()]);
        let bye = a.hangup("done").unwrap();
        b.on_signal(bye, 3);
        assert!(matches!(b.state(), State::Ended { .. }));
    }

    #[test]
    fn glare_busy_and_timeout() {
        let mut a = CallSession::new(1, 2);
        let mut b = CallSession::new(2, 1);
        let oa = a.dial(10, AV, "a", 0).unwrap();
        let ob = b.dial(20, AV, "b", 0).unwrap();
        // a (lower id) keeps its call and rejects b's offer; b yields to a's.
        assert!(matches!(
            a.on_signal(ob, 0),
            Some(Signal::Hangup { call: 20, .. })
        ));
        assert!(b.on_signal(oa, 0).is_none());
        assert!(matches!(b.state(), State::Ringing { call: 10, .. }));

        let mut c = CallSession::new(3, 4);
        c.dial(1, AV, "x", 100).unwrap();
        assert!(c.tick(120).is_none());
        assert!(matches!(c.tick(145), Some(Signal::Hangup { .. })));
        assert!(
            c.dial(2, AV, "y", 200).is_some(),
            "can call again after end"
        );
    }
}

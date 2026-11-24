//! Incremental bounded stream framing for the AgentOS replication wire protocol.
//!
//! Accepts arbitrary byte fragments, reconstructs complete protocol frames, and
//! queues decoded messages. It performs no socket I/O and owns no transport.

use crate::wire::{decode, CodecError, Limits, Message};

const MAGIC: &[u8; 4] = b"AMRP";
const HEADER_LEN: usize = 10;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamError {
    InvalidLimits,
    BadMagic,
    UnsupportedVersion(u8),
    FrameTooLarge,
    LengthOverflow,
    Protocol(CodecError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeedOutcome {
    /// Number of input bytes consumed. The caller retains the unconsumed suffix.
    pub consumed: usize,
    /// True when the completed-message queue is full.
    pub backpressured: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamDecoder {
    limits: Limits,
    max_ready_messages: usize,
    frame_buffer: Vec<u8>,
    expected_frame_len: Option<usize>,
    ready: Vec<Message>,
}

impl StreamDecoder {
    pub fn new(limits: Limits, max_ready_messages: usize) -> Result<Self, StreamError> {
        if limits.max_frame_bytes < HEADER_LEN + 4
            || limits.max_frame_bytes > u32::MAX as usize + HEADER_LEN + 4
            || max_ready_messages == 0
        {
            return Err(StreamError::InvalidLimits);
        }
        Ok(Self {
            limits,
            max_ready_messages,
            frame_buffer: Vec::new(),
            expected_frame_len: None,
            ready: Vec::new(),
        })
    }

    pub fn buffered_bytes(&self) -> usize {
        self.frame_buffer.len()
    }
    pub fn ready_messages(&self) -> usize {
        self.ready.len()
    }
    pub fn has_partial_frame(&self) -> bool {
        !self.frame_buffer.is_empty()
    }

    /// Feed as much of `input` as the bounded ready queue permits.
    ///
    /// If `backpressured` is true, call `drain_ready` and retry with the
    /// unconsumed suffix. A malformed frame clears the partial frame buffer.
    pub fn feed(&mut self, input: &[u8]) -> Result<FeedOutcome, StreamError> {
        let mut consumed = 0_usize;

        while consumed < input.len() {
            if self.ready.len() >= self.max_ready_messages {
                return Ok(FeedOutcome {
                    consumed,
                    backpressured: true,
                });
            }

            let byte = match input.get(consumed) {
                Some(value) => *value,
                None => {
                    return Ok(FeedOutcome {
                        consumed,
                        backpressured: false,
                    })
                }
            };
            self.frame_buffer.push(byte);
            consumed = consumed.saturating_add(1);

            if self.frame_buffer.len() == HEADER_LEN {
                self.inspect_header()?;
            }

            if let Some(expected) = self.expected_frame_len {
                if self.frame_buffer.len() == expected {
                    let message = match decode(&self.frame_buffer, &self.limits) {
                        Ok(value) => value,
                        Err(error) => {
                            self.reset_partial();
                            return Err(StreamError::Protocol(error));
                        }
                    };
                    self.ready.push(message);
                    self.reset_partial();
                } else if self.frame_buffer.len() > expected {
                    self.reset_partial();
                    return Err(StreamError::LengthOverflow);
                }
            }
        }

        Ok(FeedOutcome {
            consumed,
            backpressured: false,
        })
    }

    /// Removes up to `maximum` completed messages, preserving their wire order.
    pub fn drain_ready(&mut self, maximum: usize) -> Vec<Message> {
        let count = maximum.min(self.ready.len());
        self.ready.drain(..count).collect()
    }

    /// Discards an incomplete frame, but leaves already decoded messages intact.
    pub fn discard_partial_frame(&mut self) {
        self.reset_partial();
    }

    fn inspect_header(&mut self) -> Result<(), StreamError> {
        if self.frame_buffer.get(0..4) != Some(MAGIC.as_slice()) {
            self.reset_partial();
            return Err(StreamError::BadMagic);
        }
        let version = self
            .frame_buffer
            .get(4)
            .copied()
            .ok_or(StreamError::LengthOverflow)?;
        if version != 1 {
            self.reset_partial();
            return Err(StreamError::UnsupportedVersion(version));
        }
        let b4 = self
            .frame_buffer
            .get(6)
            .copied()
            .ok_or(StreamError::LengthOverflow)?;
        let b5 = self
            .frame_buffer
            .get(7)
            .copied()
            .ok_or(StreamError::LengthOverflow)?;
        let b6 = self
            .frame_buffer
            .get(8)
            .copied()
            .ok_or(StreamError::LengthOverflow)?;
        let b7 = self
            .frame_buffer
            .get(9)
            .copied()
            .ok_or(StreamError::LengthOverflow)?;
        let body_len_u32 = u32::from_be_bytes([b4, b5, b6, b7]);
        let body_len = usize::try_from(body_len_u32).map_err(|_| StreamError::LengthOverflow)?;
        let total = HEADER_LEN
            .checked_add(body_len)
            .and_then(|value| value.checked_add(4))
            .ok_or(StreamError::LengthOverflow)?;
        if total > self.limits.max_frame_bytes {
            self.reset_partial();
            return Err(StreamError::FrameTooLarge);
        }
        self.expected_frame_len = Some(total);
        Ok(())
    }

    fn reset_partial(&mut self) {
        self.frame_buffer.clear();
        self.expected_frame_len = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::encode;

    fn sample(id: u64) -> Message {
        Message::Acknowledge {
            tenant_id: "tenant-x".to_owned(),
            request_id: id,
            through_sequence: id.saturating_add(10),
        }
    }

    fn decoder(queue: usize) -> StreamDecoder {
        match StreamDecoder::new(Limits::default(), queue) {
            Ok(value) => value,
            Err(_) => panic!("valid test limits rejected"),
        }
    }

    #[test]
    fn reconstructs_frame_split_across_many_fragments() {
        let limits = Limits::default();
        let frame = match encode(&sample(1), &limits) {
            Ok(value) => value,
            Err(_) => return,
        };
        let mut stream = decoder(4);
        for byte in &frame {
            let result = stream.feed(std::slice::from_ref(byte));
            assert!(result.is_ok());
        }
        assert_eq!(stream.drain_ready(4), vec![sample(1)]);
        assert!(!stream.has_partial_frame());
    }

    #[test]
    fn decodes_multiple_frames_from_one_input_chunk() {
        let limits = Limits::default();
        let mut input = match encode(&sample(1), &limits) {
            Ok(value) => value,
            Err(_) => return,
        };
        let second = match encode(&sample(2), &limits) {
            Ok(value) => value,
            Err(_) => return,
        };
        input.extend_from_slice(&second);

        let mut stream = decoder(4);
        let outcome = stream.feed(&input);
        assert_eq!(
            outcome,
            Ok(FeedOutcome {
                consumed: input.len(),
                backpressured: false
            })
        );
        assert_eq!(stream.drain_ready(4), vec![sample(1), sample(2)]);
    }

    #[test]
    fn applies_backpressure_without_losing_unconsumed_bytes() {
        let limits = Limits::default();
        let mut input = match encode(&sample(1), &limits) {
            Ok(value) => value,
            Err(_) => return,
        };
        let second = match encode(&sample(2), &limits) {
            Ok(value) => value,
            Err(_) => return,
        };
        input.extend_from_slice(&second);

        let mut stream = decoder(1);
        let first = stream.feed(&input);
        assert!(first.is_ok());
        let first = match first {
            Ok(value) => value,
            Err(_) => return,
        };
        assert!(first.backpressured);
        assert!(first.consumed < input.len());
        assert_eq!(stream.drain_ready(1), vec![sample(1)]);

        let remaining = input.get(first.consumed..).unwrap_or(&[]);
        let second_result = stream.feed(remaining);
        assert!(second_result.is_ok());
        assert_eq!(stream.drain_ready(1), vec![sample(2)]);
    }

    #[test]
    fn rejects_bad_magic_and_resets_partial_state() {
        let mut stream = decoder(2);
        let result = stream.feed(b"NOPE");
        assert_eq!(
            result,
            Ok(FeedOutcome {
                consumed: 4,
                backpressured: false
            })
        );
        let result = stream.feed(&[0, 1, 0, 0, 0, 0]);
        assert_eq!(result, Err(StreamError::BadMagic));
        assert_eq!(stream.buffered_bytes(), 0);
    }

    #[test]
    fn explicit_partial_discard_preserves_ready_queue() {
        let limits = Limits::default();
        let frame = match encode(&sample(7), &limits) {
            Ok(value) => value,
            Err(_) => return,
        };
        let mut stream = decoder(2);
        assert!(stream.feed(&frame).is_ok());
        assert!(stream.feed(b"AMR").is_ok());
        stream.discard_partial_frame();
        assert_eq!(stream.drain_ready(2), vec![sample(7)]);
        assert_eq!(stream.buffered_bytes(), 0);
    }
}

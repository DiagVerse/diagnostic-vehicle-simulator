//! An in-memory serial transport: whatever is written to one end can be read from the other.
//!
//! This is what lets the bridge, the SLCAN codec and the ISO-TP transmit path be tested end to
//! end with no hardware and no operating system involved — the tests drive the far end
//! directly and assert on the exact bytes the engine put on the wire.

#![allow(non_snake_case, non_upper_case_globals)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::{SerialError, SerialTransport};

/// The bytes travelling in one direction.
type Pipe = Arc<Mutex<VecDeque<u8>>>;

/// One end of a loopback pair.
pub struct LoopbackTransport {
    m_strName: String,
    /// Bytes this end writes.
    m_arcOutgoing: Pipe,
    /// Bytes this end reads, which is the other end's outgoing pipe.
    m_arcIncoming: Pipe,
}

impl LoopbackTransport {
    /// Create a connected pair. What the first writes, the second reads, and vice versa.
    pub fn NewPair() -> (LoopbackTransport, LoopbackTransport) {
        let arcLeftToRight: Pipe = Arc::new(Mutex::new(VecDeque::new()));
        let arcRightToLeft: Pipe = Arc::new(Mutex::new(VecDeque::new()));

        let left = LoopbackTransport {
            m_strName: "loopback-a".to_string(),
            m_arcOutgoing: Arc::clone(&arcLeftToRight),
            m_arcIncoming: Arc::clone(&arcRightToLeft),
        };
        let right = LoopbackTransport {
            m_strName: "loopback-b".to_string(),
            m_arcOutgoing: arcRightToLeft,
            m_arcIncoming: arcLeftToRight,
        };
        (left, right)
    }

    /// Everything this end has been sent and not yet read, as a string. For assertions.
    pub fn TakeIncomingAsText(&mut self) -> String {
        let mut queue = self.m_arcIncoming.lock().expect("loopback pipe poisoned");
        let vecBytes: Vec<u8> = queue.drain(..).collect();
        String::from_utf8_lossy(&vecBytes).into_owned()
    }
}

impl SerialTransport for LoopbackTransport {
    fn Name(&self) -> &str {
        &self.m_strName
    }

    fn Write(&mut self, vecBytes: &[u8]) -> Result<(), SerialError> {
        let mut queue = self.m_arcOutgoing.lock().expect("loopback pipe poisoned");
        queue.extend(vecBytes.iter().copied());
        Ok(())
    }

    fn Read(&mut self, vecBuffer: &mut [u8]) -> Result<usize, SerialError> {
        let mut queue = self.m_arcIncoming.lock().expect("loopback pipe poisoned");

        let uCount = queue.len().min(vecBuffer.len());
        for byteSlot in vecBuffer.iter_mut().take(uCount) {
            *byteSlot = queue.pop_front().expect("just checked the length");
        }
        Ok(uCount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_one_end_writes_the_other_reads() {
        let (mut left, mut right) = LoopbackTransport::NewPair();

        left.Write(b"t7E0210030\r").expect("write");

        let mut arrBuffer = [0u8; 64];
        let uCount = right.Read(&mut arrBuffer).expect("read");
        assert_eq!(&arrBuffer[..uCount], b"t7E0210030\r");

        // And nothing came back the other way.
        assert_eq!(left.Read(&mut arrBuffer).expect("read"), 0);
    }

    #[test]
    fn an_idle_pipe_reads_nothing_rather_than_failing() {
        let (mut left, _right) = LoopbackTransport::NewPair();
        let mut arrBuffer = [0u8; 8];
        assert_eq!(left.Read(&mut arrBuffer).expect("read"), 0);
    }
}

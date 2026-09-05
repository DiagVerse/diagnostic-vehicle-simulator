//! Watching the frames that cross the bridge.
//!
//! A port, in the same spirit as `CanBusPort` and `ProtocolHandler`: the bridge announces what
//! it sees and knows nothing about who is listening. That keeps this crate independent of the
//! HTTP layer that currently does the listening — the bridge must not learn about SSE, and a
//! test observer must be as easy to attach as the real one.

use can::CanFrame;
use simulation::RoutingOutcome;

/// Which way a frame travelled, from the simulator's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameDirection {
    /// Into the simulator: sent by whatever is at the far end of the link.
    Received,
    /// Out of the simulator: an ECU answering.
    Sent,
}

impl FrameDirection {
    /// A short name for a log or a wire format.
    pub fn Name(self) -> &'static str {
        match self {
            FrameDirection::Received => "rx",
            FrameDirection::Sent => "tx",
        }
    }
}

/// Something that wants to see every frame crossing the bridge.
///
/// Implementations must not block: this is called from the bridge's polling loop, and time
/// spent here is time the simulator is not answering a tester. Hand the frame to a channel and
/// return.
pub trait FrameObserver: Send + Sync {
    /// One frame, as it went past.
    fn OnFrame(&self, direction: FrameDirection, frame: &CanFrame);

    /// One complete request, reassembled, with what the simulation decided about it.
    ///
    /// Frames alone under-serve a reader: by the time the bridge calls this it has already put
    /// the segments back together and knows whether the request was routed, broadcast, or met
    /// with silence and why. Reporting only the frames would make the monitor throw that away
    /// and ask the reader to redo it by eye.
    ///
    /// Defaulted to nothing so an observer that only cares about frames stays a one-method
    /// implementation.
    fn OnExchange(&self, _u32RequestCanId: u32, _vecRequest: &[u8], _outcome: &RoutingOutcome) {}
}

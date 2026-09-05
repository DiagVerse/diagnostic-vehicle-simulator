//! The live traffic feed: everything crossing the simulator, streamed to whoever is watching.
//!
//! Two things were previously invisible. Frames crossing the CAN bridge were counted and
//! nothing more — a real tester could hold a whole session with the simulation and the only
//! evidence was two numbers going up. And an exchange driven from the UI was visible only to
//! the browser tab that sent it.
//!
//! This makes both observable over `GET /events`, as Server-Sent Events.
//!
//! # Why a broadcast channel, and what happens when it overflows
//!
//! The channel is bounded. A monitor that cannot keep up must not be allowed to make the
//! simulator slow or make it allocate without limit — answering a tester on time is the job,
//! and watching is a convenience. So a slow receiver is *told* it fell behind, with the count,
//! rather than being quietly given an incomplete picture. A gap you can see is debuggable; a
//! gap you cannot is a bug hunt.

#![allow(non_snake_case, non_upper_case_globals)]

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use bridge::observer::{FrameDirection, FrameObserver};
use can::CanFrame;
use futures_core::Stream;
use serde::Serialize;
use simulation::RoutingOutcome;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::AppState;

/// How many events the channel holds before the slowest receiver starts losing them.
///
/// A busy flashing sequence is a few hundred frames a second, so this is a couple of seconds of
/// grace for a monitor that stalls — long enough to survive a browser repaint, short enough
/// that a monitor left behind cannot pin memory.
const c_uChannelCapacity: usize = 2048;

/// How often to send a keep-alive comment when nothing is happening.
///
/// Idle SSE connections are dropped by proxies and by some browsers. This is invisible to the
/// reader and keeps a monitor open through a quiet period.
const c_keepAliveInterval: Duration = Duration::from_secs(15);

/// One thing that happened, as the monitor sees it.
///
/// Tagged by `kind` so a reader can switch on it without guessing from which fields are
/// present. Every variant carries `atMs` — wall-clock milliseconds, because the reader is a
/// person comparing this against their own tester's log.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TrafficEvent {
    /// One CAN frame crossing the hardware bridge.
    #[serde(rename_all = "camelCase")]
    Frame {
        at_ms: u64,
        /// "rx" for a frame from the far end, "tx" for one the simulator sent.
        direction: String,
        can_id_hex: String,
        data_hex: String,
        /// Payload length. Sent explicitly so a reader need not count the hex.
        length: usize,
        /// True for a frame the simulator would treat as ISO-TP flow control.
        is_flow_control: bool,
    },

    /// One request routed through the simulation, with what answered it.
    #[serde(rename_all = "camelCase")]
    Exchange {
        at_ms: u64,
        can_id_hex: String,
        request_hex: String,
        /// "physical", "functional", "unrouted", "stopped" or "silenced".
        addressing: String,
        routed: bool,
        /// One entry per ECU that answered.
        responses: Vec<ExchangeResponse>,
        /// Why nothing answered, when that is the interesting part.
        reason: Option<String>,
    },

    /// The simulation was loaded, started, stopped or cleared.
    #[serde(rename_all = "camelCase")]
    Lifecycle { at_ms: u64, what: String },

    /// This monitor fell behind and missed events.
    ///
    /// Reported rather than hidden: a monitor showing a gap it does not mention is worse than
    /// one that says "you missed 412 events here".
    #[serde(rename_all = "camelCase")]
    Lagged { at_ms: u64, missed: u64 },
}

/// One ECU's answer inside an exchange event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExchangeResponse {
    pub ecu_name: String,
    pub response_can_id_hex: String,
    pub response_hex: String,
    pub suppressed: bool,
}

/// Wall-clock milliseconds. Falls back to zero rather than panicking if the clock is before the
/// epoch, which is a broken machine rather than something to take the engine down for.
pub fn NowMs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// The publisher every part of the engine hands events to.
///
/// Cloneable and cheap: it is a broadcast sender. Publishing with no subscribers is not an
/// error and is not logged — an engine nobody is watching is the normal case.
#[derive(Clone)]
pub struct TrafficChannel {
    m_sender: broadcast::Sender<TrafficEvent>,
}

impl TrafficChannel {
    pub fn New() -> Self {
        let (sender, _receiver) = broadcast::channel(c_uChannelCapacity);
        TrafficChannel { m_sender: sender }
    }

    /// Publish one event. Silently does nothing when nobody is listening.
    pub fn Publish(&self, event: TrafficEvent) {
        let _ = self.m_sender.send(event);
    }

    /// Start listening.
    pub fn Subscribe(&self) -> broadcast::Receiver<TrafficEvent> {
        self.m_sender.subscribe()
    }

    /// How many monitors are attached, for the status display.
    pub fn SubscriberCount(&self) -> usize {
        self.m_sender.receiver_count()
    }
}

impl Default for TrafficChannel {
    fn default() -> Self {
        TrafficChannel::New()
    }
}

/// Lets the CAN bridge announce frames without knowing anything about HTTP.
impl FrameObserver for TrafficChannel {
    fn OnFrame(&self, direction: FrameDirection, frame: &CanFrame) {
        self.Publish(TrafficEvent::Frame {
            at_ms: NowMs(),
            direction: direction.Name().to_string(),
            can_id_hex: FormatCanId(frame.m_u32CanId),
            data_hex: FormatHexBytes(&frame.m_vecData),
            length: frame.m_vecData.len(),
            is_flow_control: IsFlowControlFrame(frame),
        });
    }

    fn OnExchange(&self, u32RequestCanId: u32, vecRequest: &[u8], outcome: &RoutingOutcome) {
        let (strAddressing, bRouted, vecResponses, optStrReason) = DescribeOutcome(outcome);

        self.Publish(TrafficEvent::Exchange {
            at_ms: NowMs(),
            can_id_hex: FormatCanId(u32RequestCanId),
            request_hex: FormatHexBytes(vecRequest),
            addressing: strAddressing,
            routed: bRouted,
            responses: vecResponses,
            reason: optStrReason,
        });
    }
}

/// Turn a routing outcome into the four things the monitor shows about it.
///
/// Silence is described rather than left blank: "no ECU listens on that identifier", "the
/// simulation is stopped" and "that ECU is switched off" look identical on a wire and are three
/// entirely different problems.
fn DescribeOutcome(
    outcome: &RoutingOutcome,
) -> (String, bool, Vec<ExchangeResponse>, Option<String>) {
    match outcome {
        RoutingOutcome::Stopped => (
            "stopped".to_string(),
            false,
            Vec::new(),
            Some("the simulation is stopped; every ECU is off the bus".to_string()),
        ),
        RoutingOutcome::NoTarget => (
            "unrouted".to_string(),
            false,
            Vec::new(),
            Some("no ECU listens on that identifier".to_string()),
        ),
        RoutingOutcome::Silenced {
            strEcuName,
            strReason,
        } => (
            "silenced".to_string(),
            false,
            Vec::new(),
            Some(format!("{strEcuName}: {strReason}")),
        ),
        RoutingOutcome::Handled(vecRouted) => {
            let vecResponses = vecRouted
                .iter()
                .map(|routed| ExchangeResponse {
                    ecu_name: routed.m_strEcuName.clone(),
                    response_can_id_hex: FormatCanId(routed.m_u32ResponseCanId),
                    response_hex: FormatHexBytes(&routed.m_vecResponse),
                    suppressed: routed.IsSuppressed(),
                })
                .collect();

            // More than one answer means the request was addressed to the broadcast identifier
            // and several ECUs replied.
            let strAddressing = if vecRouted.len() > 1 {
                "functional".to_string()
            } else {
                "physical".to_string()
            };
            (strAddressing, true, vecResponses, None)
        }
    }
}

/// GET /events — every frame and every exchange, as Server-Sent Events.
///
/// SSE rather than a WebSocket: this is one-way, and SSE reconnects on its own, survives a
/// proxy, and needs no protocol upgrade. A monitor that loses the engine reattaches without
/// anyone writing reconnect logic.
pub async fn GetEvents(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    tracing::info!(
        monitors = state.traffic.SubscriberCount() + 1,
        "a traffic monitor attached"
    );

    let stream = BuildEventStream(state.traffic.Subscribe());
    Sse::new(stream).keep_alive(KeepAlive::new().interval(c_keepAliveInterval))
}

/// Turn the broadcast receiver into a stream of SSE events.
///
/// A receiver that falls behind yields a `Lagged` event carrying the count rather than ending
/// the stream: the monitor stays attached and says what it missed.
fn BuildEventStream(
    receiver: broadcast::Receiver<TrafficEvent>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    BroadcastStream::new(receiver).map(|result| {
        let event = match result {
            Ok(event) => event,
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(uMissed)) => {
                tracing::warn!(missed = uMissed, "a traffic monitor fell behind");
                TrafficEvent::Lagged {
                    at_ms: NowMs(),
                    missed: uMissed,
                }
            }
        };

        // Serialization of these types cannot fail; if it somehow did, an empty comment keeps
        // the stream alive rather than tearing down every monitor over one bad event.
        let strJson = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
        Ok(Event::default().data(strJson))
    })
}

/// Format a CAN identifier the way the rest of the API does.
fn FormatCanId(u32CanId: u32) -> String {
    if u32CanId > 0x7FF {
        format!("{u32CanId:08X}")
    } else {
        format!("{u32CanId:03X}")
    }
}

/// Space-separated uppercase hex, matching every other hex field on this boundary.
fn FormatHexBytes(vecBytes: &[u8]) -> String {
    vecBytes
        .iter()
        .map(|byByte| format!("{byByte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// True for a flow-control frame: the ISO 15765-2 PCI type is 3.
fn IsFlowControlFrame(frame: &CanFrame) -> bool {
    matches!(frame.m_vecData.first(), Some(byFirst) if (byFirst >> 4) == 0x3)
}

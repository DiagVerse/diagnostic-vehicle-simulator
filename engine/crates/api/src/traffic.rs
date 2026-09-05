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

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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

/// How many past events the engine keeps so a monitor opened later can be shown what it missed.
///
/// Separate from the channel capacity, and much larger, because the two answer different
/// questions. The channel is about how far a *connected* monitor may fall behind before events
/// are lost to it; this is about how much of the session a monitor that was not yet open can be
/// told about. At roughly 200 bytes an event this is a few megabytes — the cost of being able
/// to open the monitor after noticing something went wrong, which is when people actually open
/// it.
const c_uHistoryCapacity: usize = 20_000;

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

    /// The history a monitor received on attaching, summarised.
    ///
    /// Sent before the replayed events so it reads as the line the history begins after. It
    /// says plainly when the replay is partial: the ring is bounded, and a monitor opened an
    /// hour into a session must not be left believing it is looking at the whole thing.
    #[serde(rename_all = "camelCase")]
    Replayed {
        at_ms: u64,
        /// How many past events follow this one.
        count: usize,
        /// How many older ones the engine had already dropped before this monitor attached.
        dropped_before: u64,
    },

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
    /// The recent past, so a monitor opened after the interesting moment can still see it.
    ///
    /// Owner: the channel. Every publisher locks it briefly to append; every new subscriber
    /// locks it briefly to take a snapshot. Held across `broadcast::send` on purpose — see
    /// `Publish` — and never across anything that can block.
    m_arcMtxHistory: Arc<Mutex<VecDeque<TrafficEvent>>>,
    /// How many events have been pushed out of the history ring since the engine started.
    m_arcDroppedFromHistory: Arc<AtomicU64>,
}

impl TrafficChannel {
    pub fn New() -> Self {
        let (sender, _receiver) = broadcast::channel(c_uChannelCapacity);
        TrafficChannel {
            m_sender: sender,
            m_arcMtxHistory: Arc::new(Mutex::new(VecDeque::with_capacity(c_uHistoryCapacity))),
            m_arcDroppedFromHistory: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Publish one event. Silently does nothing when nobody is listening.
    ///
    /// The history lock is held across the broadcast send, and that is deliberate. It makes
    /// "append to history" and "deliver to subscribers" one atomic step, which is what lets
    /// `SubscribeWithHistory` take a snapshot and a receiver with no gap and no duplicate
    /// between them. `broadcast::send` writes into a preallocated ring and wakes waiters; it
    /// does no I/O and cannot block, so this is not a lock held across slow work.
    pub fn Publish(&self, event: TrafficEvent) {
        let mut queueHistory = self
            .m_arcMtxHistory
            .lock()
            .expect("traffic history mutex poisoned");

        if queueHistory.len() == c_uHistoryCapacity {
            queueHistory.pop_front();
            self.m_arcDroppedFromHistory.fetch_add(1, Ordering::Relaxed);
        }
        queueHistory.push_back(event.clone());

        let _ = self.m_sender.send(event);
    }

    /// Start listening, and take everything that happened before now.
    ///
    /// Both under one lock. Snapshotting and subscribing separately would leave a window in
    /// which an event is either missed by both (published after the snapshot, before the
    /// subscribe) or seen by both (the reverse) — and a monitor that silently drops or
    /// duplicates an event around its own attach point is worse than one that shows nothing.
    pub fn SubscribeWithHistory(
        &self,
    ) -> (Vec<TrafficEvent>, u64, broadcast::Receiver<TrafficEvent>) {
        let queueHistory = self
            .m_arcMtxHistory
            .lock()
            .expect("traffic history mutex poisoned");

        let receiver = self.m_sender.subscribe();
        let vecHistory: Vec<TrafficEvent> = queueHistory.iter().cloned().collect();
        let u64Dropped = self.m_arcDroppedFromHistory.load(Ordering::Relaxed);

        (vecHistory, u64Dropped, receiver)
    }

    /// Start listening from now, without the history. Used by the tests.
    pub fn Subscribe(&self) -> broadcast::Receiver<TrafficEvent> {
        self.m_sender.subscribe()
    }

    /// How many past events the engine is currently holding.
    pub fn HistoryLength(&self) -> usize {
        self.m_arcMtxHistory
            .lock()
            .expect("traffic history mutex poisoned")
            .len()
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
///
/// The recent past is replayed first, then the live feed continues from exactly where the
/// replay ended. People open a monitor *because* something looked wrong, which is necessarily
/// after it happened; a feed that started blank would always be missing the thing they came to
/// look at.
pub async fn GetEvents(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let (vecHistory, u64DroppedBefore, receiver) = state.traffic.SubscribeWithHistory();

    tracing::info!(
        monitors = state.traffic.SubscriberCount(),
        replayed = vecHistory.len(),
        droppedBefore = u64DroppedBefore,
        "a traffic monitor attached"
    );

    // The summary goes first so it reads as the line the replayed history begins after, and so
    // a partial replay says so before the reader forms an impression of completeness.
    let mut vecPrelude = vec![TrafficEvent::Replayed {
        at_ms: NowMs(),
        count: vecHistory.len(),
        dropped_before: u64DroppedBefore,
    }];
    vecPrelude.extend(vecHistory);

    let historyStream = tokio_stream::iter(vecPrelude.into_iter().map(ToSseEvent));
    let stream = historyStream.chain(BuildEventStream(receiver));

    Sse::new(stream).keep_alive(KeepAlive::new().interval(c_keepAliveInterval))
}

/// Render one event as an SSE frame.
///
/// Serialization of these types cannot fail; if it somehow did, an empty object keeps the
/// stream alive rather than tearing down every monitor over one bad event.
fn ToSseEvent(event: TrafficEvent) -> Result<Event, Infallible> {
    let strJson = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
    Ok(Event::default().data(strJson))
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
        ToSseEvent(event)
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

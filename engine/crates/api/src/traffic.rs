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

use ::doip::header::c_uHeaderLength;
use ::doip::payload::PayloadType;
use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use bridge::observer::{FrameDirection, FrameObserver};
use can::CanFrame;
use doip_server::{DoIpDirection, DoIpObserver};
use futures_core::Stream;
use serde::{Deserialize, Serialize};
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

    /// One DoIP message crossing the Ethernet wire.
    ///
    /// Kept separate from `Frame` rather than folded into it. They share a direction and a
    /// length and nothing else: a DoIP message has a peer address and a payload type where a
    /// CAN frame has an identifier, and squeezing one into the other's shape would mean
    /// inventing a CAN id for something that has none.
    #[serde(rename_all = "camelCase")]
    DoIp {
        at_ms: u64,
        /// "rx" for a message the entity received, "tx" for one it sent.
        direction: String,
        /// The other end's address and port, so this reads against a packet capture.
        peer: String,
        /// "UDP" for discovery, "TCP" for the diagnostic connection.
        transport: String,
        /// The ISO 13400-2 payload type, in hex.
        payload_type_hex: String,
        /// What that payload type is called, so a reader need not keep Table 17 to hand.
        payload_name: String,
        /// The payload after the eight-byte header.
        payload_hex: String,
        /// Payload length, sent explicitly so a reader need not count the hex.
        length: usize,
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
    /// True when a user-defined override produced this answer rather than the UDS plugin.
    pub overridden: bool,
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
impl DoIpObserver for TrafficChannel {
    fn OnDoIpMessage(
        &self,
        direction: DoIpDirection,
        strPeer: &str,
        bIsUdp: bool,
        arrMessage: &[u8],
    ) {
        // Read for display only, so a message this engine cannot decode is still shown rather
        // than dropped: an unknown payload type is exactly the thing worth seeing, and a
        // monitor that hid it would be silent about the interesting case.
        let (strTypeHex, strTypeName, arrPayload) = DescribeDoIpMessage(arrMessage);

        self.Publish(TrafficEvent::DoIp {
            at_ms: NowMs(),
            direction: direction.Name().to_string(),
            peer: strPeer.to_string(),
            transport: if bIsUdp { "UDP" } else { "TCP" }.to_string(),
            payload_type_hex: strTypeHex,
            payload_name: strTypeName,
            payload_hex: FormatHexBytes(arrPayload),
            length: arrPayload.len(),
        });
    }
}

/// Pick a payload type, a name for it, and the body out of an encoded DoIP message.
///
/// Deliberately tolerant. This runs on whatever crossed the wire, including the malformed
/// things a monitor most needs to show, so a message too short to hold a header is reported as
/// exactly that rather than skipped.
fn DescribeDoIpMessage(arrMessage: &[u8]) -> (String, String, &[u8]) {
    if arrMessage.len() < c_uHeaderLength {
        return (
            "----".to_string(),
            "truncated (shorter than a header)".to_string(),
            arrMessage,
        );
    }

    let u16PayloadType = u16::from_be_bytes([arrMessage[2], arrMessage[3]]);
    let strName = match PayloadType::FromCode(u16PayloadType) {
        Some(payloadType) => format!("{payloadType:?}"),
        None => "unknown payload type".to_string(),
    };

    (
        format!("{u16PayloadType:04X}"),
        strName,
        &arrMessage[c_uHeaderLength..],
    )
}

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
        RoutingOutcome::TooLargeForSubnetwork {
            uRequestBytes,
            uMaxBytes,
            strEcuName,
        } => (
            "refused".to_string(),
            false,
            Vec::new(),
            Some(format!(
                "a functional request of {uRequestBytes} bytes is longer than the {uMaxBytes} \
                 a CAN sub-network can carry, and '{strEcuName}' is on one"
            )),
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
                    overridden: routed.m_plan.m_bIsOverridden,
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
pub async fn GetEvents(
    State(state): State<Arc<AppState>>,
    Query(query): Query<EventsQuery>,
) -> impl IntoResponse {
    let (vecHistory, u64DroppedBefore, receiver) = state.traffic.SubscribeWithHistory();
    let bWantsFrames = query.frames.unwrap_or(true);

    // Dropping frames here rather than in the monitor is the difference between a browser
    // receiving fifty thousand events during a flash transfer and receiving a few hundred. The
    // exchange lines — request, answer, which ECU — survive, and those are what a person reads.
    let mut vecHistory: Vec<TrafficEvent> = if bWantsFrames {
        vecHistory
    } else {
        vecHistory
            .into_iter()
            .filter(|event| !IsFrameEvent(event))
            .collect()
    };

    // Keep the most recent, which is the part anybody scrolls back to first.
    if let Some(uWanted) = query.history {
        if vecHistory.len() > uWanted {
            vecHistory.drain(..vecHistory.len() - uWanted);
        }
    }

    tracing::info!(
        monitors = state.traffic.SubscriberCount(),
        replayed = vecHistory.len(),
        droppedBefore = u64DroppedBefore,
        frames = bWantsFrames,
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

    // History goes out in batches too, so the reader has one shape to deal with.
    let vecHistoryBatches: Vec<Vec<TrafficEvent>> = vecPrelude
        .chunks(c_uMaxEventsPerBatch)
        .map(|chunk| chunk.to_vec())
        .collect();
    let historyStream = tokio_stream::iter(vecHistoryBatches.into_iter().map(ToSseBatch));
    let stream = historyStream.chain(BuildEventStream(receiver, bWantsFrames));

    Sse::new(stream).keep_alive(KeepAlive::new().interval(c_keepAliveInterval))
}

/// How many events one SSE message may carry.
const c_uMaxEventsPerBatch: usize = 64;
/// How long to gather events before sending what there is.
///
/// Shorter than a person can perceive, so a batched feed still reads as live.
const c_batchWindow: Duration = Duration::from_millis(40);

/// Render a batch of events as one SSE frame.
///
/// Serialization of these types cannot fail; if it somehow did, an empty batch keeps the
/// stream alive rather than tearing down every monitor over one bad event.
///
/// A single event is still sent as a batch of one rather than as a bare object: one shape on
/// the wire is one path through the reader, and a format that is sometimes an array is where a
/// subtle parsing bug eventually lives.
fn ToSseBatch(vecEvents: Vec<TrafficEvent>) -> Result<Event, Infallible> {
    let strJson = serde_json::to_string(&vecEvents).unwrap_or_else(|_| "[]".to_string());
    Ok(Event::default().data(strJson))
}

/// Query string for `GET /events`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsQuery {
    /// How many past events to replay on attach. Left out, everything held is replayed.
    ///
    /// Worth bounding because an EventSource reconnects by itself: a long session with a
    /// dropped connection replays the whole history again, and a monitor that can only hold a
    /// few thousand events pays to parse twenty thousand in order to throw most of them away.
    pub history: Option<usize>,
    /// Send raw wire traffic — CAN frames and DoIP messages — as well as decoded exchanges.
    /// Defaults to true.
    ///
    /// Turning it off is not cosmetic. A flash transfer puts tens of thousands of frames on the
    /// bus in a minute, and a browser asked to receive, parse and hold all of them stops
    /// responding to its own buttons — so the choice belongs where the events are, not where
    /// they land.
    pub frames: Option<bool>,
}

/// True for an event describing one CAN frame rather than a decoded exchange.
fn IsFrameEvent(event: &TrafficEvent) -> bool {
    // DoIP messages count too. They are the same kind of thing — one line per message on the
    // wire, rather than one per decoded exchange — and a flash transfer over Ethernet produces
    // them at the same rate a CAN one produces frames. A switch that quietly stopped working
    // once the session moved to Ethernet would be worse than no switch.
    matches!(
        event,
        TrafficEvent::Frame { .. } | TrafficEvent::DoIp { .. }
    )
}

/// Turn the broadcast receiver into a stream of SSE events.
///
/// A receiver that falls behind yields a `Lagged` event carrying the count rather than ending
/// the stream: the monitor stays attached and says what it missed.
fn BuildEventStream(
    receiver: broadcast::Receiver<TrafficEvent>,
    bWantsFrames: bool,
) -> impl Stream<Item = Result<Event, Infallible>> {
    BroadcastStream::new(receiver)
        .filter_map(move |result| {
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

            // Dropped before it is serialised, so a monitor that does not want frames costs
            // nothing to serve during a flood rather than merely hiding what it received.
            if !bWantsFrames && IsFrameEvent(&event) {
                return None;
            }
            Some(event)
        })
        // Batched rather than one message per event, and this is the difference that shows at
        // a flash transfer's rates. Two hundred thousand frames delivered singly is two hundred
        // thousand EventSource dispatches into the browser's main thread — each one a separate
        // task, a separate parse, a separate wake-up. The same data in batches costs a
        // fiftieth of the dispatches and parses faster besides.
        //
        // The window is short enough to stay live to a person watching, so nothing appears to
        // lag; the size cap keeps one batch from growing without bound on a busy bus.
        .chunks_timeout(c_uMaxEventsPerBatch, c_batchWindow)
        .map(ToSseBatch)
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

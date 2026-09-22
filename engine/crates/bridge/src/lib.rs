//! The CAN bridge: the simulation, on a wire.
//!
//! A frame arrives, ISO-TP reassembles it into a request, the simulation routes it to an ECU,
//! and the ECU's answer — which may be a ResponsePending followed later by the real response —
//! is segmented back into frames. The same [`ResponsePlan`](ecu::schedule::ResponsePlan) the
//! HTTP path executes drives this one, so an ECU behaves identically whichever way it is
//! reached.
//!
//! Requests are handled one at a time. That is not a simplification: a CAN bus serialises
//! frames anyway, so two ECUs cannot be mid-transfer simultaneously on one link.

#![allow(non_snake_case, non_upper_case_globals)]

pub mod bus;
pub mod mock;
pub mod observer;
pub mod probe;

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::observer::{FrameDirection, FrameObserver};
use application::ProtocolHandler;
use can::CanFrame;
use isotp::params::{c_timeoutFlowControl, IsoTpParameters};
use isotp::rx::{IsoTpReceiver, ReceiveOutcome};
use isotp::tx::{IsoTpTransmitter, IsoTpTransportError, TransmitState};
use simulation::execute::{EmittedFrame, ExecutePlans};
use simulation::{RoutingOutcome, SimulationService};
use tokio::time::{sleep, Duration};

use crate::bus::CanBusPort;

/// How long to wait between polls of a quiet bus. Short enough to stay responsive, long enough
/// not to spin a core.
const c_pollInterval: Duration = Duration::from_millis(2);

/// How much has crossed the link, for a status display.
///
/// Atomics rather than a lock: these are written on every frame and read occasionally by an
/// HTTP handler, and a counter is not worth contending over.
#[derive(Debug, Default)]
pub struct BridgeStats {
    m_atomicFramesReceived: AtomicU64,
    m_atomicFramesSent: AtomicU64,
}

impl BridgeStats {
    /// Frames taken off the bus.
    pub fn FramesReceived(&self) -> u64 {
        self.m_atomicFramesReceived.load(Ordering::Relaxed)
    }

    /// Frames put on it.
    pub fn FramesSent(&self) -> u64 {
        self.m_atomicFramesSent.load(Ordering::Relaxed)
    }
}

/// One ECU's view of the link: what it is reassembling, and on which identifier it answers.
struct Endpoint {
    m_u32RequestCanId: u32,
    m_u32ResponseCanId: u32,
    m_receiver: IsoTpReceiver,
}

/// Drives the simulation from a CAN bus.
pub struct CanBridge {
    m_boxBus: Box<dyn CanBusPort>,
    /// Who is watching the wire, if anyone. The bridge works identically without one.
    m_optObserver: Option<Arc<dyn FrameObserver>>,
    m_arcSimulation: Arc<Mutex<SimulationService>>,
    m_params: IsoTpParameters,
    /// Endpoints keyed by the identifier frames arrive on — physical and broadcast alike.
    m_mapEndpoints: BTreeMap<u32, Endpoint>,
    /// Frames read from the bus but not yet dealt with.
    ///
    /// A single queue matters: a read returns everything that has arrived, which may include
    /// both a request and the flow control for the answer to it. Without one queue that both
    /// the main loop and the flow-control wait draw from, the second would be thrown away and
    /// every segmented response would time out.
    m_queueInbound: VecDeque<CanFrame>,
    /// Frames taken off the queue while waiting for a flow control that were not it.
    ///
    /// They are put back, in order, once the transfer finishes. Discarding them is what makes
    /// a second ECU's multi-frame request lose ConsecutiveFrames while the first is still
    /// answering — and the hole then shows up as a sequence error on hardware that sent
    /// everything correctly.
    m_vecDeferred: Vec<CanFrame>,
    m_arcStats: Arc<BridgeStats>,
    m_startedAt: Instant,
    /// What the link between host and adapter can carry, when that is known.
    ///
    /// Used to refuse to advertise a pacing the link cannot honour. `None` for a bus with no
    /// such limit — an in-memory one, or a pseudo-terminal.
    m_optLinkCapacity: Option<LinkCapacity>,
    /// The simulation's configuration generation these endpoints were built from. Compared
    /// once per poll so an ECU added, removed or re-timed while the link is up reaches the
    /// wire without the operator having to stop and restart it.
    m_u64EndpointGeneration: u64,
}

impl CanBridge {
    /// Build a bridge over a bus, for whatever the simulation currently holds.
    pub fn New(
        boxBus: Box<dyn CanBusPort>,
        arcSimulation: Arc<Mutex<SimulationService>>,
        params: IsoTpParameters,
    ) -> Self {
        let mut bridge = CanBridge {
            m_boxBus: boxBus,
            m_optObserver: None,
            m_arcSimulation: arcSimulation,
            m_params: params,
            m_mapEndpoints: BTreeMap::new(),
            m_queueInbound: VecDeque::new(),
            m_vecDeferred: Vec::new(),
            m_arcStats: Arc::new(BridgeStats::default()),
            m_startedAt: Instant::now(),
            m_optLinkCapacity: None,
            m_u64EndpointGeneration: 0,
        };
        bridge.RebuildEndpoints();
        bridge
    }

    /// Tell the bridge what the link to the adapter can carry.
    ///
    /// Without this the bridge advertises exactly what each ECU asks for, which is right when
    /// nothing is in the way. With it, an ECU asking for no pacing at all on a link that cannot
    /// carry the bus unpaced is given the slowest pacing the link does support — see
    /// [`LinkCapacity::SafeSeparationTime`].
    pub fn WithLinkCapacity(mut self, u32SerialBaud: u32, u32CanBitrateBps: u32) -> Self {
        self.m_optLinkCapacity = Some(LinkCapacity::New(u32SerialBaud, u32CanBitrateBps));
        self.RebuildEndpoints();
        self
    }

    /// Attach something that wants to see every frame crossing this bridge.
    ///
    /// Optional by construction: a bridge with no observer behaves exactly as before, so
    /// nothing about the wire depends on whether anyone happens to be watching it.
    pub fn WithObserver(mut self, arcObserver: Arc<dyn FrameObserver>) -> Self {
        self.m_optObserver = Some(arcObserver);
        self
    }

    /// Tell the observer about one frame, if there is one.
    fn AnnounceFrame(&self, direction: FrameDirection, frame: &CanFrame) {
        if let Some(arcObserver) = &self.m_optObserver {
            arcObserver.OnFrame(direction, frame);
        }
    }

    /// The counters this bridge updates, for a status display.
    pub fn Stats(&self) -> Arc<BridgeStats> {
        Arc::clone(&self.m_arcStats)
    }

    /// Rebuild the per-identifier endpoints from the loaded vehicle. Call after the vehicle
    /// changes, or a newly added ECU is unreachable from the bus.
    pub fn RebuildEndpoints(&mut self) {
        let simulation = self
            .m_arcSimulation
            .lock()
            .expect("simulation mutex poisoned");
        let mut mapEndpoints = BTreeMap::new();

        for (key, runningEcu) in simulation.RunningEcus() {
            // The bridge is a CAN bus. An ECU reachable only over DoIP has no identifier to
            // listen on here, and skipping it is not a limitation — it is simply not on this
            // wire.
            let u32RequestCanId = match key.RequestCanId() {
                Some(u32RequestCanId) => u32RequestCanId,
                None => continue,
            };
            let address = match runningEcu.Config().m_optCanAddress {
                Some(address) => address,
                None => continue,
            };

            // Flow control is per ECU, not per link: each one declares how fast it is willing
            // to be sent a multi-frame request, exactly as a real ECU does.
            let paramsForEcu =
                self.PacedForLink(runningEcu.Config().m_strName.as_str(), runningEcu.Timing());

            mapEndpoints.insert(
                u32RequestCanId,
                Endpoint {
                    m_u32RequestCanId: u32RequestCanId,
                    m_u32ResponseCanId: address.m_u32ResponseCanId,
                    m_receiver: IsoTpReceiver::NewPhysical(paramsForEcu),
                },
            );

            // A broadcast identifier reaches several ECUs, and accepts single frames only.
            if let Some(u32FunctionalCanId) = address.m_optU32FunctionalCanId {
                mapEndpoints.entry(u32FunctionalCanId).or_insert(Endpoint {
                    m_u32RequestCanId: u32FunctionalCanId,
                    m_u32ResponseCanId: 0,
                    m_receiver: IsoTpReceiver::NewFunctional(self.m_params),
                });
            }
        }

        let u64Generation = simulation.ConfigGeneration();
        tracing::info!(
            endpoints = mapEndpoints.len(),
            generation = u64Generation,
            bus = %self.m_boxBus.Describe(),
            "bridge endpoints rebuilt"
        );
        drop(simulation);

        self.m_mapEndpoints = mapEndpoints;
        self.m_u64EndpointGeneration = u64Generation;
    }

    /// This link's ISO-TP parameters with one ECU's flow control applied.
    ///
    /// The padding byte belongs to the link and is kept; BlockSize and STmin belong to the ECU
    /// and are taken from its timing parameters, which is where an operator sets them.
    fn ParametersFor(&self, timing: core_domain::model::EcuTiming) -> IsoTpParameters {
        IsoTpParameters {
            m_u8BlockSize: timing.m_u8IsoTpBlockSize,
            m_bySeparationTimeMin: timing.m_byIsoTpSeparationTimeMin,
            m_optByPaddingByte: self.m_params.m_optByPaddingByte,
        }
    }

    /// One ECU's flow control, with the link's own limit applied when it has one.
    ///
    /// Only "no pacing at all" is overridden. An operator who set a BlockSize or an STmin has
    /// stated a rate and is obeyed; `0/0` is not a rate, it is the absence of one, and over a
    /// link that cannot carry the bus it means the middle of every long request is lost.
    /// Advertising it is the server promising something it cannot keep.
    fn PacedForLink(
        &self,
        strEcuName: &str,
        timing: core_domain::model::EcuTiming,
    ) -> IsoTpParameters {
        let params = self.ParametersFor(timing);

        let bAsksForNoPacing = params.m_u8BlockSize == 0 && params.m_bySeparationTimeMin == 0;
        if !bAsksForNoPacing {
            return params;
        }

        let capacity = match self.m_optLinkCapacity {
            Some(capacity) => capacity,
            // No known limit: nothing is in the way, so ask for nothing.
            None => return params,
        };

        let byNeeded = match capacity.SafeSeparationTime() {
            Some(byNeeded) => byNeeded,
            None => return params,
        };

        tracing::warn!(
            ecu = %strEcuName,
            linkFramesPerSecond = capacity.m_uLinkFramesPerSecond,
            busFramesPerSecond = capacity.m_uBusFramesPerSecond,
            stMinMs = byNeeded,
            "this ECU asks a tester for no pacing, which this link cannot carry; advertising \
             the slowest separation time it can honour instead. Set a BlockSize or an STmin to \
             choose your own, or raise the host link speed"
        );

        IsoTpParameters {
            m_bySeparationTimeMin: byNeeded,
            ..params
        }
    }

    /// Rebuild the endpoints if the simulation's configuration has moved on since they were
    /// built. Cheap when nothing has changed, which is the normal case.
    ///
    /// Anything part-way through reassembly is abandoned by the rebuild. That is the honest
    /// outcome: the operator has just changed what this ECU accepts, and finishing the message
    /// under the old rules would answer a question nobody asked.
    fn RebuildEndpointsIfStale(&mut self) {
        let u64Current = self
            .m_arcSimulation
            .lock()
            .expect("simulation mutex poisoned")
            .ConfigGeneration();

        if u64Current == self.m_u64EndpointGeneration {
            return;
        }

        tracing::info!(
            from = self.m_u64EndpointGeneration,
            to = u64Current,
            "the simulation changed; rebuilding the bridge's endpoints"
        );
        self.RebuildEndpoints();
    }

    /// Seconds since the bridge started, for stamping frames.
    fn NowSeconds(&self) -> f64 {
        self.m_startedAt.elapsed().as_secs_f64()
    }

    /// Poll the bus once and deal with whatever arrived.
    ///
    /// Returns how many complete requests were answered, so a caller can tell a busy link from
    /// a quiet one.
    pub async fn PumpOnce(&mut self, protocol: &dyn ProtocolHandler) -> usize {
        self.RebuildEndpointsIfStale();
        self.FillInbound();

        let mut uHandled = 0;
        while let Some(frame) = self.m_queueInbound.pop_front() {
            if self.HandleFrame(&frame, protocol).await {
                uHandled += 1;
            }
        }
        uHandled
    }

    /// Move whatever the bus has into the inbound queue.
    fn FillInbound(&mut self) {
        match self
            .m_boxBus
            .ReceiveFrames(self.m_startedAt.elapsed().as_secs_f64())
        {
            Ok(vecFrames) => {
                self.m_arcStats
                    .m_atomicFramesReceived
                    .fetch_add(vecFrames.len() as u64, Ordering::Relaxed);
                for frame in &vecFrames {
                    self.AnnounceFrame(FrameDirection::Received, frame);
                }
                self.m_queueInbound.extend(vecFrames);
            }
            Err(error) => tracing::warn!(%error, "could not read from the bus"),
        }
    }

    /// Run until the caller drops the future.
    pub async fn Run(&mut self, protocol: &dyn ProtocolHandler) {
        tracing::info!(bus = %self.m_boxBus.Describe(), "bridge running");
        loop {
            if self.PumpOnce(protocol).await == 0 {
                sleep(c_pollInterval).await;
            }
        }
    }

    /// Deal with one inbound frame. Returns true when it completed a request that was answered.
    async fn HandleFrame(&mut self, frame: &CanFrame, protocol: &dyn ProtocolHandler) -> bool {
        // A stopped simulation is an unpowered ECU: it does not answer, and — the part that is
        // easy to get wrong — it does not flow-control either. Checking here rather than at
        // routing time is what stops a half-alive ECU appearing on the wire, acknowledging
        // multi-frame requests it will never answer.
        if !self.IsRunning() {
            tracing::debug!(
                canId = format!("{:03X}", frame.m_u32CanId),
                "simulation is stopped; the frame is dropped without any reply"
            );
            return false;
        }

        let optOutcome = self.FeedEndpoint(frame);
        let (u32RequestCanId, vecPdu) = match optOutcome {
            Some(pair) => pair,
            None => return false,
        };

        self.AnswerRequest(u32RequestCanId, &vecPdu, protocol).await;
        true
    }

    /// Give the frame to whichever endpoint owns its identifier, sending any flow control the
    /// receiver asks for. Yields a complete request when one finishes.
    fn FeedEndpoint(&mut self, frame: &CanFrame) -> Option<(u32, Vec<u8>)> {
        let f64Now = self.NowSeconds();
        let endpoint = match self.m_mapEndpoints.get_mut(&frame.m_u32CanId) {
            Some(endpoint) => endpoint,
            None => {
                // A tester scanning for ECUs addresses identifiers nothing owns; that is
                // ordinary traffic, not a fault.
                tracing::trace!(canId = format!("{:03X}", frame.m_u32CanId), "frame ignored");
                return None;
            }
        };

        let u32RequestCanId = endpoint.m_u32RequestCanId;
        let u32ResponseCanId = endpoint.m_u32ResponseCanId;

        match endpoint.m_receiver.OnFrame(&frame.m_vecData) {
            ReceiveOutcome::Completed { vecPdu } => Some((u32RequestCanId, vecPdu)),
            ReceiveOutcome::SendFlowControl { vecFrame } | ReceiveOutcome::Refused { vecFrame } => {
                // Flow control goes on the identifier the ECU answers on: that is where the
                // tester's transmitter is listening.
                self.SendRaw(u32ResponseCanId, vecFrame, f64Now);
                None
            }
            ReceiveOutcome::Aborted(error) => {
                // Named rather than left to be deduced. Frames going missing between a tester
                // and this engine is not the tester sending them out of order — a VCI that
                // works against real ECUs is sending them correctly, and the difference is
                // everything in between, which on a serial adapter is a link far slower than
                // the bus feeding it.
                tracing::warn!(
                    %error,
                    canId = format!("{u32RequestCanId:03X}"),
                    "inbound message abandoned; frames went missing on the way here rather than \
                     arriving out of order, so look at what carries them — over a serial adapter \
                     that is the host link speed and the flow control this ECU advertises"
                );
                None
            }
            ReceiveOutcome::Nothing => None,
        }
    }

    /// Route a complete request and put the answer on the bus.
    async fn AnswerRequest(
        &mut self,
        u32RequestCanId: u32,
        vecPdu: &[u8],
        protocol: &dyn ProtocolHandler,
    ) {
        let outcome = {
            let mut simulation = self
                .m_arcSimulation
                .lock()
                .expect("simulation mutex poisoned");
            simulation.ProcessByCanId(u32RequestCanId, vecPdu, protocol)
            // The guard is dropped here, before anything sleeps. The compiler enforces it.
        };

        // Announced before the silent outcomes return, so a monitor sees the request that got
        // nothing back and the reason — which is the case a person is usually chasing.
        if let Some(arcObserver) = &self.m_optObserver {
            arcObserver.OnExchange(u32RequestCanId, vecPdu, &outcome);
        }

        let vecResponses = match outcome {
            RoutingOutcome::Handled(vecResponses) => vecResponses,
            // Silence on the wire in every one of these cases, and the simulation service has
            // already logged which it was and why.
            RoutingOutcome::NoTarget
            | RoutingOutcome::Stopped
            | RoutingOutcome::Silenced { .. } => return,
        };

        // Collect what to send as each step comes due, then segment it. The plan's timing is
        // the ECU's; how long segmentation then takes is the link's, and the two must not be
        // allowed to interfere — a tester dawdling over flow control cannot be permitted to
        // delay a later plan step.
        let mut vecDue: Vec<(u32, Vec<u8>)> = Vec::new();
        let mut fnOnFrame = |frame: EmittedFrame<'_>| {
            let response = &vecResponses[frame.m_uResponseIndex];
            vecDue.push((response.m_u32ResponseCanId, frame.m_step.m_vecBytes.clone()));
        };
        ExecutePlans(&vecResponses, &mut fnOnFrame).await;

        for (u32ResponseCanId, vecBytes) in vecDue {
            self.TransmitPdu(u32RequestCanId, u32ResponseCanId, &vecBytes)
                .await;
        }
    }

    /// Send one PDU, segmenting it and obeying the tester's flow control.
    async fn TransmitPdu(&mut self, u32RequestCanId: u32, u32ResponseCanId: u32, vecPdu: &[u8]) {
        let mut transmitter = IsoTpTransmitter::New(self.m_params);

        let vecFirst = match transmitter.Begin(vecPdu) {
            Ok(vecFirst) => vecFirst,
            Err(error) => {
                tracing::warn!(%error, "the response could not be segmented");
                return;
            }
        };
        self.SendRaw(u32ResponseCanId, vecFirst, self.NowSeconds());

        while *transmitter.State() != TransmitState::Complete {
            match self
                .AwaitFlowControl(u32RequestCanId, &mut transmitter)
                .await
            {
                Ok(()) => {}
                Err(error) => {
                    tracing::warn!(%error, responseCanId = format!("{u32ResponseCanId:03X}"), "response abandoned");
                    self.RestoreDeferredFrames();
                    return;
                }
            }

            // Send this block, spacing the frames the way the tester asked for.
            let separationTime = transmitter.SeparationTime();
            while let Some(vecFrame) = transmitter.NextConsecutiveFrame() {
                if !separationTime.is_zero() {
                    sleep(separationTime).await;
                }
                self.SendRaw(u32ResponseCanId, vecFrame, self.NowSeconds());
            }
        }

        self.RestoreDeferredFrames();
    }

    /// Put everything set aside during a transfer back at the head of the queue, in the order
    /// it arrived, so the next pump deals with it as though the transfer had not happened.
    fn RestoreDeferredFrames(&mut self) {
        if self.m_vecDeferred.is_empty() {
            return;
        }

        tracing::debug!(
            frames = self.m_vecDeferred.len(),
            "returning frames set aside during a transfer"
        );
        // Pushed to the front in reverse, which puts them back in their original order ahead of
        // anything that has arrived since.
        for frame in self.m_vecDeferred.drain(..).rev() {
            self.m_queueInbound.push_front(frame);
        }
    }

    /// Poll the bus until the tester's flow control turns up, or the timeout expires.
    async fn AwaitFlowControl(
        &mut self,
        u32TransmittingForCanId: u32,
        transmitter: &mut IsoTpTransmitter,
    ) -> Result<(), IsoTpTransportError> {
        let deadline = Instant::now() + c_timeoutFlowControl;

        while *transmitter.State() == TransmitState::AwaitingFlowControl {
            if Instant::now() >= deadline {
                return transmitter.OnFlowControlTimeout();
            }

            self.FillInbound();
            let optFrame = self.m_queueInbound.pop_front();
            let frame = match optFrame {
                Some(frame) => frame,
                None => {
                    sleep(c_pollInterval).await;
                    continue;
                }
            };

            if IsFlowControlFrame(&frame) && self.m_mapEndpoints.contains_key(&frame.m_u32CanId) {
                transmitter.OnFlowControl(&frame.m_vecData)?;
                continue;
            }

            // A frame for the identifier being answered is dropped: this ECU has told the
            // tester it is busy, a real one does not take a new request mid-response, and
            // interleaving two messages on one identifier is unrecoverable.
            if frame.m_u32CanId == u32TransmittingForCanId {
                tracing::warn!(
                    canId = format!("{:03X}", frame.m_u32CanId),
                    "dropping a frame that arrived while this ECU was mid-transfer"
                );
                continue;
            }

            // A frame for any *other* identifier belongs to a different conversation and is
            // nothing to do with this transfer. Dropping it used to punch a hole in that ECU's
            // message, which surfaces as a consecutive-frame sequence error on a bus where
            // every frame was in fact sent correctly.
            self.m_vecDeferred.push(frame);
        }
        Ok(())
    }

    /// Whether the simulation is currently on the bus.
    fn IsRunning(&self) -> bool {
        self.m_arcSimulation
            .lock()
            .expect("simulation mutex poisoned")
            .IsRunning()
    }

    /// Put one frame on the bus, logging rather than failing if the link is gone.
    fn SendRaw(&mut self, u32CanId: u32, vecData: Vec<u8>, f64TimestampSec: f64) {
        let frame = CanFrame::NewClassic(f64TimestampSec, u32CanId, vecData);
        self.m_arcStats
            .m_atomicFramesSent
            .fetch_add(1, Ordering::Relaxed);
        // Announced before the send is attempted, so a frame the link then refuses still shows
        // up in the monitor alongside the warning explaining why it never left.
        self.AnnounceFrame(FrameDirection::Sent, &frame);
        if let Err(error) = self.m_boxBus.SendFrame(&frame) {
            tracing::warn!(%error, canId = format!("{u32CanId:03X}"), "could not transmit a frame");
        }
    }
}

/// How fast frames can cross each side of the bridge.
///
/// The two numbers that matter are frames per second, not bits: a CAN frame and the SLCAN line
/// that carries it are wildly different sizes, and it is the mismatch between the rates that
/// loses frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkCapacity {
    /// Frames per second the host-to-adapter link can carry.
    pub m_uLinkFramesPerSecond: usize,
    /// Frames per second the CAN bus can deliver, back to back.
    pub m_uBusFramesPerSecond: usize,
}

/// Bytes in the SLCAN line for a 29-bit frame with eight data bytes: `T`, eight identifier
/// characters, one length digit, sixteen data characters and the terminator.
const c_uSlcanLineBytes: usize = 27;
/// Bits per serial byte at 8N1 — the start and stop bits are why a byte costs ten, not eight.
const c_uSerialBitsPerByte: usize = 10;
/// Bits on the wire for a 29-bit CAN frame carrying eight data bytes, stuffing included.
const c_uCanBitsPerFrame: usize = 135;

impl LinkCapacity {
    /// Work out both rates from the two speeds.
    pub fn New(u32SerialBaud: u32, u32CanBitrateBps: u32) -> Self {
        LinkCapacity {
            m_uLinkFramesPerSecond: (u32SerialBaud as usize)
                / (c_uSerialBitsPerByte * c_uSlcanLineBytes),
            m_uBusFramesPerSecond: (u32CanBitrateBps as usize) / c_uCanBitsPerFrame,
        }
    }

    /// The separation time a sender must leave for this link to keep up, or `None` when the
    /// link is fast enough that no pacing is needed.
    ///
    /// Rounded up to whole milliseconds, which is all an STmin below `0xF1` can express, and
    /// then given one millisecond of headroom — landing exactly on the limit leaves nothing for
    /// a busy host, and the cost of being one millisecond slow is far smaller than the cost of
    /// losing a frame.
    pub fn SafeSeparationTime(self) -> Option<u8> {
        if self.m_uLinkFramesPerSecond == 0
            || self.m_uLinkFramesPerSecond >= self.m_uBusFramesPerSecond
        {
            return None;
        }

        let uMillisecondsPerFrame = 1000usize.div_ceil(self.m_uLinkFramesPerSecond);
        Some((uMillisecondsPerFrame + 1).min(0x7F) as u8)
    }
}

/// True for a flow-control frame: the PCI type is 3.
fn IsFlowControlFrame(frame: &CanFrame) -> bool {
    matches!(frame.m_vecData.first(), Some(byFirst) if (byFirst >> 4) == 0x3)
}

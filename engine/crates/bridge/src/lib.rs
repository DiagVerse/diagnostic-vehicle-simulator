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

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
    m_arcStats: Arc<BridgeStats>,
    m_startedAt: Instant,
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
            m_arcSimulation: arcSimulation,
            m_params: params,
            m_mapEndpoints: BTreeMap::new(),
            m_queueInbound: VecDeque::new(),
            m_arcStats: Arc::new(BridgeStats::default()),
            m_startedAt: Instant::now(),
        };
        bridge.RebuildEndpoints();
        bridge
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

        for (u32RequestCanId, runningEcu) in simulation.RunningEcus() {
            let address = match runningEcu.Config().m_optCanAddress {
                Some(address) => address,
                None => continue,
            };

            mapEndpoints.insert(
                u32RequestCanId,
                Endpoint {
                    m_u32RequestCanId: u32RequestCanId,
                    m_u32ResponseCanId: address.m_u32ResponseCanId,
                    m_receiver: IsoTpReceiver::NewPhysical(self.m_params),
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

        tracing::info!(
            endpoints = mapEndpoints.len(),
            bus = %self.m_boxBus.Describe(),
            "bridge endpoints rebuilt"
        );
        self.m_mapEndpoints = mapEndpoints;
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
                tracing::warn!(%error, canId = format!("{u32RequestCanId:03X}"), "inbound message abandoned");
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

        let vecResponses = match outcome {
            RoutingOutcome::Handled(vecResponses) => vecResponses,
            // Silence either way, and both are already logged by the simulation service.
            RoutingOutcome::NoTarget | RoutingOutcome::Stopped => return,
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
            self.TransmitPdu(u32ResponseCanId, &vecBytes).await;
        }
    }

    /// Send one PDU, segmenting it and obeying the tester's flow control.
    async fn TransmitPdu(&mut self, u32ResponseCanId: u32, vecPdu: &[u8]) {
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
            match self.AwaitFlowControl(&mut transmitter).await {
                Ok(()) => {}
                Err(error) => {
                    tracing::warn!(%error, responseCanId = format!("{u32ResponseCanId:03X}"), "response abandoned");
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
    }

    /// Poll the bus until the tester's flow control turns up, or the timeout expires.
    async fn AwaitFlowControl(
        &mut self,
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

            // Anything else arriving mid-transfer is dropped. An ECU part-way through
            // answering has told the tester it is busy, and a real one does not take a new
            // request; interleaving two messages on one identifier would be unrecoverable.
            tracing::warn!(
                canId = format!("{:03X}", frame.m_u32CanId),
                "dropping a frame that arrived while this ECU was mid-transfer"
            );
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
        if let Err(error) = self.m_boxBus.SendFrame(&frame) {
            tracing::warn!(%error, canId = format!("{u32CanId:03X}"), "could not transmit a frame");
        }
    }
}

/// True for a flow-control frame: the PCI type is 3.
fn IsFlowControlFrame(frame: &CanFrame) -> bool {
    matches!(frame.m_vecData.first(), Some(byFirst) if (byFirst >> 4) == 0x3)
}

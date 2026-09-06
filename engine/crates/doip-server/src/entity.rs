//! What this vehicle tells a DoIP tester about itself, and how it answers one message.
//!
//! The decision half of the server: given a decoded request it produces the bytes to send back
//! and whether to close the socket. It holds the connection table and the simulation, but no
//! sockets — so a whole diagnostic session can be driven in a test without a network.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use application::ProtocolHandler;
use core_domain::model::VehicleIdentity;
use doip::connection::Connection;
use doip::header::{self, HeaderLimits, HeaderNack};
use doip::messages::*;
use doip::payload::PayloadType;
use simulation::{RoutingOutcome, SimulationService};

use crate::settings::DoIpSettings;

/// How many concurrent TCP data sockets this entity serves.
///
/// The standard names no value — it is manufacturer discretion — and requires `<n+1>` resources
/// so one is always free for socket handling. This is the `<n>` reported to a tester, and the
/// reserve is not included in it.
pub const c_uMaxConnections: usize = 4;

/// What the caller must do with one incoming message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaction {
    /// Messages to send back, in order. Empty means answer nothing at all — which for some
    /// requests is the conformant behaviour, not an omission.
    pub m_vecReplies: Vec<Vec<u8>>,
    /// Whether to close the socket once they are sent.
    pub m_bCloseSocket: bool,
}

impl Reaction {
    /// Say nothing and stay connected.
    pub fn Silence() -> Self {
        Reaction {
            m_vecReplies: Vec::new(),
            m_bCloseSocket: false,
        }
    }

    /// One reply, staying connected.
    pub fn Reply(vecMessage: Vec<u8>) -> Self {
        Reaction {
            m_vecReplies: vec![vecMessage],
            m_bCloseSocket: false,
        }
    }

    /// One reply, then close.
    pub fn ReplyAndClose(vecMessage: Vec<u8>) -> Self {
        Reaction {
            m_vecReplies: vec![vecMessage],
            m_bCloseSocket: true,
        }
    }
}

/// The DoIP entity: this vehicle, as a tester sees it.
pub struct DoIpEntity {
    m_arcSimulation: Arc<Mutex<SimulationService>>,
    /// The logical address this entity answers vehicle identification on, and reports as its own.
    m_u16EntityAddress: u16,
    /// What this entity says about itself, and what it has been told to say instead.
    ///
    /// Shared rather than owned so a change takes effect on a running entity: being able to
    /// inject a fault only by restarting the server would make it useless for reproducing one
    /// mid-session, which is exactly when a fault matters.
    m_arcMtxSettings: Arc<Mutex<DoIpSettings>>,
    /// The connection table, keyed by whatever the transport calls a socket.
    m_mapConnections: BTreeMap<u64, Connection>,
}

impl DoIpEntity {
    /// Build an entity over a loaded simulation, with settings it shares with its caller.
    pub fn New(
        arcSimulation: Arc<Mutex<SimulationService>>,
        u16EntityAddress: u16,
        arcMtxSettings: Arc<Mutex<DoIpSettings>>,
    ) -> Self {
        DoIpEntity {
            m_arcSimulation: arcSimulation,
            m_u16EntityAddress: u16EntityAddress,
            m_arcMtxSettings: arcMtxSettings,
            m_mapConnections: BTreeMap::new(),
        }
    }

    /// A snapshot of the settings, taken once per message so one answer cannot be built from
    /// two different configurations.
    fn Settings(&self) -> DoIpSettings {
        self.m_arcMtxSettings
            .lock()
            .expect("DoIP settings mutex poisoned")
            .clone()
    }

    /// The header limits this entity currently enforces.
    ///
    /// Derived from the settings rather than fixed, so the maximum data size a tester is *told*
    /// and the one actually enforced cannot disagree — a conformance test cross-checks them.
    fn Limits(&self) -> HeaderLimits {
        let u32MaxDataSize = self.Settings().m_u32MaxDataSize;
        HeaderLimits {
            m_u32MaxDataSize: u32MaxDataSize,
            m_u32AvailableMemory: u32MaxDataSize,
        }
    }

    /// The logical address this entity reports as its own.
    pub fn EntityAddress(&self) -> u16 {
        self.m_u16EntityAddress
    }

    /// How many connections are open.
    pub fn ConnectionCount(&self) -> usize {
        self.m_mapConnections.len()
    }

    /// Register a newly established socket.
    pub fn OpenConnection(&mut self, u64Socket: u64) {
        self.m_mapConnections.insert(u64Socket, Connection::New());
    }

    /// Forget a socket that has closed.
    pub fn CloseConnection(&mut self, u64Socket: u64) {
        self.m_mapConnections.remove(&u64Socket);
    }

    /// Advance every connection's clock, returning the sockets that have now timed out.
    pub fn Tick(&mut self, u64ElapsedMs: u64) -> Vec<u64> {
        let mut vecExpired = Vec::new();
        for (u64Socket, connection) in &mut self.m_mapConnections {
            if connection.Tick(u64ElapsedMs) {
                vecExpired.push(*u64Socket);
            }
        }
        for u64Socket in &vecExpired {
            self.m_mapConnections.remove(u64Socket);
        }
        vecExpired
    }

    /// Answer one message that arrived on the UDP discovery port.
    ///
    /// The vehicle identification requests are the reason the header's version byte cannot be
    /// enforced here: a tester that has not yet found the vehicle has nothing to base it on.
    pub fn HandleUdp(&self, arrDatagram: &[u8]) -> Reaction {
        let settings = self.Settings();
        if let Some(byForced) = settings.m_optByForcedHeaderNack {
            return self.ForcedHeaderNackReaction(0x03, byForced);
        }

        let header = match header::ReadHeader(arrDatagram, self.Limits()) {
            Ok(header) => header,
            Err(nack) => return self.NackReaction(0x03, nack),
        };

        let vecPayload = &arrDatagram[header::c_uHeaderLength.min(arrDatagram.len())..];
        let byReplyVersion = header::ReplyVersionFor(header.m_byProtocolVersion);

        match header.m_payloadType {
            PayloadType::VehicleIdentificationRequest => {
                // A vehicle that cannot be discovered is a real failure to reproduce: an entity
                // still booting, or one the tester cannot reach.
                if settings.m_bSuppressIdentificationResponse {
                    return Reaction::Silence();
                }
                Reaction::Reply(self.BuildAnnouncement(byReplyVersion))
            }

            PayloadType::VehicleIdentificationRequestByEid => {
                // A non-match is answered with silence, not a negative response: the tester is
                // asking "is this you?" of every vehicle in range, and only the right one speaks.
                let identity = self.Identity();
                if vecPayload == identity.EidBytes() && !settings.m_bSuppressIdentificationResponse
                {
                    Reaction::Reply(self.BuildAnnouncement(byReplyVersion))
                } else {
                    Reaction::Silence()
                }
            }

            PayloadType::VehicleIdentificationRequestByVin => {
                let identity = self.Identity();
                if vecPayload == identity.VinBytes() && !settings.m_bSuppressIdentificationResponse
                {
                    Reaction::Reply(self.BuildAnnouncement(byReplyVersion))
                } else {
                    Reaction::Silence()
                }
            }

            PayloadType::PowerModeRequest => Reaction::Reply(header::WriteMessage(
                byReplyVersion,
                PayloadType::PowerModeResponse,
                &[settings.m_byPowerMode],
            )),

            PayloadType::EntityStatusRequest => {
                let status = EntityStatus {
                    m_byNodeType: settings.m_byNodeType,
                    // The count excludes the reserve socket kept for socket handling.
                    m_byMaxSockets: settings.m_byMaxSockets,
                    m_byOpenSockets: self.m_mapConnections.len() as u8,
                    m_optU32MaxDataSize: Some(settings.m_u32MaxDataSize),
                };
                Reaction::Reply(header::WriteMessage(
                    byReplyVersion,
                    PayloadType::EntityStatusResponse,
                    &status.ToBytes(),
                ))
            }

            // Everything else belongs on the TCP data socket, so it is not a message this port
            // knows how to answer.
            _ => self.NackReaction(
                byReplyVersion,
                HeaderNack::UnknownPayloadType {
                    u16PayloadType: header.m_payloadType.Code(),
                },
            ),
        }
    }

    /// Answer one message that arrived on a TCP data socket.
    pub fn HandleTcp(
        &mut self,
        u64Socket: u64,
        arrMessage: &[u8],
        protocol: &dyn ProtocolHandler,
    ) -> Reaction {
        if let Some(byForced) = self.Settings().m_optByForcedHeaderNack {
            return self.ForcedHeaderNackReaction(0x03, byForced);
        }

        let header = match header::ReadHeader(arrMessage, self.Limits()) {
            Ok(header) => header,
            Err(nack) => return self.NackReaction(0x03, nack),
        };

        if let Some(connection) = self.m_mapConnections.get_mut(&u64Socket) {
            connection.NoteActivity();
        }

        let vecPayload = arrMessage[header::c_uHeaderLength.min(arrMessage.len())..].to_vec();
        let byReplyVersion = header::ReplyVersionFor(header.m_byProtocolVersion);

        match header.m_payloadType {
            PayloadType::RoutingActivationRequest => {
                self.HandleRoutingActivation(u64Socket, &vecPayload, byReplyVersion)
            }

            PayloadType::DiagnosticMessage => {
                self.HandleDiagnosticMessage(u64Socket, &vecPayload, byReplyVersion, protocol)
            }

            // A tester may send this unsolicited purely to reset the inactivity timer — it is
            // the smallest valid message that does nothing else. `NoteActivity` above has
            // already done the work, so the correct answer is nothing at all.
            PayloadType::AliveCheckResponse => Reaction::Silence(),

            PayloadType::AliveCheckRequest => {
                let optU16SourceAddress = self
                    .m_mapConnections
                    .get(&u64Socket)
                    .and_then(|connection| connection.SourceAddress());
                match optU16SourceAddress {
                    Some(u16SourceAddress) => Reaction::Reply(header::WriteMessage(
                        byReplyVersion,
                        PayloadType::AliveCheckResponse,
                        &BuildAliveCheckResponse(u16SourceAddress),
                    )),
                    // Nothing is registered here yet, so there is no address to answer with.
                    None => Reaction::Silence(),
                }
            }

            _ => self.NackReaction(
                byReplyVersion,
                HeaderNack::UnknownPayloadType {
                    u16PayloadType: header.m_payloadType.Code(),
                },
            ),
        }
    }

    /// Decide and apply a routing activation request.
    fn HandleRoutingActivation(
        &mut self,
        u64Socket: u64,
        vecPayload: &[u8],
        byReplyVersion: u8,
    ) -> Reaction {
        let request = match RoutingActivationRequest::FromBytes(vecPayload) {
            Some(request) => request,
            None => {
                return self.NackReaction(
                    byReplyVersion,
                    HeaderNack::InvalidPayloadLength {
                        u16PayloadType: PayloadType::RoutingActivationRequest.Code(),
                        u32Length: vecPayload.len() as u32,
                    },
                )
            }
        };

        // What the other connections say about this address, gathered before the borrow of the
        // one being decided.
        let bIsActiveElsewhere = self.m_mapConnections.iter().any(|(u64Other, connection)| {
            *u64Other != u64Socket && connection.SourceAddress() == Some(request.m_u16SourceAddress)
        });
        let bHasFreeSocket = self.m_mapConnections.len() <= c_uMaxConnections;

        let optByForced = self
            .m_arcMtxSettings
            .lock()
            .expect("DoIP settings mutex poisoned")
            .m_optByForcedRoutingActivationCode;

        let connection = match self.m_mapConnections.get_mut(&u64Socket) {
            Some(connection) => connection,
            None => return Reaction::Silence(),
        };

        let outcome = match optByForced.and_then(ForcedRoutingOutcome) {
            // A forced denial is still applied to the connection, so the socket closes or is
            // held exactly as the real decision would have made it.
            Some(forced) => forced,
            None => connection.DecideRoutingActivation(
                &request,
                IsAcceptableTesterAddress(request.m_u16SourceAddress),
                bIsActiveElsewhere,
                bHasFreeSocket,
            ),
        };
        connection.ApplyRoutingActivation(&request, outcome);

        tracing::info!(
            socket = u64Socket,
            sourceAddress = format!("{:04X}", request.m_u16SourceAddress),
            activationType = format!("{:02X}", request.m_byActivationType),
            responseCode = format!("{:02X}", outcome.Code()),
            "routing activation decided"
        );

        let vecMessage = header::WriteMessage(
            byReplyVersion,
            PayloadType::RoutingActivationResponse,
            &BuildRoutingActivationResponse(
                request.m_u16SourceAddress,
                self.m_u16EntityAddress,
                outcome,
            ),
        );

        if outcome.ClosesSocket() {
            Reaction::ReplyAndClose(vecMessage)
        } else {
            Reaction::Reply(vecMessage)
        }
    }

    /// Route a diagnostic message into the simulation and package what comes back.
    ///
    /// The ordering is the part that matters: the positive acknowledgement goes out **before**
    /// the ECU has answered, because it means "routed", not "accepted". A negative response
    /// arriving later does not contradict it, and an ECU that goes quiet after it is a UDS
    /// timeout rather than a DoIP fault.
    fn HandleDiagnosticMessage(
        &mut self,
        u64Socket: u64,
        vecPayload: &[u8],
        byReplyVersion: u8,
        protocol: &dyn ProtocolHandler,
    ) -> Reaction {
        let request = match DiagnosticMessage::FromBytes(vecPayload) {
            Some(request) => request,
            None => return Reaction::Silence(),
        };

        // Nothing is routed before routing is active (REQ 3.DoIP-131 NL) — and it is not
        // negatively acknowledged either. The socket dies to the initial inactivity timer.
        let optU16Registered = self
            .m_mapConnections
            .get(&u64Socket)
            .filter(|connection| connection.IsRoutingActive())
            .and_then(|connection| connection.SourceAddress());

        let u16Registered = match optU16Registered {
            Some(u16Registered) => u16Registered,
            None => {
                tracing::debug!(
                    socket = u64Socket,
                    "a diagnostic message arrived before routing was activated; ignoring it"
                );
                return Reaction::Silence();
            }
        };

        // Injected before anything is routed, so the tester sees the refusal a real entity
        // would give rather than an answer followed by one.
        if let Some(byForced) = self.Settings().m_optByForcedDiagnosticNack {
            if let Some(nack) = ForcedDiagnosticNack(byForced) {
                return self.DiagnosticNackReaction(byReplyVersion, &request, nack);
            }
        }

        // The source address must be the one activated on this socket. This is the one
        // diagnostic rejection that resets the connection.
        if request.m_u16SourceAddress != u16Registered {
            return self.DiagnosticNackReaction(
                byReplyVersion,
                &request,
                DiagnosticNack::InvalidSourceAddress,
            );
        }

        let outcome = {
            let mut simulation = self
                .m_arcSimulation
                .lock()
                .expect("simulation mutex poisoned");
            simulation.ProcessByLogicalAddress(
                request.m_u16TargetAddress,
                &request.m_vecUserData,
                protocol,
            )
        };

        match outcome {
            // No ECU carries that logical address.
            RoutingOutcome::NoTarget => self.DiagnosticNackReaction(
                byReplyVersion,
                &request,
                DiagnosticNack::UnknownTargetAddress,
            ),

            // The entity is not on the air at all, so nothing was routed anywhere.
            RoutingOutcome::Stopped => self.DiagnosticNackReaction(
                byReplyVersion,
                &request,
                DiagnosticNack::TargetUnreachable,
            ),

            // The ECU exists and the request was routed to it; it simply is not answering.
            // That is an acknowledgement followed by silence — exactly what a real gateway
            // produces, and what leaves the tester to time out on P2 as it should.
            RoutingOutcome::Silenced { strEcuName, .. } => {
                tracing::debug!(
                    ecu = %strEcuName,
                    "routed to an ECU that is off the air; acknowledging and staying quiet"
                );
                Reaction::Reply(self.BuildAck(byReplyVersion, &request))
            }

            RoutingOutcome::Handled(vecResponses) => {
                let mut vecReplies = vec![self.BuildAck(byReplyVersion, &request)];

                for response in vecResponses {
                    if response.IsSuppressed() {
                        continue;
                    }
                    // Every step of the plan is its own DoIP diagnostic message, including each
                    // ResponsePending. Concatenating them into one payload would hand the
                    // tester something it cannot parse as UDS.
                    for step in &response.m_plan.m_vecSteps {
                        let answer = DiagnosticMessage {
                            m_u16SourceAddress: request.m_u16TargetAddress,
                            m_u16TargetAddress: request.m_u16SourceAddress,
                            m_vecUserData: step.m_vecBytes.clone(),
                        };
                        vecReplies.push(header::WriteMessage(
                            byReplyVersion,
                            PayloadType::DiagnosticMessage,
                            &answer.ToBytes(),
                        ));
                    }
                }

                Reaction {
                    m_vecReplies: vecReplies,
                    m_bCloseSocket: false,
                }
            }
        }
    }

    /// The positive acknowledgement for a routed message.
    fn BuildAck(&self, byReplyVersion: u8, request: &DiagnosticMessage) -> Vec<u8> {
        header::WriteMessage(
            byReplyVersion,
            PayloadType::DiagnosticMessageAck,
            &BuildDiagnosticAck(request, c_byAckRoutingConfirmation),
        )
    }

    /// A diagnostic negative acknowledgement, closing the socket if that code requires it.
    fn DiagnosticNackReaction(
        &self,
        byReplyVersion: u8,
        request: &DiagnosticMessage,
        nack: DiagnosticNack,
    ) -> Reaction {
        let vecMessage = header::WriteMessage(
            byReplyVersion,
            PayloadType::DiagnosticMessageNack,
            &BuildDiagnosticAck(request, nack.Code()),
        );

        if nack.ClosesSocket() {
            Reaction::ReplyAndClose(vecMessage)
        } else {
            Reaction::Reply(vecMessage)
        }
    }

    /// An injected generic header negative acknowledgement.
    ///
    /// Built from the code directly rather than from a `HeaderNack`, because the point is to
    /// send a refusal the entity has no genuine reason for — and the closing rule still has to
    /// match the code, or a tester would see a combination the standard never produces.
    fn ForcedHeaderNackReaction(&self, byReplyVersion: u8, byCode: u8) -> Reaction {
        let vecMessage =
            header::WriteMessage(byReplyVersion, PayloadType::GenericHeaderNack, &[byCode]);
        // 0x00 and 0x04 close the socket; 0x01 through 0x03 discard the message and keep it.
        if byCode == 0x00 || byCode == 0x04 {
            Reaction::ReplyAndClose(vecMessage)
        } else {
            Reaction::Reply(vecMessage)
        }
    }

    /// A generic header negative acknowledgement, closing the socket if that code requires it.
    fn NackReaction(&self, byReplyVersion: u8, nack: HeaderNack) -> Reaction {
        let vecMessage = doip::BuildHeaderNack(byReplyVersion, nack);
        if nack.ClosesSocket() {
            Reaction::ReplyAndClose(vecMessage)
        } else {
            Reaction::Reply(vecMessage)
        }
    }

    /// The vehicle announcement, which is also the identification response.
    pub fn BuildAnnouncement(&self, byReplyVersion: u8) -> Vec<u8> {
        let identity = self.Identity();
        let announcement = VehicleAnnouncement {
            m_arrVin: identity.VinBytes(),
            m_u16LogicalAddress: self.m_u16EntityAddress,
            m_arrEid: identity.EidBytes(),
            m_arrGid: identity.GidBytes(),
            m_byFurtherActionRequired: identity.m_byFurtherActionRequired,
            m_optBySyncStatus: Some(identity.m_byVinGidSyncStatus),
        };
        header::WriteMessage(
            byReplyVersion,
            PayloadType::VehicleAnnouncement,
            &announcement.ToBytes(),
        )
    }

    /// What the loaded vehicle says about itself, or nothing programmed when none is loaded.
    fn Identity(&self) -> VehicleIdentity {
        self.m_arcSimulation
            .lock()
            .expect("simulation mutex poisoned")
            .Vehicle()
            .map(|vehicle| vehicle.m_identity.clone())
            .unwrap_or_default()
    }
}

/// Turn a forced routing activation code into the outcome that carries it.
///
/// Only the codes this entity can actually produce are accepted; a value it has no meaning for
/// is ignored rather than sent, because putting an undefined response code on the wire tests a
/// tester against something no vehicle would ever say.
fn ForcedRoutingOutcome(byCode: u8) -> Option<RoutingActivationOutcome> {
    let outcome = match byCode {
        0x00 => RoutingActivationOutcome::DeniedUnknownSourceAddress,
        0x01 => RoutingActivationOutcome::DeniedAllSocketsRegistered,
        0x02 => RoutingActivationOutcome::DeniedSourceAddressMismatch,
        0x03 => RoutingActivationOutcome::DeniedSourceAddressInUse,
        0x04 => RoutingActivationOutcome::DeniedMissingAuthentication,
        0x06 => RoutingActivationOutcome::DeniedUnsupportedActivationType,
        0x10 => RoutingActivationOutcome::Activated,
        _ => return None,
    };
    Some(outcome)
}

/// Turn a forced diagnostic NACK code into the one it names.
fn ForcedDiagnosticNack(byCode: u8) -> Option<DiagnosticNack> {
    let nack = match byCode {
        0x02 => DiagnosticNack::InvalidSourceAddress,
        0x03 => DiagnosticNack::UnknownTargetAddress,
        0x04 => DiagnosticNack::MessageTooLarge,
        0x05 => DiagnosticNack::OutOfMemory,
        0x06 => DiagnosticNack::TargetUnreachable,
        _ => return None,
    };
    Some(nack)
}

/// Whether this entity will talk to a tester at that address.
///
/// ISO 13400-2 Table 13 reserves `0x0E00`–`0x0FFF` for external test equipment, and the standard
/// never enumerates an accept-list beyond that — definition 3.13 says only "not listed in the
/// connection table entry", which makes it policy. Accepting the reserved client block and
/// refusing everything else is the defensible default: it means an ECU address used by mistake
/// as a tester address is caught rather than silently honoured.
pub fn IsAcceptableTesterAddress(u16SourceAddress: u16) -> bool {
    (0x0E00..=0x0FFF).contains(&u16SourceAddress)
}

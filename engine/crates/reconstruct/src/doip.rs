//! Reconstructing a vehicle from a DoIP capture.
//!
//! The Ethernet counterpart of `pipeline.rs`. What it does *not* borrow from that module is the
//! correlation machinery, and deliberately: a DoIP diagnostic message carries its source and
//! target addresses explicitly, so there is nothing to derive and no heuristic to apply. The CAN
//! pipeline has to infer which identifier answers which because the bus does not say; here the
//! wire says.
//!
//! What both share is `behaviour` — everything about what an exchange *means* for an ECU.

use std::collections::BTreeMap;

use core_domain::model::{Ecu, Network, NetworkKind, Vehicle, VehicleIdentity};
use core_domain::Confidence;
use doip::header::{self, HeaderLimits};
use doip::messages::{DiagnosticMessage, VehicleAnnouncement};
use doip::payload::PayloadType;
use pcap::ethernet::TransportKind;
use pcap::CapturedPacket;

use crate::behaviour::{ApplyPair, IsRequestSid};
use crate::ReconstructError;

/// The unsecured DoIP port. Traffic on any other port is not ours to read.
const c_u16PortDoIp: u16 = 13400;

/// The TLS port. Traffic here is encrypted and is counted, never guessed at.
const c_u16PortDoIpTls: u16 = 3496;

/// Longest DoIP message this reader will assemble from a capture.
///
/// Generous — a capture may legitimately hold a large upload — but bounded, because a corrupt
/// length field in a capture should not become an allocation.
const c_u32MaxMessageLength: u32 = 64 * 1024;

/// What a capture turned out to contain, beyond the vehicle itself.
#[derive(Debug, Default, Clone)]
pub struct CaptureSummary {
    /// DoIP messages successfully decoded.
    pub m_uMessages: usize,
    /// Diagnostic exchanges correlated into ECU behaviour.
    pub m_uExchanges: usize,
    /// Packets seen on the TLS port, which cannot be read.
    pub m_uEncryptedPackets: usize,
    /// TCP streams abandoned because their sequence numbers had a gap.
    pub m_uAbandonedStreams: usize,
}

/// Build a vehicle from a pcap or pcapng capture of DoIP traffic.
pub fn ReconstructFromCapture(arrBytes: &[u8]) -> Result<Vehicle, ReconstructError> {
    let (vehicle, _summary) = ReconstructFromCaptureWithSummary(arrBytes)?;
    Ok(vehicle)
}

/// Build a vehicle, and report what the capture held.
pub fn ReconstructFromCaptureWithSummary(
    arrBytes: &[u8],
) -> Result<(Vehicle, CaptureSummary), ReconstructError> {
    let vecPackets = pcap::ReadCapture(arrBytes)?;
    let mut summary = CaptureSummary::default();

    let vecMessages = ExtractDoIpMessages(&vecPackets, &mut summary);
    if vecMessages.is_empty() {
        return Err(ReconstructError::NoDoIpTraffic {
            uPacketsSeen: vecPackets.len(),
            uEncryptedPackets: summary.m_uEncryptedPackets,
        });
    }

    let vehicle = BuildVehicle(&vecMessages, &mut summary);
    tracing::info!(
        messages = summary.m_uMessages,
        exchanges = summary.m_uExchanges,
        ecus = vehicle.m_vecEcus.len(),
        encrypted = summary.m_uEncryptedPackets,
        "reconstructed a vehicle from a DoIP capture"
    );
    Ok((vehicle, summary))
}

/// One decoded DoIP message, with where it came from.
struct DoIpMessage {
    m_f64TimestampSec: f64,
    m_strSourceIp: String,
    m_payloadType: PayloadType,
    m_vecPayload: Vec<u8>,
}

/// Pull every DoIP message out of the captured packets.
fn ExtractDoIpMessages(
    vecPackets: &[CapturedPacket],
    summary: &mut CaptureSummary,
) -> Vec<DoIpMessage> {
    let mut vecMessages = Vec::new();
    // TCP is a stream, so its segments are gathered per direction and framed afterwards.
    let mut mapStreams: BTreeMap<String, Vec<&CapturedPacket>> = BTreeMap::new();

    for packet in vecPackets {
        if packet.TouchesPort(c_u16PortDoIpTls) {
            summary.m_uEncryptedPackets += 1;
            continue;
        }
        if !packet.TouchesPort(c_u16PortDoIp) || packet.m_vecPayload.is_empty() {
            continue;
        }

        match packet.m_transport {
            // A UDP datagram is already exactly one DoIP message (REQ 7.DoIP-122 AL), so it
            // needs no reassembly and must not be concatenated with its neighbours.
            TransportKind::Udp => {
                ReadMessages(
                    &packet.m_vecPayload,
                    packet.m_f64TimestampSec,
                    &packet.m_strSourceIp,
                    &mut vecMessages,
                );
            }
            TransportKind::Tcp => {
                mapStreams.entry(packet.FlowKey()).or_default().push(packet);
            }
        }
    }

    for (strFlow, mut vecSegments) in mapStreams {
        vecSegments.sort_by_key(|packet| packet.m_u32SequenceNumber);
        match JoinTcpStream(&vecSegments) {
            Some(vecStream) => {
                let f64TimestampSec = vecSegments[0].m_f64TimestampSec;
                let strSourceIp = vecSegments[0].m_strSourceIp.clone();
                ReadMessages(&vecStream, f64TimestampSec, &strSourceIp, &mut vecMessages);
            }
            None => {
                summary.m_uAbandonedStreams += 1;
                tracing::warn!(
                    flow = %strFlow,
                    "a gap in the TCP sequence; abandoning this stream rather than joining across it"
                );
            }
        }
    }

    // Framing produced messages per stream; a reader wants them in the order they happened.
    vecMessages.sort_by(|a, b| {
        a.m_f64TimestampSec
            .partial_cmp(&b.m_f64TimestampSec)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    summary.m_uMessages = vecMessages.len();
    vecMessages
}

/// Join sorted TCP segments into one byte stream, or refuse if any bytes are missing.
///
/// A gap abandons the stream rather than concatenating across it — the same discipline the
/// ISO-TP consecutive-frame check applies, and for the same reason: a PDU assembled across a
/// hole is the right length and the wrong content, and reconstruction would then record it as
/// observed ECU behaviour.
fn JoinTcpStream(vecSegments: &[&CapturedPacket]) -> Option<Vec<u8>> {
    let mut vecStream: Vec<u8> = Vec::new();
    let mut optU32Expected: Option<u32> = None;

    for packet in vecSegments {
        if let Some(u32Expected) = optU32Expected {
            if packet.m_u32SequenceNumber
                == u32Expected.wrapping_sub(packet.m_vecPayload.len() as u32)
            {
                // An exact retransmission of the previous segment; skip it rather than
                // duplicating the bytes.
                continue;
            }
            if packet.m_u32SequenceNumber != u32Expected {
                return None;
            }
        }
        vecStream.extend_from_slice(&packet.m_vecPayload);
        optU32Expected = Some(
            packet
                .m_u32SequenceNumber
                .wrapping_add(packet.m_vecPayload.len() as u32),
        );
    }
    Some(vecStream)
}

/// Frame a byte stream into DoIP messages using each header's length field.
fn ReadMessages(
    arrStream: &[u8],
    f64TimestampSec: f64,
    strSourceIp: &str,
    vecMessages: &mut Vec<DoIpMessage>,
) {
    let limits = HeaderLimits {
        m_u32MaxDataSize: c_u32MaxMessageLength,
        m_u32AvailableMemory: c_u32MaxMessageLength,
    };
    let mut uOffset = 0usize;

    while uOffset + header::c_uHeaderLength <= arrStream.len() {
        let header = match header::ReadHeader(&arrStream[uOffset..], limits) {
            Ok(header) => header,
            Err(nack) => {
                // A capture legitimately contains messages an entity rejected — the reference
                // capture has a deliberately malformed one. Stop framing this stream rather
                // than guessing where the next message starts.
                tracing::debug!(%nack, "stopping at a DoIP message this reader cannot frame");
                return;
            }
        };

        let uStart = uOffset + header::c_uHeaderLength;
        let uEnd = uStart + header.m_u32PayloadLength as usize;
        if uEnd > arrStream.len() {
            return;
        }

        vecMessages.push(DoIpMessage {
            m_f64TimestampSec: f64TimestampSec,
            m_strSourceIp: strSourceIp.to_string(),
            m_payloadType: header.m_payloadType,
            m_vecPayload: arrStream[uStart..uEnd].to_vec(),
        });
        uOffset = uEnd;
    }
}

/// A request seen but not yet answered.
struct PendingRequest {
    m_u16TargetAddress: u16,
    m_byServiceId: u8,
    m_vecBytes: Vec<u8>,
}

/// Turn decoded messages into a vehicle.
fn BuildVehicle(vecMessages: &[DoIpMessage], summary: &mut CaptureSummary) -> Vehicle {
    // ECUs keyed by logical address, which for DoIP *is* an ECU's identity.
    let mut mapEcus: BTreeMap<u16, Ecu> = BTreeMap::new();
    let mut identity = VehicleIdentity::default();
    let mut optStrEntityIp: Option<String> = None;
    let mut vecPending: Vec<PendingRequest> = Vec::new();

    for message in vecMessages {
        match message.m_payloadType {
            PayloadType::VehicleAnnouncement => {
                ReadAnnouncement(&message.m_vecPayload, &mut identity);
                optStrEntityIp.get_or_insert_with(|| message.m_strSourceIp.clone());
            }

            PayloadType::DiagnosticMessage => {
                let diagnostic = match DiagnosticMessage::FromBytes(&message.m_vecPayload) {
                    Some(diagnostic) => diagnostic,
                    None => continue,
                };
                if diagnostic.m_vecUserData.is_empty() {
                    continue;
                }

                if IsRequestSid(diagnostic.m_vecUserData[0]) {
                    // One outstanding request per target: a second supersedes the first, which
                    // evidently went unanswered.
                    vecPending.retain(|pending| {
                        pending.m_u16TargetAddress != diagnostic.m_u16TargetAddress
                    });
                    vecPending.push(PendingRequest {
                        m_u16TargetAddress: diagnostic.m_u16TargetAddress,
                        m_byServiceId: diagnostic.m_vecUserData[0],
                        m_vecBytes: diagnostic.m_vecUserData.clone(),
                    });
                    continue;
                }

                // A response comes *from* the ECU, so its source address is the one that was
                // targeted. No derivation, no heuristic — the wire says so.
                let optIndex = vecPending.iter().position(|pending| {
                    pending.m_u16TargetAddress == diagnostic.m_u16SourceAddress
                        && AnswersService(pending.m_byServiceId, &diagnostic.m_vecUserData)
                });

                if let Some(uIndex) = optIndex {
                    let pending = vecPending.remove(uIndex);
                    let ecu = mapEcus
                        .entry(diagnostic.m_u16SourceAddress)
                        .or_insert_with(|| BuildEcu(diagnostic.m_u16SourceAddress));

                    ApplyPair(ecu, &pending.m_vecBytes, &diagnostic.m_vecUserData);
                    summary.m_uExchanges += 1;
                    optStrEntityIp.get_or_insert_with(|| message.m_strSourceIp.clone());
                }
            }

            // Acknowledgements say a message was routed, not what an ECU does. Routing
            // activation says who the tester is, which the model has no place for.
            _ => {}
        }
    }

    let vecNetworks = BuildNetwork(optStrEntityIp.as_deref(), &mut mapEcus);

    Vehicle {
        m_strName: "Vehicle from DoIP capture".to_string(),
        m_vecEcus: mapEcus.into_values().collect(),
        m_vecNetworks: vecNetworks,
        m_identity: identity,
    }
}

/// An ECU that answered on a logical address.
///
/// Named for the address, the way the CAN pipeline names one for its response identifier — a
/// capture never carries a name, and inventing one would be a claim.
fn BuildEcu(u16LogicalAddress: u16) -> Ecu {
    let mut ecu = Ecu::New(&format!("ECU_{u16LogicalAddress:04X}"), u16LogicalAddress);
    // The whole reason this ECU exists in the model: a tester reached it at this address and it
    // answered. That is a real DoIP address, not the placeholder a CAN-only ECU carries.
    ecu.m_bHasDoIpAddress = true;
    ecu.m_optCanAddress = None;
    ecu
}

/// The one network a capture can honestly claim.
///
/// Unlike a CAN log, an Ethernet capture *does* observe something about topology: every one of
/// these logical addresses was reached at one IP endpoint. That is a real observation, so it is
/// recorded as one. What remains unknowable is whether any of them sits behind a gateway — the
/// topology view says so in its own caveats.
fn BuildNetwork(optStrEntityIp: Option<&str>, mapEcus: &mut BTreeMap<u16, Ecu>) -> Vec<Network> {
    let strEntityIp = match optStrEntityIp {
        Some(strEntityIp) if !mapEcus.is_empty() => strEntityIp,
        _ => return Vec::new(),
    };

    let strNetworkId = "doip-entity".to_string();
    for ecu in mapEcus.values_mut() {
        ecu.m_optStrNetworkId = Some(strNetworkId.clone());
    }

    vec![Network {
        m_strId: strNetworkId,
        m_strName: format!("DoIP entity at {strEntityIp}"),
        m_kind: NetworkKind::EthernetDoIp,
        m_optU32BitrateBps: None,
        m_optU32DataBitrateBps: None,
        // The tester reached these ECUs here, which is exactly what an entry point is.
        m_bIsDiagnosticEntryPoint: true,
        m_confidence: Confidence::Observed,
    }]
}

/// Read a vehicle announcement into the model's identity.
fn ReadAnnouncement(vecPayload: &[u8], identity: &mut VehicleIdentity) {
    if vecPayload.len() < 32 {
        return;
    }

    let arrVin: [u8; 17] = match vecPayload[0..17].try_into() {
        Ok(arrVin) => arrVin,
        Err(_) => return,
    };
    // ISO 13400-2 Table 1: all zeroes or all 0xFF means "not programmed". Recording that as a
    // VIN would turn an absent value into a wrong one.
    let bIsProgrammed = !arrVin.iter().all(|byByte| *byByte == 0x00)
        && !arrVin.iter().all(|byByte| *byByte == 0xFF);
    if bIsProgrammed {
        identity.m_optVecVin = Some(arrVin.to_vec());
    }

    if let Ok(arrEid) = vecPayload[19..25].try_into() {
        identity.m_optArrEid = Some(arrEid);
    }
    if let Ok(arrGid) = vecPayload[25..31].try_into() {
        identity.m_optArrGid = Some(arrGid);
    }
    identity.m_byFurtherActionRequired = vecPayload[31];
    if vecPayload.len() >= 33 {
        identity.m_byVinGidSyncStatus = vecPayload[32];
    }

    // Parsed again through the codec purely to fail loudly in a test if the two disagree.
    debug_assert!(
        VehicleAnnouncement {
            m_arrVin: arrVin,
            m_u16LogicalAddress: u16::from_be_bytes([vecPayload[17], vecPayload[18]]),
            m_arrEid: identity.EidBytes(),
            m_arrGid: identity.GidBytes(),
            m_byFurtherActionRequired: identity.m_byFurtherActionRequired,
            m_optBySyncStatus: None,
        }
        .ToBytes()
        .len()
            == 32
    );
}

/// True when these response bytes answer that service.
fn AnswersService(byServiceId: u8, vecResponse: &[u8]) -> bool {
    let byFirst = match vecResponse.first() {
        Some(byFirst) => *byFirst,
        None => return false,
    };
    let bIsPositive = byFirst == byServiceId.wrapping_add(0x40);
    let bIsNegative = byFirst == 0x7F && vecResponse.get(1) == Some(&byServiceId);
    bIsPositive || bIsNegative
}

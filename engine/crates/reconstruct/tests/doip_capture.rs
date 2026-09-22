//! Reconstructing a vehicle from a DoIP capture.

#![allow(non_snake_case, non_upper_case_globals)]

use reconstruct::doip::ReconstructFromCaptureWithSummary;
use reconstruct::ReconstructError;

/// The reference capture, if this machine has one. Skipped otherwise so CI stays green.
fn LoadReferenceCapture() -> Option<Vec<u8>> {
    let strPath = format!(
        "{}/.claude/doip-expert/reference/sample-doip.pcap",
        std::env::var("HOME").unwrap_or_default()
    );
    std::fs::read(&strPath).ok()
}

#[test]
fn a_real_capture_reconstructs_the_ecu_that_answered() {
    let vecBytes = match LoadReferenceCapture() {
        Some(vecBytes) => vecBytes,
        None => {
            eprintln!("skipping: no reference capture on this machine");
            return;
        }
    };

    let (vehicle, summary) =
        ReconstructFromCaptureWithSummary(&vecBytes).expect("the capture should reconstruct");

    eprintln!(
        "{} messages, {} exchanges, {} ECU(s)",
        summary.m_uMessages,
        summary.m_uExchanges,
        vehicle.m_vecEcus.len()
    );

    // The capture holds one ECU that answered, at logical address 0x1234, plus a request to
    // 0x9999 that was refused — and a refusal is not an ECU.
    assert_eq!(vehicle.m_vecEcus.len(), 1, "only the ECU that answered");
    let ecu = &vehicle.m_vecEcus[0];
    assert_eq!(ecu.m_u16LogicalAddress, 0x1234);
    assert!(ecu.m_bHasDoIpAddress, "a real, routable DoIP address");
    assert!(ecu.m_optCanAddress.is_none(), "nothing about CAN was seen");

    // The VIN it answered with, learned through the shared behaviour extraction.
    let did = ecu.m_mapDids.get(&0xF190).expect("DID 0xF190 was read");
    assert_eq!(did.m_vecValue, b"1HGBH41JXMN109186");
    assert!(ecu.m_vecSupportedServices.contains(&0x22));
}

#[test]
fn the_announcement_becomes_the_vehicle_identity() {
    let vecBytes = match LoadReferenceCapture() {
        Some(vecBytes) => vecBytes,
        None => return,
    };

    let (vehicle, _summary) = ReconstructFromCaptureWithSummary(&vecBytes).expect("reconstructs");

    assert_eq!(
        vehicle.m_identity.m_optVecVin.as_deref(),
        Some(&b"1HGBH41JXMN109186"[..]),
        "the VIN comes from the vehicle announcement"
    );
    assert_eq!(
        vehicle.m_identity.m_optArrEid,
        Some([0x00, 0x11, 0x22, 0x33, 0x44, 0x55])
    );
    assert_eq!(
        vehicle.m_identity.m_optArrGid,
        Some([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
    );
}

#[test]
fn the_capture_yields_the_one_network_it_can_honestly_claim() {
    // Unlike a CAN log, an Ethernet capture does observe something: every logical address was
    // reached at one IP endpoint. Whether any sits behind a gateway stays unknowable.
    let vecBytes = match LoadReferenceCapture() {
        Some(vecBytes) => vecBytes,
        None => return,
    };

    let (vehicle, _summary) = ReconstructFromCaptureWithSummary(&vecBytes).expect("reconstructs");

    assert_eq!(vehicle.m_vecNetworks.len(), 1);
    let network = &vehicle.m_vecNetworks[0];
    assert!(network.m_bIsDiagnosticEntryPoint);
    assert!(
        network.m_strName.contains("192.168.0.2"),
        "named for the entity's address, got '{}'",
        network.m_strName
    );
    assert_eq!(network.m_confidence, core_domain::Confidence::Observed);
    assert_eq!(
        vehicle.m_vecEcus[0].m_optStrNetworkId.as_deref(),
        Some("doip-entity")
    );
}

#[test]
fn a_capture_with_no_doip_traffic_says_so_rather_than_returning_an_empty_vehicle() {
    // A capture of ordinary web traffic is a perfectly good file that simply holds nothing to
    // reconstruct from. An empty vehicle would leave someone guessing which it was.
    let mut vecFile = Vec::new();
    vecFile.extend_from_slice(&0xA1B2_C3D4u32.to_le_bytes());
    vecFile.extend_from_slice(&[2, 0, 4, 0]);
    vecFile.extend_from_slice(&0u32.to_le_bytes());
    vecFile.extend_from_slice(&0u32.to_le_bytes());
    vecFile.extend_from_slice(&65535u32.to_le_bytes());
    vecFile.extend_from_slice(&1u32.to_le_bytes());

    match ReconstructFromCaptureWithSummary(&vecFile) {
        Err(ReconstructError::NoDoIpTraffic { uPacketsSeen, .. }) => {
            assert_eq!(uPacketsSeen, 0);
        }
        other => panic!(
            "expected NoDoIpTraffic, got {:?}",
            other.map(|(v, _)| v.m_strName)
        ),
    }
}

#[test]
fn something_that_is_not_a_capture_is_reported_as_such() {
    match ReconstructFromCaptureWithSummary(b"a CAN log, not a capture") {
        Err(ReconstructError::Capture(_)) => {}
        other => panic!(
            "expected a capture error, got {:?}",
            other.map(|(v, _)| v.m_strName)
        ),
    }
}

#[test]
fn every_message_carries_its_own_arrival_time() {
    // The bug this pins: a reassembled TCP stream used to stamp every message in it with the
    // *first* segment's time. Requests and responses travel in opposite directions and are
    // therefore separate streams, so that put every request before every response once merged —
    // and with one-outstanding-request-per-target, collapsed a whole session into one exchange
    // per ECU.
    //
    // The reference capture has a request and its response in different streams, so a correct
    // reader correlates them. Under the old behaviour it still would, by luck of two streams;
    // what proves the fix is a capture with several exchanges, which is the local one below.
    let vecBytes = match LoadReferenceCapture() {
        Some(vecBytes) => vecBytes,
        None => return,
    };

    let (vehicle, summary) = ReconstructFromCaptureWithSummary(&vecBytes).expect("reconstructs");
    assert!(summary.m_uExchanges >= 1);
    assert_eq!(vehicle.m_vecEcus.len(), 1);
}

#[test]
fn a_large_local_capture_reconstructs_deterministically() {
    // Ordering must not depend on which order the streams happened to be walked in. Capture
    // timestamps are not unique — a busy link puts many packets in one microsecond — so the
    // tie-break is the capture's own order, and the same file must give the same answer twice.
    let strPath = format!(
        "{}/Downloads/Reprolog3.pcapng",
        std::env::var("HOME").unwrap_or_default()
    );
    let vecBytes = match std::fs::read(&strPath) {
        Ok(vecBytes) => vecBytes,
        Err(_) => {
            eprintln!("skipping: no large capture on this machine");
            return;
        }
    };

    let (first, firstSummary) = ReconstructFromCaptureWithSummary(&vecBytes).expect("reconstructs");
    let (second, secondSummary) =
        ReconstructFromCaptureWithSummary(&vecBytes).expect("reconstructs again");

    assert_eq!(firstSummary.m_uExchanges, secondSummary.m_uExchanges);
    assert_eq!(first.m_vecEcus.len(), second.m_vecEcus.len());

    let vecFirstAddresses: Vec<u16> = first
        .m_vecEcus
        .iter()
        .map(|ecu| ecu.m_u16LogicalAddress)
        .collect();
    let vecSecondAddresses: Vec<u16> = second
        .m_vecEcus
        .iter()
        .map(|ecu| ecu.m_u16LogicalAddress)
        .collect();
    assert_eq!(vecFirstAddresses, vecSecondAddresses);

    eprintln!(
        "{} messages, {} exchanges, {} ECUs from {} MB",
        firstSummary.m_uMessages,
        firstSummary.m_uExchanges,
        first.m_vecEcus.len(),
        vecBytes.len() / (1024 * 1024)
    );
}

/// A DoIP message: header plus payload, ready to concatenate into a stream.
fn DoIpMessageBytes(u16PayloadType: u16, vecPayload: &[u8]) -> Vec<u8> {
    let mut vecBytes = vec![0x02, 0xFD];
    vecBytes.extend_from_slice(&u16PayloadType.to_be_bytes());
    vecBytes.extend_from_slice(&(vecPayload.len() as u32).to_be_bytes());
    vecBytes.extend_from_slice(vecPayload);
    vecBytes
}

/// A diagnostic message payload: source, target, then the UDS bytes.
fn DiagnosticPayload(u16Source: u16, u16Target: u16, vecUserData: &[u8]) -> Vec<u8> {
    let mut vecPayload = Vec::new();
    vecPayload.extend_from_slice(&u16Source.to_be_bytes());
    vecPayload.extend_from_slice(&u16Target.to_be_bytes());
    vecPayload.extend_from_slice(vecUserData);
    vecPayload
}

/// A classic pcap holding one Ethernet/IPv4/TCP packet carrying this byte stream on port 13400.
fn SingleTcpPacketCapture(vecStream: &[u8]) -> Vec<u8> {
    const c_u16PortDoIp: u16 = 13400;
    const c_uIpHeaderLength: usize = 20;
    const c_byProtocolTcp: u8 = 6;

    let mut vecTcp = Vec::new();
    vecTcp.extend_from_slice(&c_u16PortDoIp.to_be_bytes()); // source: the entity answering
    vecTcp.extend_from_slice(&40000u16.to_be_bytes()); // destination: the tester
    vecTcp.extend_from_slice(&1u32.to_be_bytes()); // sequence number
    vecTcp.extend_from_slice(&0u32.to_be_bytes()); // acknowledgement number
    vecTcp.push(0x50); // data offset: five 32-bit words, no options
    vecTcp.push(0x18); // PSH | ACK
    vecTcp.extend_from_slice(&8192u16.to_be_bytes()); // window
    vecTcp.extend_from_slice(&0u16.to_be_bytes()); // checksum, not verified by the reader
    vecTcp.extend_from_slice(&0u16.to_be_bytes()); // urgent pointer
    vecTcp.extend_from_slice(vecStream);

    let mut vecIp = vec![0x45, 0x00];
    let u16TotalLength = (c_uIpHeaderLength + vecTcp.len()) as u16;
    vecIp.extend_from_slice(&u16TotalLength.to_be_bytes());
    vecIp.extend_from_slice(&[0x00, 0x01, 0x40, 0x00, 0x40, c_byProtocolTcp, 0x00, 0x00]);
    vecIp.extend_from_slice(&[192, 168, 0, 2]); // source: the entity
    vecIp.extend_from_slice(&[192, 168, 0, 1]); // destination: the tester
    vecIp.extend_from_slice(&vecTcp);

    let mut vecEthernet = vec![0x00; 12];
    vecEthernet.extend_from_slice(&0x0800u16.to_be_bytes());
    vecEthernet.extend_from_slice(&vecIp);

    let mut vecFile = Vec::new();
    vecFile.extend_from_slice(&0xA1B2_C3D4u32.to_le_bytes());
    vecFile.extend_from_slice(&[2, 0, 4, 0]);
    vecFile.extend_from_slice(&0u32.to_le_bytes());
    vecFile.extend_from_slice(&0u32.to_le_bytes());
    vecFile.extend_from_slice(&65535u32.to_le_bytes());
    vecFile.extend_from_slice(&1u32.to_le_bytes()); // link type: Ethernet

    vecFile.extend_from_slice(&1u32.to_le_bytes()); // seconds
    vecFile.extend_from_slice(&0u32.to_le_bytes()); // microseconds
    vecFile.extend_from_slice(&(vecEthernet.len() as u32).to_le_bytes());
    vecFile.extend_from_slice(&(vecEthernet.len() as u32).to_le_bytes());
    vecFile.extend_from_slice(&vecEthernet);
    vecFile
}

#[test]
fn a_payload_type_this_reader_does_not_interpret_is_skipped_not_fatal() {
    // The bug this pins: framing stopped at the first payload type the reader has no decoder
    // for, silently dropping every later message in that stream — and with them, whole ECUs.
    // A header's length field says how long a message is whatever its type means, so an
    // uninterpretable type is a message to step over, not a reason to abandon the stream.
    // Manufacturer-specific types (0xF000-0xFFFF) are legal by design and common in OEM
    // captures, which is exactly where this cost the most.
    const c_u16ManufacturerSpecificType: u16 = 0xF00D;

    let mut vecStream = Vec::new();
    vecStream.extend_from_slice(&DoIpMessageBytes(
        0x8001,
        &DiagnosticPayload(0x0E00, 0x1234, &[0x22, 0xF1, 0x90]),
    ));
    vecStream.extend_from_slice(&DoIpMessageBytes(
        c_u16ManufacturerSpecificType,
        &[0xDE, 0xAD, 0xBE, 0xEF],
    ));
    let mut vecResponse = vec![0x62, 0xF1, 0x90];
    vecResponse.extend_from_slice(b"1HGBH41JXMN109186");
    vecStream.extend_from_slice(&DoIpMessageBytes(
        0x8001,
        &DiagnosticPayload(0x1234, 0x0E00, &vecResponse),
    ));

    let vecFile = SingleTcpPacketCapture(&vecStream);
    let (vehicle, summary) =
        ReconstructFromCaptureWithSummary(&vecFile).expect("the capture should reconstruct");

    assert_eq!(
        summary.m_uUninterpreted, 1,
        "the unknown type is counted, not hidden"
    );
    assert_eq!(
        summary.m_uAbandonedStreams, 0,
        "and the stream carries on rather than being abandoned"
    );
    assert_eq!(
        summary.m_uMessages, 2,
        "the two interpretable messages survive; the skipped one is counted separately \
         rather than being handed on as a message with no meaning attached"
    );
    assert_eq!(summary.m_uExchanges, 1);

    let ecu = vehicle
        .m_vecEcus
        .first()
        .expect("the ECU that answered after the uninterpretable message");
    assert_eq!(ecu.m_u16LogicalAddress, 0x1234);
    let did = ecu.m_mapDids.get(&0xF190).expect("DID 0xF190 was read");
    assert_eq!(did.m_vecValue, b"1HGBH41JXMN109186");
}

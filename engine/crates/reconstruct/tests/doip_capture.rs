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

//! Golden + round-trip test for CAN-log reconstruction (Phase 2 acceptance, README §24):
//!   1. Reconstruct a Vehicle model from a recorded CAN log (golden assertions).
//!   2. Rebuild a VirtualEcu from that model and replay the log's requests through the real
//!      UDS logic, asserting the simulated responses match what the log observed.

#![allow(non_snake_case)]

use abi_stable::std_types::RVec;
use application::ProtocolHandler;
use ecu::VirtualEcu;
use plugin_contract::protocol::{REcuSnapshot, RProtocolOutcome};
use reconstruct::ReconstructFromLogText;

/// Bridge the UDS plugin's pure handler to the engine's `ProtocolHandler` trait (same as the
/// Phase 1 test — deterministic, no dynamic library needed).
struct UdsHandler;

impl ProtocolHandler for UdsHandler {
    fn Handle(&self, vecRequest: RVec<u8>, snapshot: REcuSnapshot) -> RProtocolOutcome {
        let reply = uds_plugin::handler::HandleRequest(vecRequest.as_slice(), &snapshot);
        RProtocolOutcome {
            m_vecResponse: RVec::from(reply.m_vecResponse),
            m_vecChanges: RVec::from(reply.m_vecChanges),
        }
    }
    fn Name(&self) -> &str {
        "uds"
    }
}

const SAMPLE_LOG: &str = include_str!("fixtures/session.log");

#[test]
fn golden_reconstruction_from_log() {
    let vehicle = ReconstructFromLogText(SAMPLE_LOG).expect("reconstruction should succeed");

    assert_eq!(vehicle.m_vecEcus.len(), 1, "one ECU (response id 0x7E8)");
    let ecu = &vehicle.m_vecEcus[0];

    assert_eq!(ecu.m_u16LogicalAddress, 0x7E8);
    for byService in [0x10, 0x22, 0x3E, 0x19] {
        assert!(
            ecu.m_vecSupportedServices.contains(&byService),
            "service 0x{byService:02X} should be discovered"
        );
    }
    // The multi-frame VIN was reassembled and stored.
    let vin = &ecu.FindDid(0xF190).expect("VIN DID discovered").m_vecValue;
    assert_eq!(vin.as_slice(), b"VIN0123456789ABCD");
    // The DTC was decoded.
    assert_eq!(ecu.m_vecDtcs.len(), 1);
    assert_eq!(ecu.m_vecDtcs[0].m_u32Code, 0x123456);
}

#[test]
fn reconstructed_ecu_reproduces_observed_responses() {
    let vehicle = ReconstructFromLogText(SAMPLE_LOG).expect("reconstruction should succeed");
    let handler = UdsHandler;
    let mut ecu = VirtualEcu::New(vehicle.m_vecEcus[0].clone());

    // Enter the extended session (observed in the log) and read the VIN back.
    let response = ecu.ProcessRequest(&handler, &[0x10, 0x03]);
    assert_eq!(&response[0..2], &[0x50, 0x03]);

    let response = ecu.ProcessRequest(&handler, &[0x22, 0xF1, 0x90]);
    assert_eq!(&response[0..3], &[0x62, 0xF1, 0x90]);
    assert_eq!(&response[3..], b"VIN0123456789ABCD");

    // TesterPresent and ReadDTC reproduce the observed responses.
    assert_eq!(
        ecu.ProcessRequest(&handler, &[0x3E, 0x00]),
        vec![0x7E, 0x00]
    );
    assert_eq!(
        ecu.ProcessRequest(&handler, &[0x19, 0x02, 0xFF]),
        vec![0x59, 0x02, 0xFF, 0x12, 0x34, 0x56, 0x2F]
    );
}

/// A timestamped diagnostic-trace capture, the format service tools produce. Unlike the other
/// two supported formats it marks each frame's direction, and it mixes 29-bit normal-fixed
/// addressing with an OEM 11-bit pair on one bus.
const SERVICE_TRACE: &str = include_str!("fixtures/service_trace.txt");

#[test]
fn golden_reconstruction_from_a_service_trace() {
    let vehicle = ReconstructFromLogText(SERVICE_TRACE).expect("reconstruction should succeed");

    // Two ECUs answered. The tester also addressed 0x18DADAF1 twice and got nothing back, so
    // no ECU is invented for it — an unanswered request is not evidence of a server.
    assert_eq!(vehicle.m_vecEcus.len(), 2);

    let gateway = vehicle
        .m_vecEcus
        .iter()
        .find(|ecu| ecu.m_strName == "ECU_18DAF1D4")
        .expect("the 29-bit ECU");
    let address = gateway.m_optCanAddress.expect("CAN address recorded");
    assert_eq!(address.m_u32RequestCanId, 0x18DAD4F1);
    assert_eq!(address.m_u32ResponseCanId, 0x18DAF1D4);
    // The ECU's own address is the source byte of the identifier it answers on.
    assert_eq!(gateway.m_u16LogicalAddress, 0xD4);

    // A VIN is 17 characters (ISO 3779), carried here as a segmented ISO-TP message.
    let vin = &gateway.FindDid(0xF190).expect("VIN discovered").m_vecValue;
    assert_eq!(vin.as_slice(), b"JN8AY3NY5T9100001");
    assert_eq!(vin.len(), 17);

    // The OEM pair is +0x20, which no standard defines — it can only come from observing both
    // identifiers, never from deriving one.
    let body = vehicle
        .m_vecEcus
        .iter()
        .find(|ecu| ecu.m_strName == "ECU_765")
        .expect("the OEM-paired ECU");
    let bodyAddress = body.m_optCanAddress.expect("CAN address recorded");
    assert_eq!(bodyAddress.m_u32RequestCanId, 0x745);
    assert_eq!(bodyAddress.m_u32ResponseCanId, 0x765);
    assert_eq!(body.FindDid(0xF012).unwrap().m_vecValue, b"1234512345");
}

#[test]
fn a_service_trace_ecu_reproduces_its_observed_responses() {
    let vehicle = ReconstructFromLogText(SERVICE_TRACE).expect("reconstruction should succeed");
    let handler = UdsHandler;

    let gateway = vehicle
        .m_vecEcus
        .iter()
        .find(|ecu| ecu.m_strName == "ECU_18DAF1D4")
        .expect("the 29-bit ECU")
        .clone();
    let mut ecu = VirtualEcu::New(gateway);

    let response = ecu.ProcessRequest(&handler, &[0x22, 0xF1, 0x90]);
    assert_eq!(&response[0..3], &[0x62, 0xF1, 0x90]);
    assert_eq!(&response[3..], b"JN8AY3NY5T9100001");

    // The log showed DID 0x1111 refused with requestOutOfRange; the simulation refuses it too,
    // because the DID was never observed rather than because the log is being replayed.
    assert_eq!(
        ecu.ProcessRequest(&handler, &[0x22, 0x11, 0x11]),
        vec![0x7F, 0x22, 0x31]
    );
}

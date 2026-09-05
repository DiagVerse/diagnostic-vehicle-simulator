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

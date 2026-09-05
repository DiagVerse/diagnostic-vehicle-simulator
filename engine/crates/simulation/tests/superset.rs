//! The shipped UDS superset sample must actually answer every service it claims to.
//!
//! A sample file that loads but returns NRC 0x11 to half of it would be worse than no sample:
//! someone would build a test suite against it and discover the gaps one at a time. So these
//! tests drive the real UDS plugin through the real simulation service and check the bytes
//! that come back.

#![allow(non_snake_case, non_upper_case_globals)]

use abi_stable::std_types::RVec;
use application::ProtocolHandler;
use plugin_contract::protocol::{REcuSnapshot, RProtocolOutcome};
use simulation::{RoutingOutcome, SimulationService};

const c_strSuperset: &str = include_str!("../../../../samples/uds-superset.simfile.json");
const c_u32ReferenceEcuRequestId: u32 = 0x7E0;

/// Every service ISO 14229-1 defines, with the request used to exercise it.
///
/// Kept as one list so a service cannot be quietly dropped from the sample: the test that walks
/// it is the sample's specification.
const c_arrServiceProbes: &[(u8, &str, &[u8])] = &[
    (0x10, "DiagnosticSessionControl", &[0x10, 0x03]),
    (0x11, "ECUReset", &[0x11, 0x04]),
    (
        0x14,
        "ClearDiagnosticInformation",
        &[0x14, 0xFF, 0xFF, 0xFF],
    ),
    (0x19, "ReadDTCInformation", &[0x19, 0x02, 0xFF]),
    (0x22, "ReadDataByIdentifier", &[0x22, 0xF1, 0x90]),
    (
        0x23,
        "ReadMemoryByAddress",
        &[0x23, 0x14, 0x11, 0x22, 0x33, 0x44, 0x04],
    ),
    (0x24, "ReadScalingDataByIdentifier", &[0x24, 0xF1, 0x90]),
    (0x27, "SecurityAccess", &[0x27, 0x01]),
    (0x28, "CommunicationControl", &[0x28, 0x00, 0x01]),
    (0x29, "Authentication", &[0x29, 0x00]),
    (
        0x2A,
        "ReadDataByPeriodicIdentifier",
        &[0x2A, 0x01, 0xF2, 0x00],
    ),
    (
        0x2C,
        "DynamicallyDefineDataIdentifier",
        &[0x2C, 0x01, 0xF3, 0x00, 0x12, 0x34, 0x56, 0x78],
    ),
    (
        0x2E,
        "WriteDataByIdentifier",
        &[0x2E, 0xF1, 0x90, 0x41, 0x42, 0x43],
    ),
    (
        0x2F,
        "InputOutputControlByIdentifier",
        &[0x2F, 0xF3, 0x01, 0x03, 0x00],
    ),
    (0x31, "RoutineControl", &[0x31, 0x01, 0x02, 0x03, 0x04]),
    (
        0x34,
        "RequestDownload",
        &[
            0x34, 0x00, 0x44, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x10, 0x00,
        ],
    ),
    (
        0x35,
        "RequestUpload",
        &[
            0x35, 0x00, 0x44, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x10, 0x00,
        ],
    ),
    (0x36, "TransferData", &[0x36, 0x01, 0xAA, 0xBB, 0xCC]),
    (0x37, "RequestTransferExit", &[0x37]),
    (
        0x38,
        "RequestFileTransfer",
        &[0x38, 0x01, 0x00, 0x04, 0x54, 0x45, 0x53, 0x54],
    ),
    (
        0x3D,
        "WriteMemoryByAddress",
        &[0x3D, 0x14, 0x11, 0x22, 0x33, 0x44, 0x04, 0x11, 0x22],
    ),
    (0x3E, "TesterPresent", &[0x3E, 0x00]),
    (0x83, "AccessTimingParameter", &[0x83, 0x01]),
    (
        0x84,
        "SecuredDataTransmission",
        &[0x84, 0x00, 0x11, 0x22, 0x33],
    ),
    (0x85, "ControlDTCSetting", &[0x85, 0x01]),
    (0x86, "ResponseOnEvent", &[0x86, 0x01, 0x02]),
    (0x87, "LinkControl", &[0x87, 0x01, 0x01]),
];

/// Bridges the UDS plugin's pure handler to the engine's `ProtocolHandler` port.
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

fn LoadSuperset() -> SimulationService {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromSimFileText(c_strSuperset)
        .expect("the shipped superset sample must load");
    simulation.Start();
    simulation
}

/// Send one request to the reference ECU and return the bytes it answered with.
fn Ask(simulation: &mut SimulationService, vecRequest: &[u8]) -> Vec<u8> {
    let outcome = simulation.ProcessByCanId(c_u32ReferenceEcuRequestId, vecRequest, &UdsHandler);
    match outcome {
        RoutingOutcome::Handled(vecResponses) => vecResponses
            .first()
            .map(|response| response.m_vecResponse.clone())
            .unwrap_or_default(),
        other => panic!("expected an answer, got {other:?}"),
    }
}

#[test]
fn every_iso_14229_service_answers_positively() {
    let mut simulation = LoadSuperset();

    // SecurityAccess is refused in the default session by any ECU worth simulating, so the
    // tester does what a real one does and moves to extended first.
    Ask(&mut simulation, &[0x10, 0x03]);

    let mut vecFailures: Vec<String> = Vec::new();
    for (byServiceId, strName, vecRequest) in c_arrServiceProbes {
        let vecResponse = Ask(&mut simulation, vecRequest);

        let bIsPositive = vecResponse.first() == Some(&(byServiceId.wrapping_add(0x40)));
        if !bIsPositive {
            vecFailures.push(format!(
                "0x{byServiceId:02X} {strName}: answered {vecResponse:02X?}"
            ));
        }

        // 0x11 clears the session back to default, and 0x10 0x02 would move to programming;
        // return to extended so the following probes are not judged in the wrong session.
        Ask(&mut simulation, &[0x10, 0x03]);
    }

    assert!(
        vecFailures.is_empty(),
        "these services did not answer positively:\n  {}",
        vecFailures.join("\n  ")
    );
}

#[test]
fn security_access_is_refused_in_the_default_session_and_granted_in_extended() {
    // Not a limitation of the sample — this is what a real ECU does, and a sample that handed
    // out seeds in the default session would teach a tester the wrong lesson.
    let mut simulation = LoadSuperset();

    let vecRefused = Ask(&mut simulation, &[0x27, 0x01]);
    assert_eq!(vecRefused, vec![0x7F, 0x27, 0x7F]);

    Ask(&mut simulation, &[0x10, 0x03]);
    let vecSeed = Ask(&mut simulation, &[0x27, 0x01]);
    assert_eq!(vecSeed, vec![0x67, 0x01, 0x11, 0x22, 0x33, 0x44]);

    let vecUnlocked = Ask(&mut simulation, &[0x27, 0x02, 0xAA, 0xBB, 0xCC, 0xDD]);
    assert_eq!(vecUnlocked, vec![0x67, 0x02]);
}

#[test]
fn every_read_dtc_information_sub_function_answers() {
    // 0x19 is the service with the most sub-functions in the standard, and the plugin
    // implements exactly one of them. The rest are what the sample is for.
    const c_arrSubFunctions: &[(u8, &[u8])] = &[
        (0x01, &[0x19, 0x01, 0xFF]),
        (0x02, &[0x19, 0x02, 0xFF]),
        (0x03, &[0x19, 0x03]),
        (0x04, &[0x19, 0x04, 0x08, 0x05, 0x11, 0x01]),
        (0x05, &[0x19, 0x05, 0x01]),
        (0x06, &[0x19, 0x06, 0x08, 0x05, 0x11, 0x10]),
        (0x07, &[0x19, 0x07, 0x20, 0x01]),
        (0x08, &[0x19, 0x08, 0x20, 0x01]),
        (0x09, &[0x19, 0x09, 0x08, 0x05, 0x11]),
        (0x0A, &[0x19, 0x0A]),
        (0x0B, &[0x19, 0x0B]),
        (0x0C, &[0x19, 0x0C]),
        (0x0D, &[0x19, 0x0D]),
        (0x0E, &[0x19, 0x0E]),
        (0x0F, &[0x19, 0x0F, 0xFF]),
        (0x10, &[0x19, 0x10, 0x08, 0x05, 0x11, 0x10]),
        (0x11, &[0x19, 0x11, 0xFF]),
        (0x12, &[0x19, 0x12, 0xFF]),
        (0x13, &[0x19, 0x13, 0xFF]),
        (0x14, &[0x19, 0x14]),
        (0x15, &[0x19, 0x15]),
        (0x16, &[0x19, 0x16, 0x10]),
        (0x17, &[0x19, 0x17, 0x01, 0xFF, 0x00, 0x00]),
        (0x18, &[0x19, 0x18, 0x01, 0x08, 0x05, 0x11, 0x01]),
        (0x19, &[0x19, 0x19, 0x01, 0x08, 0x05, 0x11, 0x10]),
        (0x1A, &[0x19, 0x1A, 0x10]),
        (0x42, &[0x19, 0x42, 0x33, 0xFF, 0x00]),
        (0x55, &[0x19, 0x55, 0x33]),
        (0x56, &[0x19, 0x56, 0x33, 0x01]),
    ];

    let mut simulation = LoadSuperset();
    Ask(&mut simulation, &[0x10, 0x03]);

    let mut vecFailures: Vec<String> = Vec::new();
    for (bySubFunction, vecRequest) in c_arrSubFunctions {
        let vecResponse = Ask(&mut simulation, vecRequest);

        let bIsPositive = vecResponse.first() == Some(&0x59);
        let bEchoesSubFunction = vecResponse.get(1) == Some(bySubFunction);
        if !bIsPositive || !bEchoesSubFunction {
            vecFailures.push(format!(
                "0x19 sub-function 0x{bySubFunction:02X}: answered {vecResponse:02X?}"
            ));
        }
    }

    assert!(
        vecFailures.is_empty(),
        "these ReadDTCInformation sub-functions did not answer:\n  {}",
        vecFailures.join("\n  ")
    );
}

#[test]
fn echoed_fields_come_from_the_request_rather_than_being_hard_coded() {
    // The point of echo spans. A tester that checks its own identifier came back is the normal
    // case, and a wildcard override without echo would answer every request with one value.
    let mut simulation = LoadSuperset();
    Ask(&mut simulation, &[0x10, 0x03]);

    // WriteDataByIdentifier echoes the DID.
    assert_eq!(
        Ask(&mut simulation, &[0x2E, 0xF1, 0x90, 0x41, 0x42]),
        vec![0x6E, 0xF1, 0x90]
    );
    assert_eq!(
        Ask(&mut simulation, &[0x2E, 0x01, 0x23, 0x41]),
        vec![0x6E, 0x01, 0x23],
        "a different DID must come back as itself, not as F190"
    );

    // TransferData echoes the block sequence counter.
    assert_eq!(Ask(&mut simulation, &[0x36, 0x01, 0xAA]), vec![0x76, 0x01]);
    assert_eq!(
        Ask(&mut simulation, &[0x36, 0x7F, 0xAA]),
        vec![0x76, 0x7F],
        "the counter is the tester's, and a tester checks it"
    );
}

#[test]
fn a_variable_length_request_is_answered_at_any_length() {
    // What `matchTrailingBytes` is for. Without it an override matches one exact length, so
    // `2E` could be simulated for a three-byte value and nothing else.
    let mut simulation = LoadSuperset();
    Ask(&mut simulation, &[0x10, 0x03]);

    for uValueLength in 1..=16 {
        let mut vecRequest = vec![0x2E, 0xF1, 0x90];
        vecRequest.extend(std::iter::repeat_n(0x41, uValueLength));

        let vecResponse = Ask(&mut simulation, &vecRequest);
        assert_eq!(
            vecResponse,
            vec![0x6E, 0xF1, 0x90],
            "a {uValueLength}-byte value should be accepted like any other"
        );
    }
}

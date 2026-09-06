//! End-to-end tests at frame level: bytes arrive on a bus, bytes go back out.
//!
//! These drive the whole stack — ISO-TP reassembly, routing, the response plan, ISO-TP
//! segmentation — over an in-memory bus, so they assert the exact frames a tester would see
//! without any hardware.

#![allow(non_snake_case, non_upper_case_globals)]

use std::sync::{Arc, Mutex};

use abi_stable::std_types::RVec;
use application::ProtocolHandler;
use bridge::mock::MockBusHandle;
use bridge::CanBridge;
use can::CanFrame;
use core_domain::model::{
    CanAddress, CanAddressingMode, DataIdentifier, Ecu, EcuTiming, SessionType,
};
use core_domain::Confidence;
use isotp::params::{c_byDefaultPaddingByte, IsoTpParameters};
use plugin_contract::protocol::{REcuSnapshot, RProtocolOutcome};
use simulation::SimulationService;

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

const c_strVin: &str = "1HGCM82633A004352";

/// One ECU on 0x7E0/0x7E8 with a VIN, in a simulation ready to be bridged.
fn BuildSimulation() -> Arc<Mutex<SimulationService>> {
    let mut config = Ecu::New("Engine", 0);
    config.m_optCanAddress = Some(CanAddress::NewSpecified(
        0x7E0,
        0x7E8,
        CanAddressingMode::Normal11Bit,
    ));
    config.m_vecSupportedServices = vec![0x10, 0x22, 0x2E, 0x3E];
    config.m_vecSupportedSessions = vec![SessionType::Default, SessionType::Extended];
    config.m_mapDids.insert(
        0xF190,
        DataIdentifier {
            m_u16Id: 0xF190,
            m_vecValue: c_strVin.as_bytes().to_vec(),
            m_confidence: Confidence::Confirmed,
        },
    );

    let mut simulation = SimulationService::New();
    simulation.CreateEmptyVehicle("Bench");
    simulation.AddEcu(config).expect("the ECU");
    Arc::new(Mutex::new(simulation))
}

fn BuildBridge(handle: &MockBusHandle, arcSimulation: Arc<Mutex<SimulationService>>) -> CanBridge {
    CanBridge::New(
        Box::new(handle.Bus()),
        arcSimulation,
        IsoTpParameters::default(),
    )
}

fn Frame(u32CanId: u32, vecData: Vec<u8>) -> CanFrame {
    CanFrame::NewClassic(0.0, u32CanId, vecData)
}

/// The data of every frame the engine put on the bus, paired with its identifier.
fn Sent(handle: &MockBusHandle) -> Vec<(u32, Vec<u8>)> {
    handle
        .TakeTransmittedFrames()
        .into_iter()
        .map(|frame| (frame.m_u32CanId, frame.m_vecData))
        .collect()
}

#[tokio::test]
async fn a_single_frame_request_is_answered_with_a_single_frame() {
    let handle = MockBusHandle::default();
    let mut bridge = BuildBridge(&handle, BuildSimulation());

    handle.InjectFrame(Frame(
        0x7E0,
        vec![0x02, 0x10, 0x03, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA],
    ));
    bridge.PumpOnce(&UdsHandler).await;

    // 00 32 is P2Server_max 50 ms; 01 F4 is 500 units of 10 ms = P2*Server_max 5000 ms.
    assert_eq!(
        Sent(&handle),
        vec![(
            0x7E8,
            vec![
                0x06,
                0x50,
                0x03,
                0x00,
                0x32,
                0x01,
                0xF4,
                c_byDefaultPaddingByte
            ]
        )]
    );
}

#[tokio::test]
async fn a_long_response_waits_for_the_testers_flow_control() {
    let handle = MockBusHandle::default();
    let mut bridge = BuildBridge(&handle, BuildSimulation());

    // The tester asks for the VIN, then clears the engine to send.
    handle.InjectFrame(Frame(
        0x7E0,
        vec![0x03, 0x22, 0xF1, 0x90, 0xAA, 0xAA, 0xAA, 0xAA],
    ));
    handle.InjectFrame(Frame(
        0x7E0,
        vec![0x30, 0x00, 0x00, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA],
    ));
    bridge.PumpOnce(&UdsHandler).await;

    let vecSent = Sent(&handle);
    assert_eq!(vecSent.len(), 3);
    // 62 F1 90 plus a 17-character VIN is 20 bytes: 0x014.
    assert_eq!(
        vecSent[0],
        (0x7E8, vec![0x10, 0x14, 0x62, 0xF1, 0x90, b'1', b'H', b'G'])
    );
    assert_eq!(
        vecSent[1],
        (0x7E8, vec![0x21, b'C', b'M', b'8', b'2', b'6', b'3', b'3'])
    );
    assert_eq!(
        vecSent[2],
        (0x7E8, vec![0x22, b'A', b'0', b'0', b'4', b'3', b'5', b'2'])
    );
}

#[tokio::test]
async fn a_multi_frame_request_is_flow_controlled_on_the_response_identifier() {
    let handle = MockBusHandle::default();
    let mut bridge = BuildBridge(&handle, BuildSimulation());

    // A 10-byte write: 2E F1 90 then "1HGCM82".
    handle.InjectFrame(Frame(
        0x7E0,
        vec![0x10, 0x0A, 0x2E, 0xF1, 0x90, b'1', b'H', b'G'],
    ));
    bridge.PumpOnce(&UdsHandler).await;

    let vecFlowControl = Sent(&handle);
    assert_eq!(vecFlowControl.len(), 1);
    // On 0x7E8, not 0x7E0: that is where the tester's transmitter is listening.
    assert_eq!(vecFlowControl[0].0, 0x7E8);
    assert_eq!(&vecFlowControl[0].1[0..3], &[0x30, 0x00, 0x00]);

    // The trailing pad bytes must not become part of the request.
    handle.InjectFrame(Frame(
        0x7E0,
        vec![0x21, b'C', b'M', b'8', b'2', 0xAA, 0xAA, 0xAA],
    ));
    bridge.PumpOnce(&UdsHandler).await;

    let vecAnswer = Sent(&handle);
    assert_eq!(vecAnswer.len(), 1);
    // 0x2E is not implemented by the plugin, so the ECU refuses it — which is the right answer
    // and proves the reassembled request reached the ECU intact. Byte 0 is the ISO-TP length.
    assert_eq!(&vecAnswer[0].1[0..4], &[0x03, 0x7F, 0x2E, 0x11]);
}

#[tokio::test]
async fn a_response_pending_goes_out_before_the_answer() {
    let handle = MockBusHandle::default();
    let arcSimulation = BuildSimulation();
    arcSimulation
        .lock()
        .unwrap()
        .SetEcuTiming(
            simulation::EcuKey::Can(0x7E0),
            EcuTiming {
                m_u32ResponseDelayMs: 60,
                ..EcuTiming::default()
            },
        )
        .expect("ECU on 0x7E0");

    let mut bridge = BuildBridge(&handle, arcSimulation);

    handle.InjectFrame(Frame(
        0x7E0,
        vec![0x02, 0x3E, 0x00, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA],
    ));
    bridge.PumpOnce(&UdsHandler).await;

    let vecSent = Sent(&handle);
    assert_eq!(vecSent.len(), 2);
    // A ResponsePending is always a single frame, and echoes the service it defers.
    assert_eq!(
        vecSent[0].1[0..4],
        [0x03, 0x7F, 0x3E, 0x78],
        "a delay past P2 puts a ResponsePending on the wire first"
    );
    assert_eq!(vecSent[1].1[0..3], [0x02, 0x7E, 0x00]);
}

#[tokio::test]
async fn an_identifier_no_ecu_owns_draws_nothing() {
    let handle = MockBusHandle::default();
    let mut bridge = BuildBridge(&handle, BuildSimulation());

    handle.InjectFrame(Frame(
        0x7E5,
        vec![0x02, 0x10, 0x03, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA],
    ));
    bridge.PumpOnce(&UdsHandler).await;

    assert!(
        Sent(&handle).is_empty(),
        "a tester scanning for ECUs is ordinary traffic, not something to answer"
    );
}

#[tokio::test]
async fn a_stopped_simulation_sends_nothing_at_all_not_even_flow_control() {
    let handle = MockBusHandle::default();
    let arcSimulation = BuildSimulation();
    arcSimulation.lock().unwrap().Stop();

    let mut bridge = BuildBridge(&handle, arcSimulation.clone());

    // A first frame would normally be answered with flow control. An unpowered ECU does not
    // acknowledge a request it will never answer — that would put a half-alive ECU on the wire
    // and badly mislead anyone debugging.
    handle.InjectFrame(Frame(
        0x7E0,
        vec![0x10, 0x0A, 0x2E, 0xF1, 0x90, b'1', b'H', b'G'],
    ));
    bridge.PumpOnce(&UdsHandler).await;
    assert!(Sent(&handle).is_empty());

    // And nothing was buffered, so starting again produces no late flow control.
    arcSimulation.lock().unwrap().Start();
    handle.InjectFrame(Frame(
        0x7E0,
        vec![0x02, 0x3E, 0x00, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA],
    ));
    bridge.PumpOnce(&UdsHandler).await;

    let vecSent = Sent(&handle);
    assert_eq!(vecSent.len(), 1, "only the new request is answered");
    assert_eq!(vecSent[0].1[0..3], [0x02, 0x7E, 0x00]);
}

#[tokio::test]
async fn a_broadcast_reaches_the_ecu_and_is_answered_on_its_own_identifier() {
    let handle = MockBusHandle::default();
    let mut bridge = BuildBridge(&handle, BuildSimulation());

    handle.InjectFrame(Frame(
        0x7DF,
        vec![0x02, 0x3E, 0x00, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA],
    ));
    bridge.PumpOnce(&UdsHandler).await;

    let vecSent = Sent(&handle);
    assert_eq!(vecSent.len(), 1);
    assert_eq!(vecSent[0].0, 0x7E8, "answered on the ECU's own identifier");
    assert_eq!(vecSent[0].1[0..3], [0x02, 0x7E, 0x00]);
}

#[tokio::test]
async fn a_multi_frame_request_on_a_broadcast_is_dropped_without_flow_control() {
    let handle = MockBusHandle::default();
    let mut bridge = BuildBridge(&handle, BuildSimulation());

    // There is no single peer to flow control on a broadcast, and several ECUs answering with
    // one at once would collide.
    handle.InjectFrame(Frame(
        0x7DF,
        vec![0x10, 0x0A, 0x2E, 0xF1, 0x90, b'1', b'H', b'G'],
    ));
    bridge.PumpOnce(&UdsHandler).await;

    assert!(Sent(&handle).is_empty());
}

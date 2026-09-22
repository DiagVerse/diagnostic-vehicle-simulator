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
use simulation::{EcuKey, SimulationService};

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
    // BlockSize 1 is the default: one frame per flow control, which every link can carry.
    assert_eq!(&vecFlowControl[0].1[0..3], &[0x30, 0x01, 0x00]);

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

/// A 20-byte WriteDataByIdentifier, segmented: a FirstFrame carrying six payload bytes and two
/// ConsecutiveFrames of seven. Long enough that a BlockSize of 1 is felt twice.
fn LongWriteRequestFrames() -> (CanFrame, CanFrame, CanFrame) {
    let first = Frame(0x7E0, vec![0x10, 0x14, 0x2E, 0xF1, 0x90, b'1', b'H', b'G']);
    let second = Frame(0x7E0, vec![0x21, b'C', b'M', b'8', b'2', b'6', b'3', b'3']);
    let third = Frame(0x7E0, vec![0x22, b'A', b'0', b'0', b'4', b'3', b'5', b'2']);
    (first, second, third)
}

#[tokio::test]
async fn an_ecus_block_size_and_separation_time_reach_the_wire() {
    // The bug this guards: with BlockSize 0 the tester is told to send every ConsecutiveFrame
    // back to back. Over an SLCAN dongle on a 115200 baud line that is roughly eight times
    // what the link carries, so the middle of a long request is lost and the message is
    // abandoned. A BlockSize of 1 paces the tester to what the link can actually take.
    let arcSimulation = BuildSimulation();
    {
        let mut simulation = arcSimulation.lock().expect("simulation");
        let timing = EcuTiming {
            m_u8IsoTpBlockSize: 1,
            m_byIsoTpSeparationTimeMin: 3,
            ..EcuTiming::default()
        };
        timing
            .Validate()
            .expect("one frame per block, spaced 3 ms, is a legal thing to ask for");
        simulation
            .SetEcuTiming(EcuKey::Can(0x7E0), timing)
            .expect("the ECU");
    }

    let handle = MockBusHandle::default();
    let mut bridge = BuildBridge(&handle, arcSimulation);
    let (first, second, third) = LongWriteRequestFrames();

    handle.InjectFrame(first);
    bridge.PumpOnce(&UdsHandler).await;
    let vecAfterFirst = Sent(&handle);
    assert_eq!(
        vecAfterFirst.len(),
        1,
        "the FirstFrame is owed a flow control"
    );
    assert_eq!(
        &vecAfterFirst[0].1[0..3],
        &[0x30, 0x01, 0x03],
        "the flow control must carry this ECU's BlockSize and STmin, not the link default"
    );

    // The part that matters: one frame per block means another flow control is owed after
    // every single ConsecutiveFrame, which is what stops the tester flooding the link.
    handle.InjectFrame(second);
    bridge.PumpOnce(&UdsHandler).await;
    let vecAfterSecond = Sent(&handle);
    assert_eq!(
        vecAfterSecond.len(),
        1,
        "the block is used up after one frame"
    );
    assert_eq!(&vecAfterSecond[0].1[0..3], &[0x30, 0x01, 0x03]);

    // And the paced message still arrives whole.
    handle.InjectFrame(third);
    bridge.PumpOnce(&UdsHandler).await;
    let vecAnswer = Sent(&handle);
    assert_eq!(vecAnswer.len(), 1);
    assert_eq!(
        &vecAnswer[0].1[0..4],
        &[0x03, 0x7F, 0x2E, 0x11],
        "0x2E is unimplemented, so a refusal here proves the 20 bytes reassembled intact"
    );
}

#[tokio::test]
async fn changing_an_ecus_flow_control_reaches_a_bridge_that_is_already_running() {
    // An operator raising BlockSize is doing it *because* the link is dropping frames. Making
    // them stop and restart the link to apply it would be a poor answer.
    let arcSimulation = BuildSimulation();
    let handle = MockBusHandle::default();
    let mut bridge = BuildBridge(&handle, Arc::clone(&arcSimulation));
    let (first, _, _) = LongWriteRequestFrames();

    handle.InjectFrame(first.clone());
    bridge.PumpOnce(&UdsHandler).await;
    let vecBefore = Sent(&handle);
    assert_eq!(
        &vecBefore[0].1[0..3],
        &[0x30, 0x01, 0x00],
        "the default paces one frame per flow control"
    );

    {
        let mut simulation = arcSimulation.lock().expect("simulation");
        let timing = EcuTiming {
            m_u8IsoTpBlockSize: 4,
            ..EcuTiming::default()
        };
        simulation
            .SetEcuTiming(EcuKey::Can(0x7E0), timing)
            .expect("the ECU");
    }

    handle.InjectFrame(first);
    bridge.PumpOnce(&UdsHandler).await;
    let vecAfter = Sent(&handle);
    assert_eq!(vecAfter.len(), 1);
    assert_eq!(
        &vecAfter[0].1[0..3],
        &[0x30, 0x04, 0x00],
        "the change must reach the wire without the link being restarted"
    );
}

/// Two ECUs, so a transfer for one can be interrupted by traffic for the other.
fn BuildTwoEcuSimulation() -> Arc<Mutex<SimulationService>> {
    let arcSimulation = BuildSimulation();
    {
        let mut simulation = arcSimulation.lock().expect("simulation");
        let mut second = Ecu::New("Transmission", 0);
        second.m_optCanAddress = Some(CanAddress::NewSpecified(
            0x7E1,
            0x7E9,
            CanAddressingMode::Normal11Bit,
        ));
        second.m_vecSupportedServices = vec![0x10, 0x22, 0x2E, 0x3E];
        second.m_vecSupportedSessions = vec![SessionType::Default, SessionType::Extended];
        simulation.AddEcu(second).expect("the second ECU");
    }
    arcSimulation
}

#[tokio::test]
async fn a_second_ecus_frames_survive_the_first_ecus_transfer() {
    // The bug this guards: while one ECU waited for flow control, every frame for every other
    // identifier was thrown away. A multi-frame request to the second ECU lost the middle of
    // itself and reported a consecutive-frame sequence error — on a bus where the tester had
    // sent every frame correctly, and with nothing in any log to say the engine had eaten them.
    let handle = MockBusHandle::default();
    let mut bridge = BuildBridge(&handle, BuildTwoEcuSimulation());

    // Ask the first ECU for the VIN: 20 bytes, so the answer is segmented and waits for flow
    // control that never comes.
    handle.InjectFrame(Frame(
        0x7E0,
        vec![0x03, 0x22, 0xF1, 0x90, 0xAA, 0xAA, 0xAA, 0xAA],
    ));

    // And, arriving while that is in flight, a segmented request to the *second* ECU.
    handle.InjectFrame(Frame(
        0x7E1,
        vec![0x10, 0x14, 0x2E, 0xF1, 0x90, b'1', b'H', b'G'],
    ));
    handle.InjectFrame(Frame(
        0x7E1,
        vec![0x21, b'C', b'M', b'8', b'2', b'6', b'3', b'3'],
    ));
    handle.InjectFrame(Frame(
        0x7E1,
        vec![0x22, b'A', b'0', b'0', b'4', b'3', b'5', b'2'],
    ));

    bridge.PumpOnce(&UdsHandler).await;
    // The first pump ends when the first ECU's flow control times out; the deferred frames go
    // back on the queue, so a second pump deals with them.
    bridge.PumpOnce(&UdsHandler).await;

    let vecSent = Sent(&handle);
    let vecFromSecond: Vec<&(u32, Vec<u8>)> =
        vecSent.iter().filter(|(id, _)| *id == 0x7E9).collect();

    assert!(
        !vecFromSecond.is_empty(),
        "the second ECU must still have been reachable; it sent nothing at all"
    );
    assert!(
        vecFromSecond
            .iter()
            .any(|(_, data)| data[0..4] == [0x03, 0x7F, 0x2E, 0x11]),
        "its 20-byte request must reassemble whole and be answered, not lose frames to the \
         first ECU's transfer: {vecFromSecond:02X?}"
    );
}

/// Segment a PDU into the frames a tester would send for it.
fn SegmentRequest(u32CanId: u32, vecPdu: &[u8]) -> Vec<CanFrame> {
    let mut vecFrames = vec![Frame(
        u32CanId,
        [
            &[
                0x10 | ((vecPdu.len() >> 8) as u8),
                (vecPdu.len() & 0xFF) as u8,
            ],
            &vecPdu[..6],
        ]
        .concat(),
    )];

    let mut uSent = 6;
    let mut u8Sequence = 1u8;
    while uSent < vecPdu.len() {
        let uEnd = (uSent + 7).min(vecPdu.len());
        let mut vecData = vec![0x20 | u8Sequence];
        vecData.extend_from_slice(&vecPdu[uSent..uEnd]);
        while vecData.len() < 8 {
            vecData.push(0xAA);
        }
        vecFrames.push(Frame(u32CanId, vecData));
        uSent = uEnd;
        u8Sequence = (u8Sequence + 1) & 0x0F;
    }
    vecFrames
}

#[tokio::test]
async fn block_size_zero_reassembles_a_long_request_on_a_link_that_keeps_up() {
    // The question this settles: when a 386-byte request arrives with BlockSize 0 and the
    // sequence numbers come out wrong, is the engine mis-ordering them or is the link losing
    // them? The mock bus has no bandwidth limit and drops nothing, so a failure here would be
    // ours and a success puts it on the wire.
    let arcSimulation = BuildSimulation();
    {
        let mut simulation = arcSimulation.lock().expect("simulation");
        let timing = EcuTiming {
            m_u8IsoTpBlockSize: 0,
            ..EcuTiming::default()
        };
        simulation
            .SetEcuTiming(EcuKey::Can(0x7E0), timing)
            .expect("the ECU");
    }

    let handle = MockBusHandle::default();
    let mut bridge = BuildBridge(&handle, arcSimulation);

    // 386 bytes: a SecurityAccess sendKey the size of the one that failed in the field.
    let mut vecPdu = vec![0x27, 0x02];
    vecPdu.extend((0..384).map(|uIndex| (uIndex % 251) as u8));
    assert_eq!(vecPdu.len(), 386);

    let vecFrames = SegmentRequest(0x7E0, &vecPdu);
    assert_eq!(
        vecFrames.len(),
        56,
        "one FirstFrame and 55 ConsecutiveFrames"
    );

    // Every frame at once, which is exactly what BlockSize 0 asks a tester to do.
    handle.InjectFrames(vecFrames);
    bridge.PumpOnce(&UdsHandler).await;

    let vecSent = Sent(&handle);
    assert_eq!(
        &vecSent[0].1[0..3],
        &[0x30, 0x00, 0x00],
        "BlockSize 0 must be what goes out"
    );
    assert!(
        vecSent.len() >= 2,
        "the request must be answered, not abandoned: {vecSent:02X?}"
    );
    // The ECU does not declare 0x27, so it refuses the service — and that refusal is the
    // proof: it could only be produced by a request that reassembled whole and reached the
    // ECU. A lost frame abandons the message and sends nothing at all.
    assert_eq!(
        &vecSent[1].1[0..4],
        &[0x03, 0x7F, 0x27, 0x11],
        "the whole 386-byte request must have reassembled"
    );
}

#[tokio::test]
async fn a_link_that_cannot_carry_the_bus_is_not_promised_unpaced_delivery() {
    // BlockSize 0 is honest on a fast link and a broken promise on a slow one. 115200 baud
    // carries about 426 SLCAN lines a second; a 500 kbit/s bus delivers about 3700 frames. An
    // ECU asking for no pacing at all over that gap loses the middle of every long request, so
    // the bridge advertises the slowest separation time the link *can* honour instead.
    let arcSimulation = BuildSimulation();
    {
        let mut simulation = arcSimulation.lock().expect("simulation");
        let timing = EcuTiming {
            m_u8IsoTpBlockSize: 0,
            m_byIsoTpSeparationTimeMin: 0,
            ..EcuTiming::default()
        };
        simulation
            .SetEcuTiming(EcuKey::Can(0x7E0), timing)
            .expect("the ECU");
    }

    let handle = MockBusHandle::default();
    let mut bridge = BuildBridge(&handle, arcSimulation).WithLinkCapacity(115_200, 500_000);

    handle.InjectFrame(Frame(
        0x7E0,
        vec![0x10, 0x14, 0x2E, 0xF1, 0x90, b'1', b'H', b'G'],
    ));
    bridge.PumpOnce(&UdsHandler).await;

    let vecSent = Sent(&handle);
    assert_eq!(vecSent[0].1[0], 0x30, "a flow control is still owed");
    assert_eq!(
        vecSent[0].1[1], 0x00,
        "the BlockSize the operator chose is kept"
    );
    assert!(
        vecSent[0].1[2] >= 3,
        "an STmin the link can honour must be advertised, got {:02X}",
        vecSent[0].1[2]
    );
}

#[tokio::test]
async fn a_link_with_room_to_spare_is_left_alone() {
    // The clamp must not fire where it is not needed, or a fast link is slowed for nothing.
    let capacity = bridge::LinkCapacity::New(1_000_000, 500_000);
    assert_eq!(
        capacity.SafeSeparationTime(),
        None,
        "1 Mbaud carries {} frames/s against the bus's {}",
        capacity.m_uLinkFramesPerSecond,
        capacity.m_uBusFramesPerSecond
    );

    // And an operator who stated a rate is obeyed, not second-guessed.
    let arcSimulation = BuildSimulation();
    {
        let mut simulation = arcSimulation.lock().expect("simulation");
        let timing = EcuTiming {
            m_u8IsoTpBlockSize: 4,
            ..EcuTiming::default()
        };
        simulation
            .SetEcuTiming(EcuKey::Can(0x7E0), timing)
            .expect("the ECU");
    }

    let handle = MockBusHandle::default();
    let mut bridge = BuildBridge(&handle, arcSimulation).WithLinkCapacity(115_200, 500_000);
    handle.InjectFrame(Frame(
        0x7E0,
        vec![0x10, 0x14, 0x2E, 0xF1, 0x90, b'1', b'H', b'G'],
    ));
    bridge.PumpOnce(&UdsHandler).await;

    let vecSent = Sent(&handle);
    assert_eq!(&vecSent[0].1[0..3], &[0x30, 0x04, 0x00]);
}

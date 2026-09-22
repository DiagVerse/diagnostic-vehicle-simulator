//! A whole DoIP session over real sockets: discover, activate, diagnose.
//!
//! The codec and the state machine are tested exhaustively in `doip`. These prove the two
//! halves are wired together and that a tester holding an actual TCP connection gets the bytes
//! the standard says it should.

#![allow(non_snake_case, non_upper_case_globals)]

use std::sync::{Arc, Mutex};

use abi_stable::std_types::RVec;
use application::ProtocolHandler;
use doip::header::{self, HeaderLimits, ReadHeader};
use doip::messages::DiagnosticMessage;
use doip::payload::PayloadType;
use doip_server::{DoIpEntity, DoIpServer};
use plugin_contract::protocol::{REcuSnapshot, RProtocolOutcome};
use simulation::SimulationService;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const c_u16Tester: u16 = 0x0E80;
const c_u16Gateway: u16 = 0x0010;
const c_u16Airbag: u16 = 0x1030;

const c_strSimFile: &str = r#"{
  "simfileVersion": 2,
  "vehicle": "DoIP test vehicle",
  "identity": {
    "vin": "SIMDOIPVIN0000001",
    "eid": "02 00 00 00 00 01",
    "gid": "02 00 00 00 00 02"
  },
  "networks": [
    { "id": "eth", "name": "Diagnostic Ethernet", "kind": "Ethernet", "entryPoint": true }
  ],
  "ecus": [
    { "name": "Gateway", "network": "eth",
      "doip": { "logicalAddress": "0x0010" },
      "can": { "request": "0x7E7", "response": "0x7EF" },
      "sessions": ["default", "extended"],
      "dids": { "F18C": { "text": "SIM-GW-0001" } } },
    { "name": "Airbag", "network": "eth",
      "doip": { "logicalAddress": "0x1030" },
      "sessions": ["default", "extended"],
      "dids": { "F190": { "text": "SIMDOIPVIN0000001" } } }
  ]
}"#;

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

/// One decoded DoIP message read off a socket.
struct Received {
    m_payloadType: PayloadType,
    m_vecPayload: Vec<u8>,
}

/// Read exactly one complete DoIP message.
async fn ReadMessage(stream: &mut TcpStream) -> Received {
    let mut arrHeader = [0u8; 8];
    stream
        .read_exact(&mut arrHeader)
        .await
        .expect("a header should arrive");

    let header = ReadHeader(&arrHeader, HeaderLimits::default()).expect("a valid header");
    let mut vecPayload = vec![0u8; header.m_u32PayloadLength as usize];
    if !vecPayload.is_empty() {
        stream
            .read_exact(&mut vecPayload)
            .await
            .expect("the payload should arrive");
    }

    Received {
        m_payloadType: header.m_payloadType,
        m_vecPayload: vecPayload,
    }
}

/// Start a server over the test vehicle and connect a tester to it.
async fn ConnectTester() -> (doip_server::ServerHandle, TcpStream) {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromSimFileText(c_strSimFile)
        .expect("the test simfile should load");
    simulation.Start();

    let arcSimulation = Arc::new(Mutex::new(simulation));
    let arcEntity = Arc::new(Mutex::new(DoIpEntity::New(
        arcSimulation,
        c_u16Gateway,
        Arc::new(Mutex::new(doip_server::DoIpSettings::default())),
    )));

    // Port 0: the OS picks a free one, so the tests do not fight each other or whatever else is
    // on 13400 on this machine.
    let handle = DoIpServer::Start(
        arcEntity,
        "127.0.0.1:0".parse().expect("a valid address"),
        Arc::new(UdsHandler),
    )
    .await
    .expect("the server should bind");

    let stream = TcpStream::connect(handle.TcpAddress())
        .await
        .expect("the tester should connect");
    (handle, stream)
}

/// Send a routing activation request and return its response code.
async fn ActivateRouting(stream: &mut TcpStream, u16SourceAddress: u16, byType: u8) -> u8 {
    let mut vecPayload = u16SourceAddress.to_be_bytes().to_vec();
    vecPayload.push(byType);
    vecPayload.extend_from_slice(&0u32.to_be_bytes());

    let vecMessage = header::WriteMessage(0x03, PayloadType::RoutingActivationRequest, &vecPayload);
    stream.write_all(&vecMessage).await.expect("write");

    let received = ReadMessage(stream).await;
    assert_eq!(
        received.m_payloadType,
        PayloadType::RoutingActivationResponse
    );
    received.m_vecPayload[4]
}

/// Send a diagnostic message.
async fn SendDiagnostic(stream: &mut TcpStream, u16Target: u16, vecUserData: &[u8]) {
    let message = DiagnosticMessage {
        m_u16SourceAddress: c_u16Tester,
        m_u16TargetAddress: u16Target,
        m_vecUserData: vecUserData.to_vec(),
    };
    let vecMessage = header::WriteMessage(0x03, PayloadType::DiagnosticMessage, &message.ToBytes());
    stream.write_all(&vecMessage).await.expect("write");
}

#[tokio::test]
async fn a_tester_activates_routing_and_diagnoses_an_ecu() {
    let (handle, mut stream) = ConnectTester().await;

    assert_eq!(
        ActivateRouting(&mut stream, c_u16Tester, 0x00).await,
        0x10,
        "routing successfully activated"
    );

    SendDiagnostic(&mut stream, c_u16Airbag, &[0x22, 0xF1, 0x90]).await;

    // The acknowledgement comes FIRST — it means "routed", not "answered".
    let ack = ReadMessage(&mut stream).await;
    assert_eq!(ack.m_payloadType, PayloadType::DiagnosticMessageAck);
    assert_eq!(
        &ack.m_vecPayload[0..2],
        &c_u16Airbag.to_be_bytes(),
        "the acknowledgement is FROM the ECU that was addressed"
    );
    assert_eq!(
        &ack.m_vecPayload[2..4],
        &c_u16Tester.to_be_bytes(),
        "and TO the tester"
    );
    assert_eq!(ack.m_vecPayload[4], 0x00);

    // Then the UDS answer, as its own diagnostic message.
    let answer = ReadMessage(&mut stream).await;
    assert_eq!(answer.m_payloadType, PayloadType::DiagnosticMessage);
    assert_eq!(&answer.m_vecPayload[0..2], &c_u16Airbag.to_be_bytes());
    assert_eq!(&answer.m_vecPayload[2..4], &c_u16Tester.to_be_bytes());
    assert_eq!(&answer.m_vecPayload[4..7], &[0x62, 0xF1, 0x90]);
    assert_eq!(&answer.m_vecPayload[7..], b"SIMDOIPVIN0000001");

    handle.Stop();
}

#[tokio::test]
async fn a_diagnostic_message_before_routing_activation_is_ignored_entirely() {
    // REQ 3.DoIP-131 NL: nothing is answered *or routed* before routing is active — and it is
    // not negatively acknowledged either. The socket dies to the initial inactivity timer.
    let (handle, mut stream) = ConnectTester().await;

    SendDiagnostic(&mut stream, c_u16Airbag, &[0x22, 0xF1, 0x90]).await;

    // Nothing comes back. A short read timeout is the only way to assert silence.
    let mut arrBuffer = [0u8; 8];
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        stream.read(&mut arrBuffer),
    )
    .await;
    assert!(result.is_err(), "the entity should have stayed silent");

    handle.Stop();
}

#[tokio::test]
async fn an_unknown_target_address_is_negatively_acknowledged() {
    let (handle, mut stream) = ConnectTester().await;
    ActivateRouting(&mut stream, c_u16Tester, 0x00).await;

    SendDiagnostic(&mut stream, 0x9999, &[0x22, 0xF1, 0x90]).await;

    let nack = ReadMessage(&mut stream).await;
    assert_eq!(nack.m_payloadType, PayloadType::DiagnosticMessageNack);
    assert_eq!(nack.m_vecPayload[4], 0x03, "unknown target address");

    handle.Stop();
}

#[tokio::test]
async fn a_source_address_that_is_not_the_activated_one_closes_the_connection() {
    // The single diagnostic rejection that resets the connection (REQ 7.DoIP-070 AL).
    let (handle, mut stream) = ConnectTester().await;
    ActivateRouting(&mut stream, c_u16Tester, 0x00).await;

    let message = DiagnosticMessage {
        m_u16SourceAddress: 0x0E99,
        m_u16TargetAddress: c_u16Airbag,
        m_vecUserData: vec![0x22, 0xF1, 0x90],
    };
    let vecMessage = header::WriteMessage(0x03, PayloadType::DiagnosticMessage, &message.ToBytes());
    stream.write_all(&vecMessage).await.expect("write");

    let nack = ReadMessage(&mut stream).await;
    assert_eq!(nack.m_payloadType, PayloadType::DiagnosticMessageNack);
    assert_eq!(nack.m_vecPayload[4], 0x02, "invalid source address");

    // And the socket is closed, which a read of zero bytes is how a peer observes.
    let mut arrBuffer = [0u8; 8];
    let uRead = stream.read(&mut arrBuffer).await.expect("read");
    assert_eq!(uRead, 0, "the entity should have closed the connection");

    handle.Stop();
}

#[tokio::test]
async fn an_unsupported_activation_type_is_refused_and_the_socket_closed() {
    let (handle, mut stream) = ConnectTester().await;

    assert_eq!(
        ActivateRouting(&mut stream, c_u16Tester, 0xE0).await,
        0x06,
        "unsupported routing activation type"
    );

    let mut arrBuffer = [0u8; 8];
    assert_eq!(stream.read(&mut arrBuffer).await.expect("read"), 0);

    handle.Stop();
}

#[tokio::test]
async fn a_tester_address_outside_the_reserved_range_is_refused() {
    let (handle, mut stream) = ConnectTester().await;

    assert_eq!(
        ActivateRouting(&mut stream, 0x1234, 0x00).await,
        0x00,
        "unknown source address"
    );

    handle.Stop();
}

#[tokio::test]
async fn re_activating_the_same_socket_with_the_same_address_is_accepted() {
    let (handle, mut stream) = ConnectTester().await;

    assert_eq!(ActivateRouting(&mut stream, c_u16Tester, 0x00).await, 0x10);
    assert_eq!(
        ActivateRouting(&mut stream, c_u16Tester, 0x00).await,
        0x10,
        "the same tester re-activating its own socket is legal"
    );

    handle.Stop();
}

#[tokio::test]
async fn two_messages_in_one_segment_are_both_handled() {
    // TCP is a stream; the header's length field is the only framing there is. Assuming one
    // message per segment is a listed trap, and a tester that pipelines will find it.
    let (handle, mut stream) = ConnectTester().await;
    ActivateRouting(&mut stream, c_u16Tester, 0x00).await;

    let mut vecBoth = Vec::new();
    for _ in 0..2 {
        let message = DiagnosticMessage {
            m_u16SourceAddress: c_u16Tester,
            m_u16TargetAddress: c_u16Airbag,
            m_vecUserData: vec![0x22, 0xF1, 0x90],
        };
        vecBoth.extend_from_slice(&header::WriteMessage(
            0x03,
            PayloadType::DiagnosticMessage,
            &message.ToBytes(),
        ));
    }
    stream.write_all(&vecBoth).await.expect("write");

    // Two acknowledgements and two answers, in order.
    for _ in 0..2 {
        assert_eq!(
            ReadMessage(&mut stream).await.m_payloadType,
            PayloadType::DiagnosticMessageAck
        );
        assert_eq!(
            ReadMessage(&mut stream).await.m_payloadType,
            PayloadType::DiagnosticMessage
        );
    }

    handle.Stop();
}

#[tokio::test]
async fn an_unknown_payload_type_is_refused_without_closing_the_socket() {
    let (handle, mut stream) = ConnectTester().await;

    let vecMessage = vec![0x03, 0xFC, 0x12, 0x34, 0x00, 0x00, 0x00, 0x00];
    stream.write_all(&vecMessage).await.expect("write");

    let nack = ReadMessage(&mut stream).await;
    assert_eq!(nack.m_payloadType, PayloadType::GenericHeaderNack);
    assert_eq!(nack.m_vecPayload[0], 0x01);

    // Still usable: 0x01 discards the message, not the connection.
    assert_eq!(ActivateRouting(&mut stream, c_u16Tester, 0x00).await, 0x10);

    handle.Stop();
}

// ==========================================================================================
// The entity's settings, and the faults they inject.
// ==========================================================================================

use doip_server::DoIpSettings;

/// Start a server whose settings this test can change while it runs.
async fn ConnectWithSettings() -> (
    doip_server::ServerHandle,
    TcpStream,
    Arc<Mutex<DoIpSettings>>,
) {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromSimFileText(c_strSimFile)
        .expect("the test simfile should load");
    simulation.Start();

    let arcSettings = Arc::new(Mutex::new(DoIpSettings::default()));
    let arcEntity = Arc::new(Mutex::new(DoIpEntity::New(
        Arc::new(Mutex::new(simulation)),
        c_u16Gateway,
        Arc::clone(&arcSettings),
    )));

    let handle = DoIpServer::Start(
        arcEntity,
        "127.0.0.1:0".parse().expect("a valid address"),
        Arc::new(UdsHandler),
    )
    .await
    .expect("the server should bind");

    let stream = TcpStream::connect(handle.TcpAddress())
        .await
        .expect("the tester should connect");
    (handle, stream, arcSettings)
}

#[tokio::test]
async fn a_forced_routing_activation_code_is_what_the_tester_gets() {
    let (handle, mut stream, arcSettings) = ConnectWithSettings().await;

    arcSettings
        .lock()
        .expect("settings mutex")
        .m_optByForcedRoutingActivationCode = Some(0x06);

    assert_eq!(
        ActivateRouting(&mut stream, c_u16Tester, 0x00).await,
        0x06,
        "a request that would normally succeed is refused"
    );

    handle.Stop();
}

#[tokio::test]
async fn a_forced_diagnostic_nack_replaces_the_answer() {
    // Injected before anything is routed, so the tester sees the refusal a real entity would
    // give rather than an answer followed by one.
    let (handle, mut stream, arcSettings) = ConnectWithSettings().await;
    ActivateRouting(&mut stream, c_u16Tester, 0x00).await;

    arcSettings
        .lock()
        .expect("settings mutex")
        .m_optByForcedDiagnosticNack = Some(0x03);

    SendDiagnostic(&mut stream, c_u16Airbag, &[0x22, 0xF1, 0x90]).await;

    let received = ReadMessage(&mut stream).await;
    assert_eq!(received.m_payloadType, PayloadType::DiagnosticMessageNack);
    assert_eq!(received.m_vecPayload[4], 0x03);

    handle.Stop();
}

#[tokio::test]
async fn a_forced_header_nack_refuses_a_message_before_it_is_dispatched() {
    let (handle, mut stream, arcSettings) = ConnectWithSettings().await;

    arcSettings
        .lock()
        .expect("settings mutex")
        .m_optByForcedHeaderNack = Some(0x01);

    let vecPayload = c_u16Tester.to_be_bytes().to_vec();
    stream
        .write_all(&header::WriteMessage(
            0x03,
            PayloadType::AliveCheckResponse,
            &vecPayload,
        ))
        .await
        .expect("write");

    let received = ReadMessage(&mut stream).await;
    assert_eq!(received.m_payloadType, PayloadType::GenericHeaderNack);
    assert_eq!(received.m_vecPayload[0], 0x01);

    handle.Stop();
}

#[tokio::test]
async fn a_settings_change_takes_effect_without_restarting_the_entity() {
    // The reason settings are shared rather than owned: being able to inject a fault only by
    // restarting would make it useless for reproducing one mid-session, which is when a fault
    // actually matters.
    let (handle, mut stream, arcSettings) = ConnectWithSettings().await;

    assert_eq!(ActivateRouting(&mut stream, c_u16Tester, 0x00).await, 0x10);

    arcSettings
        .lock()
        .expect("settings mutex")
        .m_optByForcedRoutingActivationCode = Some(0x00);

    // The same socket, the same tester, mid-session.
    assert_eq!(ActivateRouting(&mut stream, c_u16Tester, 0x00).await, 0x00);

    handle.Stop();
}

#[tokio::test]
async fn default_settings_inject_nothing() {
    // An entity nobody has configured must behave exactly as it did before settings existed.
    let settings = DoIpSettings::default();
    assert!(!settings.IsInjectingFaults());
    assert_eq!(settings.m_byPowerMode, 0x01);
    assert_eq!(settings.m_byNodeType, 0x00);
}

/// A vehicle whose airbag answers slowly: 400 ms of configured delay against a 50 ms P2.
///
/// Both halves of the timing story in one ECU — the delay is long enough to measure, and long
/// enough past P2 that the ECU must announce a ResponsePending before it.
const c_strSlowSimFile: &str = r#"{
  "simfileVersion": 2,
  "vehicle": "Slow DoIP test vehicle",
  "networks": [
    { "id": "eth", "name": "Diagnostic Ethernet", "kind": "Ethernet", "entryPoint": true }
  ],
  "ecus": [
    { "name": "Gateway", "network": "eth",
      "doip": { "logicalAddress": "0x0010" },
      "sessions": ["default", "extended"],
      "dids": { "F18C": { "text": "SIM-GW-0001" } } },
    { "name": "Airbag", "network": "eth",
      "doip": { "logicalAddress": "0x1030" },
      "timing": { "p2Ms": 50, "responseDelayMs": 400 },
      "sessions": ["default", "extended"],
      "dids": { "F190": { "text": "SIMDOIPVIN0000001" } } }
  ]
}"#;

/// Start a server over a simfile of the caller's choosing, with settings it can change.
async fn ConnectToSimFile(
    strSimFile: &str,
) -> (
    doip_server::ServerHandle,
    TcpStream,
    Arc<Mutex<DoIpSettings>>,
) {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromSimFileText(strSimFile)
        .expect("the test simfile should load");
    simulation.Start();

    let arcSettings = Arc::new(Mutex::new(DoIpSettings::default()));
    let arcEntity = Arc::new(Mutex::new(DoIpEntity::New(
        Arc::new(Mutex::new(simulation)),
        c_u16Gateway,
        Arc::clone(&arcSettings),
    )));

    let handle = DoIpServer::Start(
        arcEntity,
        "127.0.0.1:0".parse().expect("a valid address"),
        Arc::new(UdsHandler),
    )
    .await
    .expect("the server should bind");

    let stream = TcpStream::connect(handle.TcpAddress())
        .await
        .expect("the tester should connect");
    (handle, stream, arcSettings)
}

#[tokio::test]
async fn an_ecus_response_timing_is_kept_over_doip() {
    // The defect: the entity flattened an ECU's response plan into one burst, throwing away
    // every step's offset. A configured response delay and a forced ResponsePending are only
    // observable if the messages actually arrive apart — sent together they tell a tester
    // nothing it can measure, and hide exactly the P2/P2* behaviour those knobs exist to
    // provoke.
    let (handle, mut stream, _arcSettings) = ConnectToSimFile(c_strSlowSimFile).await;
    assert_eq!(ActivateRouting(&mut stream, c_u16Tester, 0x00).await, 0x10);

    let instantSent = std::time::Instant::now();
    SendDiagnostic(&mut stream, c_u16Airbag, &[0x22, 0xF1, 0x90]).await;

    // The acknowledgement means "routed", and is not part of the ECU's plan — it is immediate.
    let ack = ReadMessage(&mut stream).await;
    assert_eq!(ack.m_payloadType, PayloadType::DiagnosticMessageAck);
    assert!(
        instantSent.elapsed() < std::time::Duration::from_millis(200),
        "the acknowledgement does not wait for the ECU, it took {:?}",
        instantSent.elapsed()
    );

    // 400 ms is past the 50 ms P2, so the ECU says so before it goes quiet.
    let pending = ReadMessage(&mut stream).await;
    assert_eq!(pending.m_payloadType, PayloadType::DiagnosticMessage);
    assert_eq!(
        &pending.m_vecPayload[4..7],
        &[0x7F, 0x22, 0x78],
        "requestCorrectlyReceived-ResponsePending"
    );

    let answer = ReadMessage(&mut stream).await;
    assert_eq!(&answer.m_vecPayload[4..7], &[0x62, 0xF1, 0x90]);

    let durationToAnswer = instantSent.elapsed();
    assert!(
        durationToAnswer >= std::time::Duration::from_millis(350),
        "the answer waited out the configured delay, it took {durationToAnswer:?}"
    );

    handle.Stop();
}

#[tokio::test]
async fn the_advertised_socket_count_is_the_one_that_is_enforced() {
    // The defect: capacity was a constant of four regardless of what the entity advertised, so
    // an operator who lowered it to one in order to provoke routing activation code 0x04
    // watched the second tester be accepted anyway.
    const c_u16SecondTester: u16 = 0x0E81;

    let (handle, mut first, arcSettings) = ConnectToSimFile(c_strSimFile).await;
    arcSettings.lock().expect("settings mutex").m_byMaxSockets = 1;

    assert_eq!(
        ActivateRouting(&mut first, c_u16Tester, 0x00).await,
        0x10,
        "the one socket this entity advertises"
    );

    let mut second = TcpStream::connect(handle.TcpAddress())
        .await
        .expect("a second tester connects; the standard requires the spare resource for it");

    assert_eq!(
        ActivateRouting(&mut second, c_u16SecondTester, 0x00).await,
        0x01,
        "Table 48 code 0x01: all concurrently supported TCP_DATA sockets are registered \
         and active"
    );

    handle.Stop();
}

#[tokio::test]
async fn raising_the_socket_count_lets_a_second_tester_in() {
    // The other half of the same property: the number is read when the decision is made, so a
    // setting changed on a running entity takes effect without restarting it.
    const c_u16SecondTester: u16 = 0x0E81;

    let (handle, mut first, arcSettings) = ConnectToSimFile(c_strSimFile).await;
    arcSettings.lock().expect("settings mutex").m_byMaxSockets = 2;

    assert_eq!(ActivateRouting(&mut first, c_u16Tester, 0x00).await, 0x10);

    let mut second = TcpStream::connect(handle.TcpAddress())
        .await
        .expect("the tester connects");
    assert_eq!(
        ActivateRouting(&mut second, c_u16SecondTester, 0x00).await,
        0x10,
        "there is room for both"
    );

    handle.Stop();
}

/// An entity over the test vehicle, with no sockets — the state machine driven directly, which
/// is the only way to hold a clock still and step it.
fn LoneEntity() -> DoIpEntity {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromSimFileText(c_strSimFile)
        .expect("the test simfile should load");
    simulation.Start();

    DoIpEntity::New(
        Arc::new(Mutex::new(simulation)),
        c_u16Gateway,
        Arc::new(Mutex::new(DoIpSettings::default())),
    )
}

/// The bytes of a routing activation request.
fn RoutingActivationBytes(u16SourceAddress: u16) -> Vec<u8> {
    let mut vecPayload = u16SourceAddress.to_be_bytes().to_vec();
    vecPayload.push(0x00);
    vecPayload.extend_from_slice(&0u32.to_be_bytes());
    header::WriteMessage(0x03, PayloadType::RoutingActivationRequest, &vecPayload)
}

#[tokio::test]
async fn data_the_entity_cannot_parse_still_keeps_an_activated_socket_alive() {
    // REQ 3.DoIP-080 NL resets the general inactivity timer "whenever data is received or sent
    // over this socket" — not whenever a *valid* message is. The defect: the timer was noted
    // only after the header parsed, so a tester sending a steady stream of rubbish was closed
    // as idle while the entity was busy negatively acknowledging every message of it.
    const c_u64Socket: u64 = 1;
    let mut entity = LoneEntity();
    entity.OpenConnection(c_u64Socket);

    let reaction = entity.HandleTcp(
        c_u64Socket,
        &RoutingActivationBytes(c_u16Tester),
        &UdsHandler,
    );
    assert!(!reaction.m_bCloseSocket, "routing activated");

    assert!(
        entity.Tick(290_000).is_empty(),
        "not yet at the five-minute general timer"
    );

    // A broken synchronisation pattern: data the entity answers but cannot interpret.
    let reaction = entity.HandleTcp(c_u64Socket, &[0xAA; 8], &UdsHandler);
    assert_eq!(
        reaction.m_vecReplies.len(),
        1,
        "answered with a generic header NACK"
    );

    assert!(
        entity.Tick(290_000).is_empty(),
        "and that traffic reset the timer, so 580 seconds of it does not close the socket"
    );
}

#[tokio::test]
async fn a_vehicle_identification_response_waits_its_announce_delay() {
    // REQ 8.DoIP-051 APP. The answer is correct but must not be instant: every entity on the
    // network answers the same broadcast, and replying in the same instant is what makes the
    // burst that drops the answers on the way back to the tester.
    let entity = LoneEntity();
    let vecRequest = header::WriteMessage(0x03, PayloadType::VehicleIdentificationRequest, &[]);

    let reaction = entity.HandleUdp(&vecRequest);
    assert_eq!(reaction.m_vecReplies.len(), 1);

    let reply = &reaction.m_vecReplies[0];
    let header = ReadHeader(&reply.m_vecBytes, HeaderLimits::default()).expect("a valid header");
    assert_eq!(header.m_payloadType, PayloadType::VehicleAnnouncement);
    assert!(
        reply.m_u32AtMs <= 500,
        "A_DoIP_Announce_Wait is 500 ms at most, got {}",
        reply.m_u32AtMs
    );
}

#[tokio::test]
async fn the_announce_delay_is_drawn_fresh_rather_than_fixed() {
    // The randomisation is the whole point: a fixed delay moves the burst instead of spreading
    // it. Two entities that always wait the same 500 ms collide exactly as two that wait none.
    let entity = LoneEntity();
    let vecRequest = header::WriteMessage(0x03, PayloadType::VehicleIdentificationRequest, &[]);

    let mut setDelays = std::collections::BTreeSet::new();
    for _ in 0..50 {
        let reaction = entity.HandleUdp(&vecRequest);
        setDelays.insert(reaction.m_vecReplies[0].m_u32AtMs);
    }

    assert!(
        setDelays.len() > 1,
        "fifty draws produced only the single delay {setDelays:?}"
    );
}

/// A vehicle with one ECU the entity can address and one it can only reach over CAN.
const c_strMixedSimFile: &str = r#"{
  "simfileVersion": 2,
  "vehicle": "Mixed transport DoIP test vehicle",
  "networks": [
    { "id": "eth", "name": "Diagnostic Ethernet", "kind": "Ethernet", "entryPoint": true },
    { "id": "pt", "name": "Powertrain CAN", "kind": "CAN" }
  ],
  "ecus": [
    { "name": "Gateway", "network": "eth", "gatewayFor": ["pt"],
      "doip": { "logicalAddress": "0x0010" },
      "sessions": ["default", "extended"],
      "dids": { "F18C": { "text": "SIM-GW-0001" } } },
    { "name": "Engine", "network": "pt",
      "can": { "request": "0x7E0", "response": "0x7E8", "functional": "0x7DF" },
      "sessions": ["default", "extended"],
      "dids": { "F18C": { "text": "SIM-ENG-0001" } } }
  ]
}"#;

#[tokio::test]
async fn a_broadcast_answer_from_an_ecu_with_no_logical_address_is_dropped() {
    // An ECU behind the gateway on CAN has no DoIP logical address, so there is nothing
    // truthful to put in a diagnostic message's source address field for it. Falling back to
    // the group address would tell the tester 0xE000 answered — which no ECU did, and which
    // makes the answer indistinguishable from every other ECU's.
    //
    // The request still reaches it and its state still changes; what is lost is the answer,
    // exactly as on a gateway with no route back for it.
    let (handle, mut stream, _arcSettings) = ConnectToSimFile(c_strMixedSimFile).await;
    assert_eq!(ActivateRouting(&mut stream, c_u16Tester, 0x00).await, 0x10);

    SendDiagnostic(&mut stream, 0xE000, &[0x3E, 0x00]).await;

    let ack = ReadMessage(&mut stream).await;
    assert_eq!(ack.m_payloadType, PayloadType::DiagnosticMessageAck);

    let answer = ReadMessage(&mut stream).await;
    assert_eq!(
        &answer.m_vecPayload[0..2],
        &c_u16Gateway.to_be_bytes(),
        "the gateway answers under its own address"
    );

    // And nothing else arrives: the CAN-only ECU's answer has no address to travel under.
    let mut arrSpare = [0u8; 64];
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(400),
        stream.read(&mut arrSpare),
    )
    .await;
    assert!(
        result.is_err(),
        "a second answer arrived: {:?}",
        result.map(|inner| inner.map(|uRead| arrSpare[..uRead].to_vec()))
    );

    handle.Stop();
}

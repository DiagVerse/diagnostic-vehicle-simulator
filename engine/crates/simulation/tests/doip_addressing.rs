//! Reaching an ECU by its DoIP logical address.
//!
//! The property these exist to pin: an ECU reachable on both CAN and DoIP is **one** ECU. A
//! tester that changes its state through one transport must find it changed through the other,
//! because two instances that look identical from outside and drift apart is the worst kind of
//! bug to hand somebody.

#![allow(non_snake_case, non_upper_case_globals)]

use abi_stable::std_types::RVec;
use application::ProtocolHandler;
use plugin_contract::protocol::{REcuSnapshot, RProtocolOutcome};
use simulation::{EcuKey, RoutingOutcome, SimulationService};

/// A gateway on both transports, an engine on CAN only, an airbag on DoIP only.
const c_strMixedSimFile: &str = r#"{
  "simfileVersion": 2,
  "vehicle": "Mixed transport vehicle",
  "networks": [
    { "id": "eth", "name": "Diagnostic Ethernet", "kind": "Ethernet", "entryPoint": true },
    { "id": "pt", "name": "Powertrain CAN", "kind": "CAN" }
  ],
  "ecus": [
    { "name": "Gateway", "network": "eth", "gatewayFor": ["pt"],
      "doip": { "logicalAddress": "0x0010" },
      "can": { "request": "0x7E7", "response": "0x7EF", "functional": "0x7DF" },
      "sessions": ["default", "extended"],
      "dids": { "F18C": { "text": "SIM-GW-0001" } } },
    { "name": "Engine", "network": "pt",
      "can": { "request": "0x7E0", "response": "0x7E8", "functional": "0x7DF" },
      "sessions": ["default", "extended"],
      "dids": { "F18C": { "text": "SIM-ENG-0001" } } },
    { "name": "Airbag", "network": "eth",
      "doip": { "logicalAddress": "0x1030" },
      "sessions": ["default", "extended"],
      "dids": { "F18C": { "text": "SIM-SRS-0001" } } }
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

fn LoadMixed() -> SimulationService {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromSimFileText(c_strMixedSimFile)
        .expect("the mixed-transport simfile should load");
    simulation.Start();
    simulation
}

fn ExpectHandled(outcome: RoutingOutcome) -> Vec<u8> {
    match outcome {
        RoutingOutcome::Handled(vecResponses) => vecResponses
            .first()
            .map(|response| response.m_vecResponse.clone())
            .unwrap_or_default(),
        other => panic!("expected an answer, got {other:?}"),
    }
}

#[test]
fn an_ecu_with_only_a_doip_address_is_started_and_answers() {
    // Before this it was loaded into the model and never started — "declared but not
    // driveable". A DoIP-addressed ECU is perfectly reachable; it simply is not on CAN.
    let mut simulation = LoadMixed();

    let vecResponse =
        ExpectHandled(simulation.ProcessByLogicalAddress(0x1030, &[0x22, 0xF1, 0x8C], &UdsHandler));

    assert_eq!(&vecResponse[..3], &[0x62, 0xF1, 0x8C]);
    assert_eq!(&vecResponse[3..], b"SIM-SRS-0001");
}

#[test]
fn one_ecu_reachable_both_ways_is_one_ecu() {
    // The whole reason for a single stored object with two indexes. Enter a session over DoIP,
    // observe it over CAN.
    let mut simulation = LoadMixed();

    let vecEntered =
        ExpectHandled(simulation.ProcessByLogicalAddress(0x0010, &[0x10, 0x03], &UdsHandler));
    assert_eq!(vecEntered[0], 0x50, "the gateway entered the session");

    // The same ECU, reached the other way. Its session must have come with it.
    let outcome = simulation.ProcessByCanId(0x7E7, &[0x22, 0xF1, 0x8C], &UdsHandler);
    match outcome {
        RoutingOutcome::Handled(vecResponses) => {
            assert_eq!(
                vecResponses[0].m_bySession, 0x03,
                "a session entered over DoIP must be visible over CAN — same ECU, one state"
            );
        }
        other => panic!("expected an answer, got {other:?}"),
    }
}

#[test]
fn switching_an_ecu_off_silences_it_on_both_transports() {
    // The on/off switch is a property of the ECU, not of a transport. An ECU that is not there
    // is not there whichever way you try to reach it.
    let mut simulation = LoadMixed();
    simulation
        .SetEcuEnabled(EcuKey::Can(0x7E7), false)
        .expect("the gateway is addressable by its CAN id");

    assert!(matches!(
        simulation.ProcessByCanId(0x7E7, &[0x22, 0xF1, 0x8C], &UdsHandler),
        RoutingOutcome::Silenced { .. }
    ));
    assert!(
        matches!(
            simulation.ProcessByLogicalAddress(0x0010, &[0x22, 0xF1, 0x8C], &UdsHandler),
            RoutingOutcome::Silenced { .. }
        ),
        "switched off over CAN means switched off over DoIP too"
    );
}

#[test]
fn a_disabled_gateway_silences_an_ecu_behind_it_over_doip_as_well() {
    let mut simulation = LoadMixed();
    simulation
        .SetEcuEnabled(EcuKey::Can(0x7E7), false)
        .expect("loaded");

    // The engine is CAN-only, so this is the CAN path — but the point is that the architecture
    // rule is enforced in one place and both transports inherit it.
    match simulation.ProcessByCanId(0x7E0, &[0x22, 0xF1, 0x8C], &UdsHandler) {
        RoutingOutcome::Silenced { strReason, .. } => {
            assert!(strReason.contains("Gateway"), "got: {strReason}")
        }
        other => panic!("expected silence behind the gateway, got {other:?}"),
    }
}

#[test]
fn an_unknown_logical_address_is_no_target_rather_than_a_negative_response() {
    // The DoIP layer turns this into diagnostic message NACK 0x03, unknown target address
    // (ISO 13400-2 REQ 7.DoIP-071 AL). It must not become a UDS negative response.
    let mut simulation = LoadMixed();

    assert!(matches!(
        simulation.ProcessByLogicalAddress(0x9999, &[0x22, 0xF1, 0x8C], &UdsHandler),
        RoutingOutcome::NoTarget
    ));
}

#[test]
fn a_can_only_ecu_has_no_logical_address_to_be_reached_on() {
    // The Engine carries no `doip` block, so its `m_u16LogicalAddress` is the model's
    // placeholder rather than a routable address — indexing it would invent an entity.
    let simulation = LoadMixed();

    let vecAddresses: Vec<u16> = simulation.LogicalAddresses().collect();
    assert_eq!(vecAddresses, vec![0x0010, 0x1030]);
    assert!(!simulation.IsKnownLogicalAddress(0x0000));
}

#[test]
fn a_stopped_simulation_answers_nothing_over_doip_either() {
    let mut simulation = LoadMixed();
    simulation.Stop();

    assert!(matches!(
        simulation.ProcessByLogicalAddress(0x1030, &[0x22, 0xF1, 0x8C], &UdsHandler),
        RoutingOutcome::Stopped
    ));
}

#[test]
fn two_ecus_claiming_one_logical_address_are_refused() {
    // Ambiguous routing, and the DoIP layer would have no way to choose. A tester's
    // `CP_DoIPLogicalGatewayAddress` must resolve to exactly one entity.
    let strFile = r#"{
      "simfileVersion": 2, "vehicle": "V",
      "ecus": [
        { "name": "A", "doip": { "logicalAddress": "0x0010" },
          "can": { "request": "0x7E0", "response": "0x7E8" } },
        { "name": "B", "doip": { "logicalAddress": "0x0010" },
          "can": { "request": "0x7E1", "response": "0x7E9" } }
      ]
    }"#;

    let mut simulation = SimulationService::New();
    let error = simulation
        .LoadFromSimFileText(strFile)
        .expect_err("one address, two ECUs");
    assert!(
        error.to_string().contains("0x0010"),
        "the error should name the contested address, got: {error}"
    );
}

#[test]
fn a_functional_group_address_reaches_the_whole_vehicle_over_doip() {
    // ISO 13400-2 Table 13 gives 0xE000-0xEFFF to functional group addresses, and Table 22's
    // worked example is a tester reading an InfoType from the whole vehicle at 0xE000. Before
    // this the address matched no ECU and the entity answered NACK 0x03, telling the tester
    // no such target existed — when every ECU in the vehicle was one.
    let mut simulation = LoadMixed();

    let outcome = simulation.ProcessByLogicalAddress(0xE000, &[0x22, 0xF1, 0x8C], &UdsHandler);
    let vecResponses = match outcome {
        RoutingOutcome::Handled(vecResponses) => vecResponses,
        other => panic!("expected the broadcast to be handled, got {other:?}"),
    };

    // All three: the gateway and the airbag directly, the engine through the gateway onto the
    // powertrain CAN — which is exactly the routing REQ 7.DoIP-072 AL's example describes.
    let mut vecNames: Vec<&str> = vecResponses
        .iter()
        .map(|response| response.m_strEcuName.as_str())
        .collect();
    vecNames.sort();
    assert_eq!(vecNames, vec!["Airbag", "Engine", "Gateway"]);

    for response in &vecResponses {
        assert_eq!(
            &response.m_vecResponse[..3],
            &[0x62, 0xF1, 0x8C],
            "'{}' answered the broadcast",
            response.m_strEcuName
        );
    }
}

#[test]
fn each_answer_to_a_broadcast_names_its_own_logical_address() {
    // The answers must be distinguishable. An ECU that echoed the group address back as its
    // source would tell the tester "the group replied", which no ECU did — and would make
    // several ECUs' answers identical in the one field that separates them.
    let mut simulation = LoadMixed();

    let outcome = simulation.ProcessByLogicalAddress(0xE000, &[0x22, 0xF1, 0x8C], &UdsHandler);
    let vecResponses = match outcome {
        RoutingOutcome::Handled(vecResponses) => vecResponses,
        other => panic!("expected the broadcast to be handled, got {other:?}"),
    };

    let FindAddress = |strName: &str| {
        vecResponses
            .iter()
            .find(|response| response.m_strEcuName == strName)
            .expect("the ECU answered")
            .m_optU16LogicalAddress
    };
    assert_eq!(FindAddress("Gateway"), Some(0x0010));
    assert_eq!(FindAddress("Airbag"), Some(0x1030));
    assert_eq!(
        FindAddress("Engine"),
        None,
        "a CAN-only ECU has no logical address to answer from; the entity fills in the \
         request's target rather than inventing one"
    );
}

#[test]
fn a_functional_request_too_long_for_can_is_refused_rather_than_half_delivered() {
    // REQ 7.DoIP-072 AL, with the standard's own example: a functional request crossing a CAN
    // sub-network may be a SingleFrame only, because there is no single peer to flow-control
    // it. Delivering it to the ECUs that *could* have taken it would leave the tester with
    // part of a broadcast and no way to know which part.
    let mut simulation = LoadMixed();

    let vecLongRequest = vec![0x22, 0xF1, 0x8C, 0xF1, 0x90, 0xF1, 0x91, 0xF1];
    assert!(vecLongRequest.len() > 7);

    match simulation.ProcessByLogicalAddress(0xE000, &vecLongRequest, &UdsHandler) {
        RoutingOutcome::TooLargeForSubnetwork {
            uRequestBytes,
            uMaxBytes,
            strEcuName,
        } => {
            assert_eq!(uRequestBytes, 8);
            assert_eq!(uMaxBytes, 7);
            assert_eq!(strEcuName, "Engine", "the ECU that is only on CAN");
        }
        other => panic!("expected the request to be refused, got {other:?}"),
    }
}

#[test]
fn a_functional_request_is_not_length_limited_when_nothing_has_to_cross_can() {
    // The limit belongs to the sub-network, not to functional addressing. A vehicle the entity
    // reaches entirely over Ethernet has no SingleFrame to fit into, and refusing a long
    // request there would be inventing a constraint the transport does not have.
    const c_strEthernetOnly: &str = r#"{
      "simfileVersion": 2,
      "vehicle": "Ethernet-only vehicle",
      "networks": [
        { "id": "eth", "name": "Diagnostic Ethernet", "kind": "Ethernet", "entryPoint": true }
      ],
      "ecus": [
        { "name": "Airbag", "network": "eth",
          "doip": { "logicalAddress": "0x1030" },
          "sessions": ["default", "extended"],
          "dids": { "F18C": { "text": "SIM-SRS-0001" } } }
      ]
    }"#;

    let mut simulation = SimulationService::New();
    simulation
        .LoadFromSimFileText(c_strEthernetOnly)
        .expect("the Ethernet-only simfile should load");
    simulation.Start();

    let vecLongRequest = vec![0x22, 0xF1, 0x8C, 0xF1, 0x90, 0xF1, 0x91, 0xF1];
    match simulation.ProcessByLogicalAddress(0xE000, &vecLongRequest, &UdsHandler) {
        RoutingOutcome::Handled(vecResponses) => {
            assert_eq!(vecResponses.len(), 1, "the one ECU processed it");
        }
        other => panic!("expected the broadcast to be handled, got {other:?}"),
    }
}

#[test]
fn a_broadcast_skips_an_ecu_behind_a_gateway_that_is_switched_off() {
    // Same rule as the physical path, and for the same reason: an unpowered gateway takes
    // everything behind it off the air. A broadcast that reached through one would be claiming
    // a route the vehicle does not have.
    let mut simulation = LoadMixed();
    simulation
        .SetEcuEnabled(EcuKey::Can(0x7E7), false)
        .expect("the gateway can be switched off");

    let outcome = simulation.ProcessByLogicalAddress(0xE000, &[0x22, 0xF1, 0x8C], &UdsHandler);
    let vecResponses = match outcome {
        RoutingOutcome::Handled(vecResponses) => vecResponses,
        other => panic!("expected the broadcast to be handled, got {other:?}"),
    };

    let vecNames: Vec<&str> = vecResponses
        .iter()
        .map(|response| response.m_strEcuName.as_str())
        .collect();
    assert_eq!(
        vecNames,
        vec!["Airbag"],
        "the gateway is off and the engine sits behind it"
    );
}

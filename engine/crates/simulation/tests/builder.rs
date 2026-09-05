//! Building a vehicle by hand: create an empty vehicle, add ECUs one at a time, rename them,
//! remove them — and have the result route exactly like a reconstructed one.

#![allow(non_snake_case, non_upper_case_globals)]

use abi_stable::std_types::RVec;
use application::ProtocolHandler;
use core_domain::model::{CanAddress, CanAddressingMode, DataIdentifier, Ecu, SessionType};
use core_domain::Confidence;
use plugin_contract::protocol::{REcuSnapshot, RProtocolOutcome};
use simulation::{RoutingOutcome, SimulationService};

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

/// A hand-built ECU: named, addressed, answering ReadDataByIdentifier for one DID.
fn BuildEcu(strName: &str, u32RequestCanId: u32, u32ResponseCanId: u32) -> Ecu {
    let mut config = Ecu::New(strName, 0);
    config.m_optCanAddress = Some(CanAddress::NewSpecified(
        u32RequestCanId,
        u32ResponseCanId,
        CanAddressingMode::Normal11Bit,
    ));
    config.m_vecSupportedServices = vec![0x10, 0x22, 0x3E];
    config.m_vecSupportedSessions = vec![SessionType::Default, SessionType::Extended];
    config.m_mapDids.insert(
        0xF190,
        DataIdentifier {
            m_u16Id: 0xF190,
            m_vecValue: b"1HGCM82633A004352".to_vec(),
            m_confidence: Confidence::Confirmed,
        },
    );
    config
}

#[test]
fn a_hand_built_vehicle_answers_exactly_like_a_reconstructed_one() {
    let mut simulation = SimulationService::New();
    simulation.CreateEmptyVehicle("Bench vehicle");

    assert!(simulation.IsLoaded(), "an empty vehicle is still a vehicle");
    assert_eq!(simulation.RunningEcus().count(), 0);

    simulation
        .AddEcu(BuildEcu("Engine", 0x7E0, 0x7E8))
        .expect("the ECU should be accepted");

    let outcome = simulation.ProcessByCanId(0x7E0, &[0x22, 0xF1, 0x90], &UdsHandler);
    match outcome {
        RoutingOutcome::Handled(vecResponses) => {
            assert_eq!(vecResponses.len(), 1);
            assert_eq!(vecResponses[0].m_u32ResponseCanId, 0x7E8);
            assert_eq!(&vecResponses[0].m_vecResponse[3..], b"1HGCM82633A004352");
        }
        RoutingOutcome::NoTarget => panic!("the ECU just added should be routable"),
    }
}

#[test]
fn a_second_ecu_on_the_same_identifiers_is_refused() {
    let mut simulation = SimulationService::New();
    simulation.CreateEmptyVehicle("Bench vehicle");
    simulation
        .AddEcu(BuildEcu("Engine", 0x7E0, 0x7E8))
        .expect("the first ECU");

    // Same request identifier: routing could not decide between them.
    assert!(simulation.AddEcu(BuildEcu("Clash", 0x7E0, 0x7E9)).is_err());
    // Same response identifier: their answers would be indistinguishable on the wire.
    assert!(simulation.AddEcu(BuildEcu("Clash", 0x7E1, 0x7E8)).is_err());

    // Neither was started.
    assert_eq!(simulation.RunningEcus().count(), 1);
}

#[test]
fn an_ecu_without_a_can_address_is_refused() {
    let mut simulation = SimulationService::New();
    simulation.CreateEmptyVehicle("Bench vehicle");

    let config = Ecu::New("Unreachable", 0x1234);
    assert!(simulation.AddEcu(config).is_err());
}

#[test]
fn adding_an_ecu_before_there_is_a_vehicle_is_refused() {
    let mut simulation = SimulationService::New();
    assert!(simulation.AddEcu(BuildEcu("Engine", 0x7E0, 0x7E8)).is_err());
}

#[test]
fn a_removed_ecu_stops_answering_and_leaves_the_model() {
    let mut simulation = SimulationService::New();
    simulation.CreateEmptyVehicle("Bench vehicle");
    simulation.AddEcu(BuildEcu("Engine", 0x7E0, 0x7E8)).unwrap();
    simulation.AddEcu(BuildEcu("Body", 0x7E1, 0x7E9)).unwrap();

    simulation.RemoveEcu(0x7E0).expect("ECU on 0x7E0");

    assert_eq!(simulation.RunningEcus().count(), 1);
    assert_eq!(
        simulation.ProcessByCanId(0x7E0, &[0x3E, 0x00], &UdsHandler),
        RoutingOutcome::NoTarget
    );
    assert_eq!(simulation.Vehicle().unwrap().m_vecEcus.len(), 1);

    // The broadcast now reaches only what is left.
    match simulation.ProcessByCanId(0x7DF, &[0x3E, 0x00], &UdsHandler) {
        RoutingOutcome::Handled(vecResponses) => assert_eq!(vecResponses.len(), 1),
        RoutingOutcome::NoTarget => panic!("0x7DF should still reach the remaining ECU"),
    }
}

#[test]
fn removing_an_ecu_that_is_not_there_is_refused() {
    let mut simulation = SimulationService::New();
    simulation.CreateEmptyVehicle("Bench vehicle");
    assert!(simulation.RemoveEcu(0x7E0).is_err());
}

#[test]
fn renaming_an_ecu_updates_both_the_running_ecu_and_the_model() {
    let mut simulation = SimulationService::New();
    simulation.CreateEmptyVehicle("Bench vehicle");
    simulation
        .AddEcu(BuildEcu("ECU_7E8", 0x7E0, 0x7E8))
        .unwrap();

    simulation.RenameEcu(0x7E0, "Engine").expect("ECU on 0x7E0");

    assert_eq!(
        simulation
            .FindEcuByRequestCanId(0x7E0)
            .unwrap()
            .Config()
            .m_strName,
        "Engine"
    );
    // The model is what gets serialized, so it must not drift from what is running.
    assert_eq!(
        simulation.Vehicle().unwrap().m_vecEcus[0].m_strName,
        "Engine"
    );
}

#[test]
fn a_hand_stated_address_is_confirmed_rather_than_observed() {
    // Nothing was seen on a bus, but nothing was guessed either: the identifiers came from
    // someone who knows the vehicle.
    let address = CanAddress::NewSpecified(0x7E0, 0x7E8, CanAddressingMode::Normal11Bit);
    assert_eq!(address.m_confidence, Confidence::Confirmed);
    assert_eq!(address.m_optU32FunctionalCanId, Some(0x7DF));
}

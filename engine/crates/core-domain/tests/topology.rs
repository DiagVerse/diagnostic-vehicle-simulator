//! How a tester reaches each ECU: gateways, depth, and wiring that could not exist.

#![allow(non_snake_case, non_upper_case_globals)]

use core_domain::model::{
    CanAddress, CanAddressingMode, Ecu, Network, NetworkKind, TopologyError, Vehicle,
};
use core_domain::Confidence;

/// A CAN-addressed ECU on a named network.
fn BuildEcu(strName: &str, u32RequestCanId: u32, strNetworkId: &str) -> Ecu {
    let mut ecu = Ecu::New(strName, 0);
    ecu.m_optCanAddress = Some(CanAddress::NewSpecified(
        u32RequestCanId,
        u32RequestCanId + 8,
        CanAddressingMode::Normal11Bit,
    ));
    ecu.m_optStrNetworkId = Some(strNetworkId.to_string());
    ecu
}

fn BuildNetwork(strId: &str, bIsEntryPoint: bool) -> Network {
    Network {
        m_strId: strId.to_string(),
        m_strName: strId.to_string(),
        m_kind: NetworkKind::CanClassic,
        m_optU32BitrateBps: None,
        m_optU32DataBitrateBps: None,
        m_bIsDiagnosticEntryPoint: bIsEntryPoint,
        m_confidence: Confidence::Confirmed,
    }
}

/// Tester → backbone → gateway → powertrain CAN → engine.
fn BuildGatewayedVehicle() -> Vehicle {
    let mut gateway = BuildEcu("Gateway", 0x7E0, "backbone");
    gateway.m_bHasDoIpAddress = true;
    gateway.m_u16LogicalAddress = 0x0010;
    gateway.m_vecGatewayForNetworkIds = vec!["powertrain".to_string()];

    Vehicle {
        m_strName: "Test vehicle".to_string(),
        m_vecEcus: vec![gateway, BuildEcu("Engine", 0x7E1, "powertrain")],
        m_vecNetworks: vec![
            BuildNetwork("backbone", true),
            BuildNetwork("powertrain", false),
        ],
        m_identity: Default::default(),
    }
}

#[test]
fn ecu_behind_a_gateway_is_one_hop_deep() {
    let vehicle = BuildGatewayedVehicle();
    vehicle.ValidateTopology().expect("this wiring is sound");

    let engine = &vehicle.m_vecEcus[1];
    let path = vehicle.DiagnosticPathTo(engine);

    assert!(path.m_bIsReachable);
    assert_eq!(path.m_uHopCount, 1);
    assert_eq!(path.m_vecGatewayEcuNames, vec!["Gateway".to_string()]);

    let depths = vehicle.NetworkDepths();
    assert_eq!(depths.get("backbone"), Some(&0));
    assert_eq!(depths.get("powertrain"), Some(&1));
}

#[test]
fn ecu_on_the_entry_point_link_crosses_no_gateway() {
    let vehicle = BuildGatewayedVehicle();
    let path = vehicle.DiagnosticPathTo(&vehicle.m_vecEcus[0]);

    assert!(path.m_bIsReachable);
    assert_eq!(path.m_uHopCount, 0);
    assert!(path.m_vecGatewayEcuNames.is_empty());
}

/// Two gateways deep, to prove the walk is not hard-coded to one hop.
#[test]
fn a_gateway_behind_a_gateway_is_two_hops_deep() {
    let mut vehicle = BuildGatewayedVehicle();

    let mut subGateway = BuildEcu("Sub-gateway", 0x7E2, "powertrain");
    subGateway.m_vecGatewayForNetworkIds = vec!["chassis".to_string()];
    vehicle.m_vecEcus.push(subGateway);
    vehicle.m_vecEcus.push(BuildEcu("ABS", 0x7E3, "chassis"));
    vehicle.m_vecNetworks.push(BuildNetwork("chassis", false));

    vehicle.ValidateTopology().expect("this wiring is sound");

    let abs = vehicle
        .m_vecEcus
        .iter()
        .find(|ecu| ecu.m_strName == "ABS")
        .expect("just added");
    let path = vehicle.DiagnosticPathTo(abs);

    assert_eq!(path.m_uHopCount, 2);
    // Nearest the tester first, so a reader follows the request rather than the response.
    assert_eq!(
        path.m_vecGatewayEcuNames,
        vec!["Gateway".to_string(), "Sub-gateway".to_string()]
    );
}

#[test]
fn an_ecu_on_a_network_nothing_gateways_onto_is_unreachable() {
    let mut vehicle = BuildGatewayedVehicle();
    vehicle.m_vecNetworks.push(BuildNetwork("orphan", false));
    vehicle
        .m_vecEcus
        .push(BuildEcu("Stranded", 0x7E4, "orphan"));

    // Sound wiring — it is simply not connected to anything, which is a real thing to model.
    vehicle
        .ValidateTopology()
        .expect("nothing here is contradictory");

    let stranded = &vehicle.m_vecEcus[2];
    let path = vehicle.DiagnosticPathTo(stranded);
    assert!(!path.m_bIsReachable);
    assert!(!vehicle.NetworkDepths().contains_key("orphan"));
}

#[test]
fn an_ecu_on_no_declared_network_is_treated_as_directly_reachable() {
    // Every log-reconstructed ECU is in this state: a capture cannot observe bus membership.
    // Treating "nobody said" as "unreachable" would render every reconstruction as broken.
    let mut vehicle = BuildGatewayedVehicle();
    let mut unplaced = BuildEcu("Unplaced", 0x7E5, "backbone");
    unplaced.m_optStrNetworkId = None;
    vehicle.m_vecEcus.push(unplaced);

    let path = vehicle.DiagnosticPathTo(&vehicle.m_vecEcus[2]);
    assert!(path.m_bIsReachable);
    assert_eq!(path.m_uHopCount, 0);
}

#[test]
fn a_gateway_onto_its_own_network_is_refused() {
    let mut vehicle = BuildGatewayedVehicle();
    vehicle.m_vecEcus[0].m_vecGatewayForNetworkIds = vec!["backbone".to_string()];

    let error = vehicle
        .ValidateTopology()
        .expect_err("a loop of length one");
    assert!(matches!(error, TopologyError::GatewayOntoOwnNetwork { .. }));
}

#[test]
fn two_gateways_onto_one_network_are_refused() {
    let mut vehicle = BuildGatewayedVehicle();
    let mut rival = BuildEcu("Rival gateway", 0x7E6, "backbone");
    rival.m_vecGatewayForNetworkIds = vec!["powertrain".to_string()];
    vehicle.m_vecEcus.push(rival);

    let error = vehicle
        .ValidateTopology()
        .expect_err("two paths to one network are ambiguous");
    assert!(matches!(error, TopologyError::NetworkHasTwoGateways { .. }));
}

#[test]
fn a_gateway_loop_is_refused() {
    let mut vehicle = BuildGatewayedVehicle();
    // The engine gateways back onto the backbone the gateway itself sits on.
    vehicle.m_vecEcus[1].m_vecGatewayForNetworkIds = vec!["backbone".to_string()];
    vehicle.m_vecNetworks[0].m_bIsDiagnosticEntryPoint = false;

    let error = vehicle.ValidateTopology().expect_err("this is a loop");
    assert!(matches!(error, TopologyError::GatewayCycle { .. }));
}

#[test]
fn a_gateway_onto_an_undeclared_network_is_refused() {
    let mut vehicle = BuildGatewayedVehicle();
    vehicle.m_vecEcus[0].m_vecGatewayForNetworkIds = vec!["no-such-bus".to_string()];

    let error = vehicle
        .ValidateTopology()
        .expect_err("nothing defines that bus");
    assert!(matches!(error, TopologyError::UnknownNetwork { .. }));
}

#[test]
fn entry_points_default_to_the_links_nothing_gateways_onto() {
    let mut vehicle = BuildGatewayedVehicle();
    for network in &mut vehicle.m_vecNetworks {
        network.m_bIsDiagnosticEntryPoint = false;
    }

    vehicle.NormalizeEntryPoints();

    assert!(
        vehicle.m_vecNetworks[0].m_bIsDiagnosticEntryPoint,
        "backbone"
    );
    assert!(
        !vehicle.m_vecNetworks[1].m_bIsDiagnosticEntryPoint,
        "powertrain is behind the gateway, so it is not where a tester attaches"
    );
}

#[test]
fn an_explicit_entry_point_choice_is_left_alone() {
    let mut vehicle = BuildGatewayedVehicle();
    // Someone deliberately said the tester attaches behind the gateway — bench-testing one
    // segment directly is a real thing to do, and the guess must not overrule it.
    vehicle.m_vecNetworks[0].m_bIsDiagnosticEntryPoint = false;
    vehicle.m_vecNetworks[1].m_bIsDiagnosticEntryPoint = true;

    vehicle.NormalizeEntryPoints();

    assert!(!vehicle.m_vecNetworks[0].m_bIsDiagnosticEntryPoint);
    assert!(vehicle.m_vecNetworks[1].m_bIsDiagnosticEntryPoint);
}

// ==========================================================================================
// Switching ECUs off. Wiring and power are different questions: the path still exists, but
// nothing is forwarding along it.
// ==========================================================================================

#[test]
fn a_disabled_gateway_names_itself_on_the_path_of_everything_behind_it() {
    let mut vehicle = BuildGatewayedVehicle();
    vehicle.m_vecEcus[0].m_bIsEnabled = false;

    let path = vehicle.DiagnosticPathTo(&vehicle.m_vecEcus[1]);

    // The route is unchanged — it is wired exactly as it was. What changed is that nothing is
    // forwarding along it, and those are different facts to report.
    assert!(path.m_bIsReachable);
    assert_eq!(path.m_uHopCount, 1);
    assert_eq!(
        path.m_optStrDisabledGatewayName,
        Some("Gateway".to_string())
    );
}

#[test]
fn an_enabled_path_names_no_blocking_gateway() {
    let vehicle = BuildGatewayedVehicle();
    let path = vehicle.DiagnosticPathTo(&vehicle.m_vecEcus[1]);
    assert_eq!(path.m_optStrDisabledGatewayName, None);
}

#[test]
fn the_gateway_nearest_the_tester_is_the_one_that_blocks() {
    // Two gateways off at once: the request never gets past the first, so that is the one to
    // name. Reporting the deeper one would send someone to fix the wrong ECU.
    let mut vehicle = BuildGatewayedVehicle();

    let mut subGateway = BuildEcu("Sub-gateway", 0x7E2, "powertrain");
    subGateway.m_vecGatewayForNetworkIds = vec!["chassis".to_string()];
    subGateway.m_bIsEnabled = false;
    vehicle.m_vecEcus.push(subGateway);
    vehicle.m_vecEcus.push(BuildEcu("ABS", 0x7E3, "chassis"));
    vehicle.m_vecNetworks.push(BuildNetwork("chassis", false));
    vehicle.m_vecEcus[0].m_bIsEnabled = false;

    let abs = vehicle
        .m_vecEcus
        .iter()
        .find(|ecu| ecu.m_strName == "ABS")
        .expect("just added");

    assert_eq!(
        vehicle.DiagnosticPathTo(abs).m_optStrDisabledGatewayName,
        Some("Gateway".to_string())
    );
}

#[test]
fn switching_off_an_ecu_does_not_block_its_neighbours() {
    // Only a *gateway* takes anything else off the air. An ordinary ECU going quiet is its
    // own business.
    let mut vehicle = BuildGatewayedVehicle();
    vehicle
        .m_vecEcus
        .push(BuildEcu("Transmission", 0x7E2, "powertrain"));
    vehicle.m_vecEcus[1].m_bIsEnabled = false;

    let transmission = &vehicle.m_vecEcus[2];
    assert_eq!(
        vehicle
            .DiagnosticPathTo(transmission)
            .m_optStrDisabledGatewayName,
        None
    );
}

#[test]
fn an_ecu_is_switched_on_when_nothing_says_otherwise() {
    // The serde default matters: `#[serde(default)]` on a bool is `false`, which would load
    // every model written before this field existed with every ECU switched off.
    let ecu = Ecu::New("Fresh", 0);
    assert!(ecu.m_bIsEnabled);

    let strJson = serde_json::to_string(&ecu).expect("an ECU serializes");
    let strWithoutField = strJson.replace(r#""mBIsEnabled":true,"#, "");
    assert_ne!(
        strJson, strWithoutField,
        "the field name must match the model"
    );

    let restored: Ecu = serde_json::from_str(&strWithoutField).expect("an older model loads");
    assert!(restored.m_bIsEnabled, "an absent field must mean 'on'");
}

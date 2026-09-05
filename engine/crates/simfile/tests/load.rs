//! Loading the shipped sample, and refusing files that describe something impossible.

#![allow(non_snake_case, non_upper_case_globals)]

use core_domain::model::{CanAddressingMode, NetworkKind, OverrideAction, SessionType};
use core_domain::Confidence;
use simfile::{LoadFromText, SimFileError};

const c_strSample: &str = include_str!("../../../../samples/demo-vehicle.simfile.json");

#[test]
fn the_shipped_sample_describes_the_vehicle_it_claims_to() {
    let vehicle = LoadFromText(c_strSample).expect("the sample should load");

    assert_eq!(vehicle.m_strName, "Demo vehicle");
    assert_eq!(vehicle.m_vecEcus.len(), 4);
    assert_eq!(vehicle.m_vecNetworks.len(), 2);

    let powertrain = vehicle
        .FindNetwork("powertrain")
        .expect("the powertrain bus");
    assert_eq!(powertrain.m_strName, "Powertrain CAN");
    assert_eq!(powertrain.m_kind, NetworkKind::CanClassic);
    assert_eq!(powertrain.m_optU32BitrateBps, Some(500_000));
    // Stated by the author, not observed on a bus and not guessed.
    assert_eq!(powertrain.m_confidence, Confidence::Confirmed);

    let engine = &vehicle.m_vecEcus[0];
    assert_eq!(engine.m_strName, "Engine");
    assert_eq!(engine.m_optStrNetworkId.as_deref(), Some("powertrain"));

    let address = engine.m_optCanAddress.expect("an addressed ECU");
    assert_eq!(address.m_u32RequestCanId, 0x7E0);
    assert_eq!(address.m_u32ResponseCanId, 0x7E8);
    assert_eq!(address.m_addressingMode, CanAddressingMode::Normal11Bit);
    assert_eq!(address.m_optU32FunctionalCanId, Some(0x7DF));

    // A DID written as text becomes its characters; one written bare is hex.
    assert_eq!(
        engine.FindDid(0xF190).expect("the VIN").m_vecValue,
        b"SIMULATORVIN00001"
    );
    assert_eq!(
        engine.FindDid(0x0100).expect("an OEM DID").m_vecValue,
        vec![0x11]
    );

    // Both codes are ones ISO 14229-1 spells out with their hex, so the sample's encoding can
    // be checked against the standard rather than against our own reasoning.
    assert_eq!(engine.m_vecDtcs[0].m_u32Code, 0x25_221F, "P2522-1F");
    assert_eq!(engine.m_vecDtcs[0].m_byStatus, 0x2F, "activeConfirmed");
    assert_eq!(engine.m_vecDtcs[1].m_u32Code, 0x08_0511, "P0805-11");
    assert_eq!(engine.m_vecDtcs[1].m_byStatus, 0x24, "pendingOnly");

    assert!(engine.IsSessionSupported(SessionType::Programming));
    assert_eq!(
        engine.m_vecSecurityLevels[0].m_vecSeed,
        vec![0x11, 0x22, 0x33, 0x44]
    );

    // The wildcard write override survives with its mask.
    let overrideRule = &engine.m_vecResponseOverrides[0];
    assert_eq!(overrideRule.m_vecRequestMask, vec![0xFF, 0xFF, 0xFF, 0x00]);
    assert!(matches!(
        &overrideRule.m_action,
        OverrideAction::Substitute { m_vecResponse, .. } if m_vecResponse == &vec![0x6E, 0xF1, 0x90]
    ));

    // The 29-bit gateway, and the OEM pair that no convention would derive.
    let gateway = vehicle
        .m_vecEcus
        .iter()
        .find(|ecu| ecu.m_strName == "Gateway")
        .expect("the gateway");
    let gatewayAddress = gateway.m_optCanAddress.unwrap();
    assert_eq!(
        gatewayAddress.m_addressingMode,
        CanAddressingMode::NormalFixed29Bit
    );
    assert_eq!(gatewayAddress.m_optU32FunctionalCanId, Some(0x18DB33F1));

    let bcm = vehicle
        .m_vecEcus
        .iter()
        .find(|ecu| ecu.m_strName == "BCM")
        .expect("the BCM");
    assert_eq!(bcm.m_optCanAddress.unwrap().m_u32RequestCanId, 0x745);
    assert_eq!(bcm.m_optStrNetworkId.as_deref(), Some("body"));
}

/// Build a minimal file with the given ECU body, for the rejection tests.
fn FileWith(strEcus: &str) -> String {
    format!(r#"{{"simfileVersion":1,"vehicle":"V","ecus":[{strEcus}]}}"#)
}

#[test]
fn a_file_from_a_future_version_is_refused_rather_than_half_understood() {
    let strFile = r#"{"simfileVersion":99,"vehicle":"V","ecus":[]}"#;
    assert!(matches!(
        LoadFromText(strFile),
        Err(SimFileError::UnsupportedVersion { uFileVersion: 99 })
    ));
}

#[test]
fn an_unknown_field_is_refused_so_a_typo_is_never_silently_ignored() {
    let strFile = FileWith(
        r#"{"name":"E","requestCanId":"7E0","responseCanId":"7E8","sesions":["default"]}"#,
    );
    assert!(matches!(
        LoadFromText(&strFile),
        Err(SimFileError::Malformed { .. })
    ));
}

#[test]
fn an_ecu_on_a_bus_the_file_never_defines_is_refused() {
    let strFile =
        FileWith(r#"{"name":"E","network":"ghost","requestCanId":"7E0","responseCanId":"7E8"}"#);
    assert!(matches!(
        LoadFromText(&strFile),
        Err(SimFileError::UnknownNetwork { .. })
    ));
}

#[test]
fn two_ecus_sharing_an_identifier_are_refused() {
    let strFile = FileWith(
        r#"{"name":"A","requestCanId":"7E0","responseCanId":"7E8"},
           {"name":"B","requestCanId":"7E1","responseCanId":"7E8"}"#,
    );
    assert!(matches!(
        LoadFromText(&strFile),
        Err(SimFileError::DuplicateCanId { .. })
    ));
}

#[test]
fn an_even_request_seed_is_refused_because_it_is_a_send_key() {
    let strFile = FileWith(
        r#"{"name":"E","requestCanId":"7E0","responseCanId":"7E8",
            "security":[{"requestSeed":"02","seed":"11","key":"22"}]}"#,
    );
    let strError = LoadFromText(&strFile).unwrap_err().to_string();
    assert!(strError.contains("even"), "{strError}");
}

#[test]
fn an_all_zero_seed_is_refused_because_it_means_already_unlocked() {
    let strFile = FileWith(
        r#"{"name":"E","requestCanId":"7E0","responseCanId":"7E8",
            "security":[{"requestSeed":"01","seed":"00 00","key":"22"}]}"#,
    );
    let strError = LoadFromText(&strFile).unwrap_err().to_string();
    assert!(strError.contains("already unlocked"), "{strError}");
}

#[test]
fn a_response_that_does_not_answer_its_request_is_refused() {
    // 0x6E answers 0x2E, not 0x22 — the same rule the API enforces, so a file cannot express
    // an exchange the UI would reject.
    let strFile = FileWith(
        r#"{"name":"E","requestCanId":"7E0","responseCanId":"7E8",
            "responses":[{"request":"22 F1 90","response":"6E F1 90"}]}"#,
    );
    let strError = LoadFromText(&strFile).unwrap_err().to_string();
    assert!(
        strError.contains("neither the positive response"),
        "{strError}"
    );
}

#[test]
fn a_text_value_written_as_bare_hex_says_what_to_do_about_it() {
    let strFile = FileWith(
        r#"{"name":"E","requestCanId":"7E0","responseCanId":"7E8",
            "dids":{"F190":"not hex at all"}}"#,
    );
    let strError = LoadFromText(&strFile).unwrap_err().to_string();
    assert!(strError.contains("\"text\""), "{strError}");
}

#[test]
fn an_ecu_that_requests_and_responds_on_one_identifier_is_refused() {
    let strFile = FileWith(r#"{"name":"E","requestCanId":"7E0","responseCanId":"7E0"}"#);
    let strError = LoadFromText(&strFile).unwrap_err().to_string();
    assert!(strError.contains("same CAN id"), "{strError}");
}

const c_strChassis: &str = include_str!("../../../../samples/chassis-control.simfile.json");

#[test]
fn the_chassis_sample_restricts_what_each_session_allows() {
    let vehicle = LoadFromText(c_strChassis).expect("the chassis sample should load");

    let idm = vehicle
        .m_vecEcus
        .iter()
        .find(|ecu| ecu.m_strName == "IDM")
        .expect("the IDM");

    // SecurityAccess is reachable in extended but not in default — which is how a real ECU
    // keeps unlocking out of the session a tester lands in.
    assert!(!idm.IsServiceAllowedInSession(0x27, 0x01), "default");
    assert!(idm.IsServiceAllowedInSession(0x27, 0x03), "extended");
    // ReadDataByIdentifier works everywhere.
    assert!(idm.IsServiceAllowedInSession(0x22, 0x01));

    // The CDM restricts only its default session, so extended stays unrestricted rather than
    // being locked down by implication.
    let cdm = vehicle
        .m_vecEcus
        .iter()
        .find(|ecu| ecu.m_strName == "CDM")
        .expect("the CDM");
    assert!(!cdm.IsServiceAllowedInSession(0x27, 0x01), "default");
    assert!(
        cdm.IsServiceAllowedInSession(0x27, 0x03),
        "extended is not mentioned"
    );
}

#[test]
fn an_ecu_that_says_nothing_about_sessions_allows_everything_everywhere() {
    let strFile = FileWith(
        r#"{"name":"E","requestCanId":"7E0","responseCanId":"7E8","services":["10","22"]}"#,
    );
    let vehicle = LoadFromText(&strFile).expect("a file with no session rules");
    assert!(vehicle.m_vecEcus[0].IsServiceAllowedInSession(0x22, 0x01));
    assert!(vehicle.m_vecEcus[0].IsServiceAllowedInSession(0x22, 0x03));
}

#[test]
fn restricting_a_session_the_ecu_cannot_enter_is_refused() {
    // Describing behaviour nothing can ever reach is a mistake worth catching at load.
    let strFile = FileWith(
        r#"{"name":"E","requestCanId":"7E0","responseCanId":"7E8",
            "sessions":["default"],
            "sessionServices":{"programming":["10"]}}"#,
    );
    let strError = LoadFromText(&strFile).unwrap_err().to_string();
    assert!(
        strError.contains("not in this ECU's sessions"),
        "{strError}"
    );
}

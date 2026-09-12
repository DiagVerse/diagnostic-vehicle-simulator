//! Writing a loaded vehicle back out as a simulation file.
//!
//! The reverse of [`crate::LoadFromText`], and the reason the two live together: a format whose
//! reader and writer drift apart stops round-tripping, and a file that cannot be re-read is
//! worth very little. Every value written here is one the reader accepts.
//!
//! What this is for: a vehicle reconstructed from a capture and then *worked on* — ECUs
//! renamed, overrides added, a security level given a policy, flow control raised for a slow
//! adapter — exists only in the running engine. Exporting turns that work into a file that can
//! be reloaded, kept, or handed to someone else.
//!
//! Defaults are deliberately left out rather than written. A file that states only what differs
//! from the defaults is one a person can read, and the reader fills the rest in identically.

#![allow(non_snake_case, non_upper_case_globals)]

use std::collections::BTreeMap;

use core_domain::model::{
    CanAddressingMode, Ecu, EcuTiming, Network, NetworkKind, OverrideAction, ResponseOverride,
    SecurityKeyPolicy, SecurityLevel, SessionType, Vehicle, VehicleIdentity,
};

use crate::dto::{
    c_uCurrentVersion, CanAddressDto, DoIpAddressDto, DtcDto, EchoSpanDto, EcuDto, IdentityDto,
    NetworkDto, ResponseDto, SecurityDto, SimFileDto, TimingDto, ValueDto,
};
use crate::encode::FormatDtcCode;

/// Render a vehicle as the JSON text of a simulation file.
///
/// Pretty-printed, because the whole point of this format is that a person can open it.
pub fn WriteSimFileText(vehicle: &Vehicle) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&BuildSimFileDto(vehicle))
}

/// Render a vehicle as the document a simulation file holds.
pub fn BuildSimFileDto(vehicle: &Vehicle) -> SimFileDto {
    SimFileDto {
        simfile_version: c_uCurrentVersion,
        vehicle: vehicle.m_strName.clone(),
        networks: vehicle.m_vecNetworks.iter().map(BuildNetworkDto).collect(),
        ecus: vehicle.m_vecEcus.iter().map(BuildEcuDto).collect(),
        identity: BuildIdentityDto(&vehicle.m_identity),
    }
}

fn BuildNetworkDto(network: &Network) -> NetworkDto {
    NetworkDto {
        id: network.m_strId.clone(),
        name: network.m_strName.clone(),
        kind: match network.m_kind {
            NetworkKind::CanClassic => "can".to_string(),
            NetworkKind::CanFd => "can-fd".to_string(),
            NetworkKind::EthernetDoIp => "ethernet".to_string(),
            NetworkKind::Unknown => "unknown".to_string(),
        },
        entry_point: network.m_bIsDiagnosticEntryPoint,
        bitrate_bps: network.m_optU32BitrateBps,
        data_bitrate_bps: network.m_optU32DataBitrateBps,
    }
}

/// Nothing programmed is a real state, and is written as an absent block rather than as a set
/// of empty-looking fields.
fn BuildIdentityDto(identity: &VehicleIdentity) -> Option<IdentityDto> {
    let bIsEmpty = identity.m_optVecVin.is_none()
        && identity.m_optArrEid.is_none()
        && identity.m_optArrGid.is_none()
        && identity.m_byFurtherActionRequired == 0
        && identity.m_byVinGidSyncStatus == 0;
    if bIsEmpty {
        return None;
    }

    Some(IdentityDto {
        vin: identity
            .m_optVecVin
            .as_ref()
            .map(|vecVin| String::from_utf8_lossy(vecVin).into_owned()),
        eid: identity.m_optArrEid.map(|arr| FormatHexBytes(&arr)),
        gid: identity.m_optArrGid.map(|arr| FormatHexBytes(&arr)),
        further_action: NonZeroByteAsHex(identity.m_byFurtherActionRequired),
        vin_gid_sync_status: NonZeroByteAsHex(identity.m_byVinGidSyncStatus),
    })
}

fn BuildEcuDto(ecu: &Ecu) -> EcuDto {
    EcuDto {
        name: ecu.m_strName.clone(),
        network: ecu.m_optStrNetworkId.clone(),
        gateway_for: ecu.m_vecGatewayForNetworkIds.clone(),
        can: ecu.m_optCanAddress.map(|address| CanAddressDto {
            request: FormatCanId(address.m_u32RequestCanId),
            response: FormatCanId(address.m_u32ResponseCanId),
            addressing: Some(
                match address.m_addressingMode {
                    CanAddressingMode::Normal11Bit => "Normal11Bit",
                    CanAddressingMode::NormalFixed29Bit => "NormalFixed29Bit",
                }
                .to_string(),
            ),
            functional: address.m_optU32FunctionalCanId.map(FormatCanId),
        }),
        doip: BuildDoIpDto(ecu),
        // The flat forms exist so a version 1 file still reads; a file written today uses the
        // `can`/`doip` blocks above and leaves these out rather than saying the same thing
        // twice and inviting the two to disagree.
        request_can_id: None,
        response_can_id: None,
        addressing: None,
        logical_address: None,
        sessions: ecu
            .m_vecSupportedSessions
            .iter()
            .map(|session| {
                match session {
                    SessionType::Default => "default",
                    SessionType::Programming => "programming",
                    SessionType::Extended => "extended",
                    SessionType::SafetySystem => "safety",
                }
                .to_string()
            })
            .collect(),
        session_services: BuildSessionServices(ecu),
        services: Some(ecu.m_vecSupportedServices.iter().map(FormatByte).collect()),
        dids: BuildDids(ecu),
        dtcs: ecu
            .m_vecDtcs
            .iter()
            .map(|dtc| DtcDto {
                code: FormatDtcCode(dtc.m_u32Code),
                status: Some(FormatByte(&dtc.m_byStatus)),
            })
            .collect(),
        security: ecu
            .m_vecSecurityLevels
            .iter()
            .map(BuildSecurityDto)
            .collect(),
        timing: BuildTimingDto(&ecu.m_timing),
        responses: ecu
            .m_vecResponseOverrides
            .iter()
            .map(BuildResponseDto)
            .collect(),
    }
}

/// A DoIP block only when the ECU actually has a logical address to reach it on.
///
/// Zero is not an address: it is what the field holds when nothing set one, and writing it
/// would turn "no DoIP" into "DoIP at address 0" on the next read.
fn BuildDoIpDto(ecu: &Ecu) -> Option<DoIpAddressDto> {
    if ecu.m_u16LogicalAddress == 0 {
        return None;
    }
    Some(DoIpAddressDto {
        logical_address: format!("{:04X}", ecu.m_u16LogicalAddress),
    })
}

fn BuildSessionServices(ecu: &Ecu) -> BTreeMap<String, Vec<String>> {
    ecu.m_mapSessionServices
        .iter()
        .map(|(bySession, vecServices)| {
            let strSession = match SessionType::FromSubFunction(*bySession) {
                Some(SessionType::Default) => "default".to_string(),
                Some(SessionType::Programming) => "programming".to_string(),
                Some(SessionType::Extended) => "extended".to_string(),
                Some(SessionType::SafetySystem) => "safety".to_string(),
                None => FormatByte(bySession),
            };
            (strSession, vecServices.iter().map(FormatByte).collect())
        })
        .collect()
}

/// Data identifiers, written as text when the value plainly is text.
///
/// A VIN written as 17 hex pairs is unreadable and unmaintainable. The rule is conservative:
/// printable ASCII only, and never an empty value, so nothing is guessed into text that was
/// meant as bytes.
fn BuildDids(ecu: &Ecu) -> BTreeMap<String, ValueDto> {
    ecu.m_mapDids
        .iter()
        .map(|(u16Id, did)| {
            let strKey = format!("{u16Id:04X}");
            let bIsPrintableText = !did.m_vecValue.is_empty()
                && did
                    .m_vecValue
                    .iter()
                    .all(|byByte| (0x20..=0x7E).contains(byByte));

            let value = if bIsPrintableText {
                ValueDto::Text {
                    text: String::from_utf8_lossy(&did.m_vecValue).into_owned(),
                }
            } else {
                ValueDto::Hex(FormatHexBytes(&did.m_vecValue))
            };
            (strKey, value)
        })
        .collect()
}

fn BuildSecurityDto(level: &SecurityLevel) -> SecurityDto {
    let (strPolicy, optStrNrc) = match level.m_keyPolicy {
        SecurityKeyPolicy::CompareWithExpectedKey => (None, None),
        SecurityKeyPolicy::AcceptAnyKey => (Some("acceptAny".to_string()), None),
        SecurityKeyPolicy::RefuseWith { m_byNrc } => {
            (Some("refuse".to_string()), Some(format!("{m_byNrc:02X}")))
        }
    };

    SecurityDto {
        request_seed: format!("{:02X}", level.m_byRequestSeedSubFunction),
        seed: FormatHexBytes(&level.m_vecSeed),
        key: FormatHexBytes(&level.m_vecExpectedKey),
        key_policy: strPolicy,
        refusal_nrc: optStrNrc,
    }
}

/// Timing, written only where it differs from the defaults the reader would supply anyway.
fn BuildTimingDto(timing: &EcuTiming) -> Option<TimingDto> {
    let defaults = EcuTiming::default();

    let dto = TimingDto {
        p2_ms: DifferentU32(timing.m_u32P2ServerMaxMs, defaults.m_u32P2ServerMaxMs),
        p2_star_ms: DifferentU32(
            timing.m_u32P2StarServerMaxMs,
            defaults.m_u32P2StarServerMaxMs,
        ),
        p4_ms: DifferentU32(timing.m_u32P4ServerMaxMs, defaults.m_u32P4ServerMaxMs),
        response_delay_ms: DifferentU32(timing.m_u32ResponseDelayMs, 0),
        iso_tp_block_size: if timing.m_u8IsoTpBlockSize == 0 {
            None
        } else {
            Some(timing.m_u8IsoTpBlockSize)
        },
        iso_tp_separation_time_min: NonZeroByteAsHex(timing.m_byIsoTpSeparationTimeMin),
    };

    let bIsAllDefault = dto.p2_ms.is_none()
        && dto.p2_star_ms.is_none()
        && dto.p4_ms.is_none()
        && dto.response_delay_ms.is_none()
        && dto.iso_tp_block_size.is_none()
        && dto.iso_tp_separation_time_min.is_none();

    if bIsAllDefault {
        None
    } else {
        Some(dto)
    }
}

fn BuildResponseDto(overrideRule: &ResponseOverride) -> ResponseDto {
    let (optStrResponse, vecEcho) = match &overrideRule.m_action {
        OverrideAction::Substitute {
            m_vecResponse,
            m_vecEchoSpans,
        } => (
            Some(FormatHexBytes(m_vecResponse)),
            m_vecEchoSpans
                .iter()
                .map(|span| EchoSpanDto {
                    request_offset: span.m_uRequestOffset,
                    length: span.m_uLength,
                    response_offset: span.m_uResponseOffset,
                })
                .collect(),
        ),
        // Suppression is written as an absent response, which is how the reader spells it.
        OverrideAction::Suppress => (None, Vec::new()),
    };

    ResponseDto {
        request: FormatHexPattern(
            &overrideRule.m_vecRequestPattern,
            &overrideRule.m_vecRequestMask,
        ),
        response: optStrResponse,
        match_trailing_bytes: overrideRule.m_bMatchTrailingBytes,
        echo: vecEcho,
        note: overrideRule.m_strNote.clone(),
    }
}

/// Render a pattern with its mask, using `**` where any byte matches — the spelling the reader
/// accepts and the one the response editor shows.
fn FormatHexPattern(vecPattern: &[u8], vecMask: &[u8]) -> String {
    vecPattern
        .iter()
        .enumerate()
        .map(|(uIndex, byValue)| {
            let bIsWildcard = vecMask.get(uIndex).copied().unwrap_or(0xFF) == 0x00;
            if bIsWildcard {
                "**".to_string()
            } else {
                format!("{byValue:02X}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn FormatHexBytes(vecBytes: &[u8]) -> String {
    vecBytes
        .iter()
        .map(|byByte| format!("{byByte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn FormatByte(byByte: &u8) -> String {
    format!("{byByte:02X}")
}

/// Identifiers keep their natural width: three digits for 11-bit, eight for 29-bit.
fn FormatCanId(u32CanId: u32) -> String {
    if u32CanId > 0x7FF {
        format!("{u32CanId:08X}")
    } else {
        format!("{u32CanId:03X}")
    }
}

fn NonZeroByteAsHex(byByte: u8) -> Option<String> {
    if byByte == 0 {
        None
    } else {
        Some(format!("{byByte:02X}"))
    }
}

fn DifferentU32(u32Value: u32, u32Default: u32) -> Option<u32> {
    if u32Value == u32Default {
        None
    } else {
        Some(u32Value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_domain::model::{CanAddress, DataIdentifier, EchoSpan};
    use core_domain::Confidence;

    /// A vehicle carrying one of everything this format can state.
    fn BuildWorkedOnVehicle() -> Vehicle {
        let mut ecu = Ecu::New("CAN(B)_Security Gateway", 0);
        ecu.m_optCanAddress = Some(CanAddress::NewSpecified(
            0x18DAD4F1,
            0x18DAF1D4,
            CanAddressingMode::NormalFixed29Bit,
        ));
        ecu.m_vecSupportedServices = vec![0x10, 0x22, 0x27];
        ecu.m_vecSupportedSessions = vec![SessionType::Default, SessionType::Extended];
        ecu.m_mapDids.insert(
            0x0111,
            DataIdentifier {
                m_u16Id: 0x0111,
                m_vecValue: vec![0xF3, 0x64, 0xF0],
                m_confidence: Confidence::Observed,
            },
        );
        ecu.m_mapDids.insert(
            0xF190,
            DataIdentifier {
                m_u16Id: 0xF190,
                m_vecValue: b"1HGCM82633A004352".to_vec(),
                m_confidence: Confidence::Confirmed,
            },
        );
        ecu.m_vecSecurityLevels.push(SecurityLevel {
            m_byRequestSeedSubFunction: 0x01,
            m_vecSeed: vec![0x93, 0xD7, 0xC5, 0x02],
            m_vecExpectedKey: Vec::new(),
            m_keyPolicy: SecurityKeyPolicy::AcceptAnyKey,
        });
        ecu.m_timing.m_u8IsoTpBlockSize = 8;
        ecu.m_timing.m_byIsoTpSeparationTimeMin = 0x03;
        ecu.m_vecResponseOverrides.push(ResponseOverride {
            m_vecRequestPattern: vec![0x22, 0x00, 0x00],
            m_vecRequestMask: vec![0xFF, 0x00, 0x00],
            m_bMatchTrailingBytes: false,
            m_action: OverrideAction::Substitute {
                m_vecResponse: vec![0x62, 0x00, 0x00, 0x01],
                m_vecEchoSpans: vec![EchoSpan {
                    m_uRequestOffset: 1,
                    m_uLength: 2,
                    m_uResponseOffset: 1,
                }],
            },
            m_strNote: "ReadDataByIdentifier — any identifier".to_string(),
            m_bRespondEvenIfSuppressed: false,
            m_bIsEnabled: true,
        });

        Vehicle {
            m_strName: "P33C".to_string(),
            m_vecEcus: vec![ecu],
            m_vecNetworks: Vec::new(),
            m_identity: VehicleIdentity::default(),
        }
    }

    #[test]
    fn a_worked_on_vehicle_survives_a_round_trip() {
        // The property the whole module exists for: what is written must read back as the same
        // vehicle, or exporting is a way to lose work rather than keep it.
        let original = BuildWorkedOnVehicle();
        let strText = WriteSimFileText(&original).expect("the vehicle serializes");

        let reloaded = crate::LoadFromText(&strText).expect("what was written must re-read");

        assert_eq!(reloaded.m_strName, original.m_strName);
        assert_eq!(reloaded.m_vecEcus.len(), 1);

        let ecuBefore = &original.m_vecEcus[0];
        let ecuAfter = &reloaded.m_vecEcus[0];
        assert_eq!(ecuAfter.m_strName, ecuBefore.m_strName);
        let addressBefore = ecuBefore.m_optCanAddress.expect("the ECU has an address");
        let addressAfter = ecuAfter.m_optCanAddress.expect("and keeps it");
        assert_eq!(
            addressAfter.m_u32RequestCanId,
            addressBefore.m_u32RequestCanId
        );
        assert_eq!(
            addressAfter.m_u32ResponseCanId,
            addressBefore.m_u32ResponseCanId
        );
        assert_eq!(
            addressAfter.m_addressingMode,
            addressBefore.m_addressingMode
        );
        assert_eq!(
            ecuAfter.m_vecSupportedServices,
            ecuBefore.m_vecSupportedServices
        );

        // The three things an operator configures by hand, and would be most annoyed to lose.
        assert_eq!(
            ecuAfter.m_timing.m_u8IsoTpBlockSize, 8,
            "flow control must survive, or a slow adapter breaks again on reload"
        );
        assert_eq!(ecuAfter.m_timing.m_byIsoTpSeparationTimeMin, 0x03);
        assert_eq!(ecuAfter.m_vecSecurityLevels.len(), 1);
        assert_eq!(
            ecuAfter.m_vecSecurityLevels[0].m_keyPolicy,
            SecurityKeyPolicy::AcceptAnyKey
        );
        assert_eq!(ecuAfter.m_vecResponseOverrides.len(), 1);
        assert_eq!(
            ecuAfter.m_vecResponseOverrides[0].m_vecRequestMask,
            vec![0xFF, 0x00, 0x00],
            "a wildcard must come back a wildcard, not a literal 00"
        );
    }

    #[test]
    fn identifiers_are_written_as_text_only_when_they_plainly_are_text() {
        let strText = WriteSimFileText(&BuildWorkedOnVehicle()).expect("serializes");

        assert!(
            strText.contains("1HGCM82633A004352"),
            "a VIN written as 17 hex pairs is unreadable; it should be text"
        );
        assert!(
            strText.contains("F3 64 F0"),
            "bytes that are not printable text stay hex"
        );
    }

    #[test]
    fn defaults_are_left_out_rather_than_restated() {
        let mut vehicle = BuildWorkedOnVehicle();
        vehicle.m_vecEcus[0].m_timing = EcuTiming::default();

        let strText = WriteSimFileText(&vehicle).expect("serializes");
        assert!(
            !strText.contains("\"timing\""),
            "an ECU on default timing should say nothing about timing"
        );
        assert!(
            !strText.contains("\"identity\": {"),
            "nothing programmed is written as an absent block, not an empty one"
        );
    }
}

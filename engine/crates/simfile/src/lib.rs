//! Simulation files: a vehicle described in a file rather than captured or clicked together.
//!
//! A third way to get a vehicle, alongside reconstructing one from a CAN log and building one
//! by hand. It is the only one of the three that can state how the ECUs are **wired** — a
//! capture cannot observe bus membership and a person clicking ECUs together has not been
//! asked — so it is what makes a real topology diagram possible.
//!
//! Everything a file states is `Confidence::Confirmed`: nothing was observed on a bus, but
//! nothing was guessed either. It came from someone who knows the vehicle, which is the
//! standing a specification has.

#![allow(non_snake_case, non_upper_case_globals)]

pub mod dto;
pub mod encode;

use core_domain::model::{
    CanAddress, CanAddressingMode, DataIdentifier, DiagnosticTroubleCode, Ecu, EcuTiming, Network,
    NetworkKind, OverrideAction, ResponseOverride, SecurityLevel, SessionType, Vehicle,
};
use core_domain::Confidence;

use crate::dto::{
    c_uCurrentVersion, c_uMinSupportedVersion, CanAddressDto, EcuDto, NetworkDto, ResponseDto,
    SimFileDto, TimingDto, ValueDto,
};
use crate::encode::{ParseDtcCode, ParseHexByte, ParseHexBytes, ParseHexPattern, ParseStatusByte};

/// Why a simulation file could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SimFileError {
    /// The text is not JSON, or not the shape a simfile has.
    #[error("this is not a valid simulation file: {strReason}")]
    Malformed { strReason: String },

    /// Written for a version of the format this engine does not know.
    #[error("the file says it is version {uFileVersion}; this engine understands versions {c_uMinSupportedVersion} to {c_uCurrentVersion}")]
    UnsupportedVersion { uFileVersion: u32 },

    /// The declared wiring does not describe a vehicle that could exist.
    #[error("{0}")]
    Topology(#[from] core_domain::model::TopologyError),

    /// An ECU says nothing about how anything reaches it.
    #[error("ECU '{strEcuName}' has neither CAN identifiers nor a DoIP logical address, so nothing could address it")]
    NoAddressing { strEcuName: String },

    /// An ECU gives its CAN identifiers twice, in both spellings.
    #[error("ECU '{strEcuName}' sets both 'can' and the older 'requestCanId'/'responseCanId'; use one or the other")]
    DuplicateCanAddressing { strEcuName: String },

    /// A field could not be read.
    #[error("{strWhere}: {strReason}")]
    BadField { strWhere: String, strReason: String },

    /// An ECU names a bus the file does not define.
    #[error("ECU '{strEcuName}' is on network '{strNetworkId}', which the file does not define")]
    UnknownNetwork {
        strEcuName: String,
        strNetworkId: String,
    },

    /// Two ECUs would be indistinguishable.
    #[error("ECUs '{strFirstEcu}' and '{strSecondEcu}' both use CAN id 0x{u32CanId:X}")]
    DuplicateCanId {
        u32CanId: u32,
        strFirstEcu: String,
        strSecondEcu: String,
    },

    /// Two buses share an id.
    #[error("two networks share the id '{strNetworkId}'")]
    DuplicateNetworkId { strNetworkId: String },

    /// The file describes no ECUs.
    #[error("the file describes no ECUs")]
    NoEcus,
}

/// Read a simulation file and build the vehicle it describes.
pub fn LoadFromText(strContent: &str) -> Result<Vehicle, SimFileError> {
    let file: SimFileDto =
        serde_json::from_str(strContent).map_err(|error| SimFileError::Malformed {
            strReason: error.to_string(),
        })?;

    let bIsSupported =
        file.simfile_version >= c_uMinSupportedVersion && file.simfile_version <= c_uCurrentVersion;
    if !bIsSupported {
        return Err(SimFileError::UnsupportedVersion {
            uFileVersion: file.simfile_version,
        });
    }
    if file.ecus.is_empty() {
        return Err(SimFileError::NoEcus);
    }

    let vecNetworks = BuildNetworks(&file.networks)?;
    let vecEcus = BuildEcus(&file.ecus, &vecNetworks)?;

    let mut vehicle = Vehicle {
        m_strName: file.vehicle,
        m_vecEcus: vecEcus,
        m_vecNetworks: vecNetworks,
    };

    // Order matters: entry points are decided first, because whether the wiring makes sense
    // and how deep each ECU sits are both answers relative to where a tester attaches.
    vehicle.NormalizeEntryPoints();
    vehicle.ValidateTopology()?;

    let uGatewayCount = vehicle
        .m_vecEcus
        .iter()
        .filter(|ecu| !ecu.m_vecGatewayForNetworkIds.is_empty())
        .count();
    tracing::info!(
        vehicle = %vehicle.m_strName,
        networks = vehicle.m_vecNetworks.len(),
        ecus = vehicle.m_vecEcus.len(),
        gateways = uGatewayCount,
        "loaded a simulation file"
    );

    Ok(vehicle)
}

/// Turn the file's buses into model networks, refusing two that share an id.
fn BuildNetworks(vecDtos: &[NetworkDto]) -> Result<Vec<Network>, SimFileError> {
    let mut vecNetworks: Vec<Network> = Vec::with_capacity(vecDtos.len());

    for dto in vecDtos {
        if vecNetworks
            .iter()
            .any(|existing| existing.m_strId == dto.id)
        {
            return Err(SimFileError::DuplicateNetworkId {
                strNetworkId: dto.id.clone(),
            });
        }

        vecNetworks.push(Network {
            m_strId: dto.id.clone(),
            m_strName: dto.name.clone(),
            m_kind: ParseNetworkKind(&dto.kind, &dto.id)?,
            m_optU32BitrateBps: dto.bitrate_bps,
            m_optU32DataBitrateBps: dto.data_bitrate_bps,
            m_bIsDiagnosticEntryPoint: dto.entry_point,
            // Stated by the file's author, who knows the vehicle. Nothing was observed, but
            // nothing was guessed.
            m_confidence: Confidence::Confirmed,
        });
    }
    Ok(vecNetworks)
}

/// Read a network kind, naming the alternatives when it is not one of them.
fn ParseNetworkKind(strKind: &str, strNetworkId: &str) -> Result<NetworkKind, SimFileError> {
    match strKind.to_ascii_lowercase().as_str() {
        "can" | "can-classic" => Ok(NetworkKind::CanClassic),
        "can-fd" | "canfd" => Ok(NetworkKind::CanFd),
        "ethernet" | "doip" | "ethernet-doip" => Ok(NetworkKind::EthernetDoIp),
        "unknown" => Ok(NetworkKind::Unknown),
        strOther => Err(SimFileError::BadField {
            strWhere: format!("network '{strNetworkId}'"),
            strReason: format!("'{strOther}' is not a kind of link; use CAN, CAN-FD or Ethernet"),
        }),
    }
}

/// Turn the file's ECUs into model ECUs, refusing anything that could not be routed.
fn BuildEcus(vecDtos: &[EcuDto], vecNetworks: &[Network]) -> Result<Vec<Ecu>, SimFileError> {
    let mut vecEcus: Vec<Ecu> = Vec::with_capacity(vecDtos.len());

    for dto in vecDtos {
        let ecu = BuildEcu(dto, vecNetworks)?;
        RejectDuplicateIdentifiers(&vecEcus, &ecu)?;
        vecEcus.push(ecu);
    }
    Ok(vecEcus)
}

/// Build one ECU from its description.
fn BuildEcu(dto: &EcuDto, vecNetworks: &[Network]) -> Result<Ecu, SimFileError> {
    let strWhere = format!("ECU '{}'", dto.name);

    if let Some(strNetworkId) = &dto.network {
        let bIsKnown = vecNetworks
            .iter()
            .any(|network| &network.m_strId == strNetworkId);
        if !bIsKnown {
            return Err(SimFileError::UnknownNetwork {
                strEcuName: dto.name.clone(),
                strNetworkId: strNetworkId.clone(),
            });
        }
    }

    let optCanAddress = BuildCanAddress(dto, &strWhere)?;
    let optU16DoIpAddress = BuildDoIpAddress(dto, &strWhere)?;

    // An ECU nothing can address is a description of nothing. Either transport will do — this
    // is where the file's CAN-or-DoIP-or-both freedom stops being free.
    if optCanAddress.is_none() && optU16DoIpAddress.is_none() {
        return Err(SimFileError::NoAddressing {
            strEcuName: dto.name.clone(),
        });
    }

    let mut ecu = Ecu::New(&dto.name, optU16DoIpAddress.unwrap_or(0));
    ecu.m_bHasDoIpAddress = optU16DoIpAddress.is_some();
    ecu.m_optCanAddress = optCanAddress;
    ecu.m_optStrNetworkId = dto.network.clone();
    ecu.m_vecGatewayForNetworkIds = dto.gateway_for.clone();
    ecu.m_vecSupportedSessions = ParseSessions(&dto.sessions, &strWhere)?;
    ecu.m_vecSupportedServices = ParseServices(dto, &strWhere)?;
    ecu.m_mapSessionServices = ParseSessionServices(dto, &strWhere)?;
    ecu.m_mapDids = BuildDids(dto, &strWhere)?;
    ecu.m_vecDtcs = BuildDtcs(dto, &strWhere)?;
    ecu.m_vecSecurityLevels = BuildSecurityLevels(dto, &strWhere)?;
    ecu.m_timing = BuildTiming(dto.timing.as_ref());
    ecu.m_vecResponseOverrides = BuildOverrides(&dto.responses, &strWhere)?;

    Ok(ecu)
}

/// Refuse an ECU whose identifiers collide with one already read.
fn RejectDuplicateIdentifiers(vecEcus: &[Ecu], candidate: &Ecu) -> Result<(), SimFileError> {
    let address = match candidate.m_optCanAddress {
        Some(address) => address,
        None => return Ok(()),
    };

    for existing in vecEcus {
        let existingAddress = match existing.m_optCanAddress {
            Some(existingAddress) => existingAddress,
            None => continue,
        };

        // A shared request identifier makes routing ambiguous; a shared response identifier
        // makes two ECUs indistinguishable on the wire. Either is fatal.
        for u32CanId in [
            existingAddress.m_u32RequestCanId,
            existingAddress.m_u32ResponseCanId,
        ] {
            let bCollides =
                u32CanId == address.m_u32RequestCanId || u32CanId == address.m_u32ResponseCanId;
            if bCollides {
                return Err(SimFileError::DuplicateCanId {
                    u32CanId,
                    strFirstEcu: existing.m_strName.clone(),
                    strSecondEcu: candidate.m_strName.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Read an ECU's CAN addressing, in either the version 2 or the version 1 spelling.
///
/// Returns `None` for an ECU that declares none, which is legitimate: a DoIP-only ECU behind a
/// gateway has no CAN identifiers to give.
fn BuildCanAddress(dto: &EcuDto, strWhere: &str) -> Result<Option<CanAddress>, SimFileError> {
    let bHasLegacyPair = dto.request_can_id.is_some() || dto.response_can_id.is_some();
    if dto.can.is_some() && bHasLegacyPair {
        return Err(SimFileError::DuplicateCanAddressing {
            strEcuName: dto.name.clone(),
        });
    }

    let can = match ResolveCanBlock(dto, strWhere)? {
        Some(can) => can,
        None => return Ok(None),
    };

    let u32RequestCanId = ParseCanId(&can.request, strWhere)?;
    let u32ResponseCanId = ParseCanId(&can.response, strWhere)?;
    if u32RequestCanId == u32ResponseCanId {
        return Err(SimFileError::BadField {
            strWhere: strWhere.to_string(),
            strReason: "an ECU cannot request and respond on the same CAN id".to_string(),
        });
    }

    let mode = ParseAddressing(
        can.addressing.as_deref(),
        u32RequestCanId,
        u32ResponseCanId,
        strWhere,
    )?;

    let mut address = CanAddress::NewSpecified(u32RequestCanId, u32ResponseCanId, mode);
    if let Some(strFunctional) = &can.functional {
        let u32FunctionalCanId = ParseCanId(strFunctional, strWhere)?;
        if u32FunctionalCanId == u32RequestCanId {
            return Err(SimFileError::BadField {
                strWhere: strWhere.to_string(),
                strReason: format!(
                    "0x{u32FunctionalCanId:X} is given as the functional id and as this ECU's own request id; routing could not tell the two apart"
                ),
            });
        }
        address.m_optU32FunctionalCanId = Some(u32FunctionalCanId);
    }
    Ok(Some(address))
}

/// Gather the CAN block from whichever spelling the file used.
///
/// Version 1 wrote the identifiers flat on the ECU and had no functional id; version 2 groups
/// them, so the loader normalises to the version 2 shape and everything downstream sees one.
fn ResolveCanBlock(dto: &EcuDto, strWhere: &str) -> Result<Option<CanAddressDto>, SimFileError> {
    if let Some(can) = &dto.can {
        return Ok(Some(can.clone()));
    }

    match (&dto.request_can_id, &dto.response_can_id) {
        (None, None) => Ok(None),
        (Some(strRequest), Some(strResponse)) => Ok(Some(CanAddressDto {
            request: strRequest.clone(),
            response: strResponse.clone(),
            addressing: dto.addressing.clone(),
            functional: None,
        })),
        // Half a pair cannot be routed and is far more likely a typo than an intention.
        _ => Err(SimFileError::BadField {
            strWhere: strWhere.to_string(),
            strReason:
                "requestCanId and responseCanId come as a pair; give both, or use the 'can' block"
                    .to_string(),
        }),
    }
}

/// Read an ECU's DoIP logical address, in either spelling.
fn BuildDoIpAddress(dto: &EcuDto, strWhere: &str) -> Result<Option<u16>, SimFileError> {
    if let Some(doip) = &dto.doip {
        let strTrimmed = doip.logical_address.trim();
        let strDigits = strTrimmed
            .strip_prefix("0x")
            .or_else(|| strTrimmed.strip_prefix("0X"))
            .unwrap_or(strTrimmed);

        let u16LogicalAddress =
            u16::from_str_radix(strDigits, 16).map_err(|_| SimFileError::BadField {
                strWhere: strWhere.to_string(),
                strReason: format!(
                    "'{}' is not a hex DoIP logical address",
                    doip.logical_address
                ),
            })?;
        return Ok(Some(u16LogicalAddress));
    }
    Ok(dto.logical_address)
}

/// Read a CAN identifier written in hex, with or without an `0x`.
fn ParseCanId(strValue: &str, strWhere: &str) -> Result<u32, SimFileError> {
    let strTrimmed = strValue.trim();
    let strDigits = strTrimmed
        .strip_prefix("0x")
        .or_else(|| strTrimmed.strip_prefix("0X"))
        .unwrap_or(strTrimmed);

    let u32CanId = u32::from_str_radix(strDigits, 16).map_err(|_| SimFileError::BadField {
        strWhere: strWhere.to_string(),
        strReason: format!("'{strValue}' is not a hex CAN id"),
    })?;

    if u32CanId > 0x1FFF_FFFF {
        return Err(SimFileError::BadField {
            strWhere: strWhere.to_string(),
            strReason: format!("CAN id 0x{u32CanId:X} is wider than the 29-bit maximum"),
        });
    }
    Ok(u32CanId)
}

/// Read the addressing mode, or work it out from the identifier width.
fn ParseAddressing(
    optStrAddressing: Option<&str>,
    u32RequestCanId: u32,
    u32ResponseCanId: u32,
    strWhere: &str,
) -> Result<CanAddressingMode, SimFileError> {
    match optStrAddressing {
        None => {
            let bIsExtended = u32RequestCanId > 0x7FF || u32ResponseCanId > 0x7FF;
            Ok(if bIsExtended {
                CanAddressingMode::NormalFixed29Bit
            } else {
                CanAddressingMode::Normal11Bit
            })
        }
        Some("Normal11Bit") => Ok(CanAddressingMode::Normal11Bit),
        Some("NormalFixed29Bit") => Ok(CanAddressingMode::NormalFixed29Bit),
        Some(strOther) => Err(SimFileError::BadField {
            strWhere: strWhere.to_string(),
            strReason: format!(
                "'{strOther}' is not an addressing mode; use Normal11Bit or NormalFixed29Bit"
            ),
        }),
    }
}

/// Read the sessions an ECU can enter. An empty list gets the two a tester actually uses.
fn ParseSessions(vecNames: &[String], strWhere: &str) -> Result<Vec<SessionType>, SimFileError> {
    if vecNames.is_empty() {
        return Ok(vec![SessionType::Default, SessionType::Extended]);
    }

    let mut vecSessions = Vec::with_capacity(vecNames.len());
    for strName in vecNames {
        vecSessions.push(ParseSessionName(strName, strWhere)?);
    }

    // An ECU that cannot enter the default session is incoherent: that is the one it powers up
    // in, so it is added rather than rejected.
    if !vecSessions.contains(&SessionType::Default) {
        tracing::warn!(
            ecu = %strWhere,
            "the default session was not listed; adding it, since an ECU powers up in it"
        );
        vecSessions.insert(0, SessionType::Default);
    }
    Ok(vecSessions)
}

/// Read the services an ECU supports, defaulting to the ones the engine implements.
fn ParseServices(dto: &EcuDto, strWhere: &str) -> Result<Vec<u8>, SimFileError> {
    let vecNames = match &dto.services {
        Some(vecNames) => vecNames,
        None => return Ok(vec![0x10, 0x11, 0x19, 0x22, 0x27, 0x31, 0x3E]),
    };

    let mut vecServices = Vec::with_capacity(vecNames.len());
    for strName in vecNames {
        vecServices.push(
            ParseHexByte(strName).map_err(|strReason| SimFileError::BadField {
                strWhere: strWhere.to_string(),
                strReason: format!("service '{strName}': {strReason}"),
            })?,
        );
    }
    Ok(vecServices)
}

/// Read which services each session allows.
fn ParseSessionServices(
    dto: &EcuDto,
    strWhere: &str,
) -> Result<std::collections::BTreeMap<u8, Vec<u8>>, SimFileError> {
    let mut mapSessionServices = std::collections::BTreeMap::new();

    for (strSession, vecServiceNames) in &dto.session_services {
        let session = ParseSessionName(strSession, strWhere)?;

        let mut vecServices = Vec::with_capacity(vecServiceNames.len());
        for strService in vecServiceNames {
            vecServices.push(ParseHexByte(strService).map_err(|strReason| {
                SimFileError::BadField {
                    strWhere: strWhere.to_string(),
                    strReason: format!(
                        "session '{strSession}', service '{strService}': {strReason}"
                    ),
                }
            })?);
        }

        // Restricting a session the ECU cannot enter describes behaviour nothing can reach.
        if !dto.sessions.is_empty() {
            let bIsEnterable = dto
                .sessions
                .iter()
                .any(|strName| strName.eq_ignore_ascii_case(strSession));
            if !bIsEnterable {
                return Err(SimFileError::BadField {
                    strWhere: strWhere.to_string(),
                    strReason: format!(
                        "sessionServices restricts '{strSession}', which is not in this ECU's sessions"
                    ),
                });
            }
        }

        mapSessionServices.insert(session.ToSubFunction(), vecServices);
    }
    Ok(mapSessionServices)
}

/// Read one session name.
fn ParseSessionName(strName: &str, strWhere: &str) -> Result<SessionType, SimFileError> {
    match strName.to_ascii_lowercase().as_str() {
        "default" => Ok(SessionType::Default),
        "programming" => Ok(SessionType::Programming),
        "extended" => Ok(SessionType::Extended),
        "safety" | "safetysystem" => Ok(SessionType::SafetySystem),
        strOther => Err(SimFileError::BadField {
            strWhere: strWhere.to_string(),
            strReason: format!(
                "'{strOther}' is not a session; use default, programming, extended or safety"
            ),
        }),
    }
}

/// Read the data identifiers an ECU answers.
fn BuildDids(
    dto: &EcuDto,
    strWhere: &str,
) -> Result<std::collections::BTreeMap<u16, DataIdentifier>, SimFileError> {
    let mut mapDids = std::collections::BTreeMap::new();

    for (strDid, value) in &dto.dids {
        let u16Did = u16::from_str_radix(strDid.trim_start_matches("0x"), 16).map_err(|_| {
            SimFileError::BadField {
                strWhere: strWhere.to_string(),
                strReason: format!("'{strDid}' is not a hex data identifier"),
            }
        })?;

        let vecValue = ReadValue(value).map_err(|strReason| SimFileError::BadField {
            strWhere: strWhere.to_string(),
            strReason: format!("DID {strDid}: {strReason}"),
        })?;

        mapDids.insert(
            u16Did,
            DataIdentifier {
                m_u16Id: u16Did,
                m_vecValue: vecValue,
                m_confidence: Confidence::Confirmed,
            },
        );
    }
    Ok(mapDids)
}

/// Read a value written either as hex or as text.
fn ReadValue(value: &ValueDto) -> Result<Vec<u8>, String> {
    match value {
        ValueDto::Text { text } => Ok(text.as_bytes().to_vec()),
        ValueDto::Hex(strHex) => ParseHexBytes(strHex).map_err(|strReason| {
            format!("{strReason}. A bare string is hex; for characters write {{\"text\": \"...\"}}")
        }),
    }
}

/// Read the trouble codes an ECU reports.
fn BuildDtcs(dto: &EcuDto, strWhere: &str) -> Result<Vec<DiagnosticTroubleCode>, SimFileError> {
    let mut vecDtcs = Vec::with_capacity(dto.dtcs.len());

    for entry in &dto.dtcs {
        let u32Code = ParseDtcCode(&entry.code).map_err(|strReason| SimFileError::BadField {
            strWhere: strWhere.to_string(),
            strReason: format!("DTC '{}': {strReason}", entry.code),
        })?;

        let byStatus = match &entry.status {
            Some(strStatus) => {
                ParseStatusByte(strStatus).map_err(|strReason| SimFileError::BadField {
                    strWhere: strWhere.to_string(),
                    strReason: format!("DTC '{}' status: {strReason}", entry.code),
                })?
            }
            // Confirmed, and stored since the last clear: the state a fault a tester is meant
            // to find would actually be in.
            None => 0x08 | 0x04,
        };

        vecDtcs.push(DiagnosticTroubleCode {
            m_u32Code: u32Code,
            m_byStatus: byStatus,
            m_confidence: Confidence::Confirmed,
        });
    }
    Ok(vecDtcs)
}

/// Read the security levels an ECU offers.
fn BuildSecurityLevels(dto: &EcuDto, strWhere: &str) -> Result<Vec<SecurityLevel>, SimFileError> {
    let mut vecLevels = Vec::with_capacity(dto.security.len());

    for entry in &dto.security {
        let bySubFunction =
            ParseHexByte(&entry.request_seed).map_err(|strReason| SimFileError::BadField {
                strWhere: strWhere.to_string(),
                strReason: format!("security requestSeed: {strReason}"),
            })?;

        // Odd sub-functions request a seed and even ones send a key, so an even value here
        // would configure a level a tester can never reach.
        if bySubFunction % 2 == 0 {
            return Err(SimFileError::BadField {
                strWhere: strWhere.to_string(),
                strReason: format!(
                    "security requestSeed 0x{bySubFunction:02X} is even; requestSeed sub-functions are odd, and sendKey is the next value up"
                ),
            });
        }

        let vecSeed = ParseHexBytes(&entry.seed).map_err(|strReason| SimFileError::BadField {
            strWhere: strWhere.to_string(),
            strReason: format!("security seed: {strReason}"),
        })?;

        // An all-zero seed is how an ECU says it is already unlocked, so configuring one makes
        // locked and unlocked indistinguishable on the wire.
        if vecSeed.iter().all(|byByte| *byByte == 0) {
            return Err(SimFileError::BadField {
                strWhere: strWhere.to_string(),
                strReason: "an all-zero seed means 'already unlocked'; use a non-zero one"
                    .to_string(),
            });
        }

        let vecKey = ParseHexBytes(&entry.key).map_err(|strReason| SimFileError::BadField {
            strWhere: strWhere.to_string(),
            strReason: format!("security key: {strReason}"),
        })?;

        vecLevels.push(SecurityLevel {
            m_byRequestSeedSubFunction: bySubFunction,
            m_vecSeed: vecSeed,
            m_vecExpectedKey: vecKey,
        });
    }
    Ok(vecLevels)
}

/// Read timing overrides onto the defaults.
fn BuildTiming(optTiming: Option<&TimingDto>) -> EcuTiming {
    let timing = match optTiming {
        Some(timing) => timing,
        None => return EcuTiming::default(),
    };

    let defaults = EcuTiming::default();
    EcuTiming {
        m_u32P2ServerMaxMs: timing.p2_ms.unwrap_or(defaults.m_u32P2ServerMaxMs),
        m_u32P2StarServerMaxMs: timing.p2_star_ms.unwrap_or(defaults.m_u32P2StarServerMaxMs),
        m_u32P4ServerMaxMs: timing.p4_ms.unwrap_or(defaults.m_u32P4ServerMaxMs),
        m_u32ResponseDelayMs: timing.response_delay_ms.unwrap_or(0),
        ..defaults
    }
}

/// Read the answers an ECU gives to particular requests.
fn BuildOverrides(
    vecDtos: &[ResponseDto],
    strWhere: &str,
) -> Result<Vec<ResponseOverride>, SimFileError> {
    let mut vecOverrides = Vec::with_capacity(vecDtos.len());

    for dto in vecDtos {
        let (vecPattern, vecMask) =
            ParseHexPattern(&dto.request).map_err(|strReason| SimFileError::BadField {
                strWhere: strWhere.to_string(),
                strReason: format!("response for '{}': {strReason}", dto.request),
            })?;

        let action = match &dto.response {
            None => OverrideAction::Suppress,
            Some(strResponse) => OverrideAction::Substitute {
                m_vecResponse: ParseHexBytes(strResponse).map_err(|strReason| {
                    SimFileError::BadField {
                        strWhere: strWhere.to_string(),
                        strReason: format!("response for '{}': {strReason}", dto.request),
                    }
                })?,
                m_vecEchoSpans: Vec::new(),
            },
        };

        let overrideRule = ResponseOverride {
            m_vecRequestPattern: vecPattern,
            m_vecRequestMask: vecMask,
            m_bMatchTrailingBytes: false,
            m_action: action,
            m_bIsEnabled: true,
            m_bRespondEvenIfSuppressed: false,
            m_strNote: dto.note.clone(),
        };

        // The same rules the API enforces, so a file cannot express an exchange the UI would
        // refuse.
        overrideRule
            .Validate()
            .map_err(|error| SimFileError::BadField {
                strWhere: strWhere.to_string(),
                strReason: format!("response for '{}': {error}", dto.request),
            })?;

        vecOverrides.push(overrideRule);
    }
    Ok(vecOverrides)
}

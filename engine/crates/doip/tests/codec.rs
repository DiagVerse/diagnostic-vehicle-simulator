//! DoIP wire format, tested against the specific mistakes ISO 13400-2 conformance tests catch.
//!
//! Each test names the trap rather than the function, because the function being right is not
//! the interesting claim — the interesting claim is that a particular well-known way of getting
//! it wrong is not present.

#![allow(non_snake_case, non_upper_case_globals)]

use doip::header::{
    c_byProtocolVersion2012, c_byProtocolVersion2019, c_byProtocolVersionDefault, HeaderLimits,
    HeaderNack, ReadHeader, ReplyVersionFor, WriteMessage,
};
use doip::messages::*;
use doip::payload::PayloadType;

/// Build the eight header bytes for a message that is not otherwise present.
fn Header(byVersion: u8, u16PayloadType: u16, u32Length: u32) -> Vec<u8> {
    let mut vecBytes = vec![byVersion, !byVersion];
    vecBytes.extend_from_slice(&u16PayloadType.to_be_bytes());
    vecBytes.extend_from_slice(&u32Length.to_be_bytes());
    vecBytes
}

#[test]
fn a_header_round_trips_through_the_codec() {
    let vecMessage = WriteMessage(
        c_byProtocolVersion2019,
        PayloadType::AliveCheckResponse,
        &[0x0E, 0x80],
    );

    assert_eq!(vecMessage[0], 0x03);
    assert_eq!(vecMessage[1], 0xFC, "the inverse version is version XOR FF");
    assert_eq!(&vecMessage[2..4], &[0x00, 0x08]);
    assert_eq!(
        &vecMessage[4..8],
        &[0, 0, 0, 2],
        "the length field excludes the header itself"
    );

    let header = ReadHeader(&vecMessage, HeaderLimits::default()).expect("valid");
    assert_eq!(header.m_payloadType, PayloadType::AliveCheckResponse);
    assert_eq!(header.m_u32PayloadLength, 2);
}

#[test]
fn a_broken_synchronisation_pattern_closes_the_socket() {
    // NACK 0x00. The pairing of which codes close and which discard is the trap: 0x00 and 0x04
    // close, 0x01 through 0x03 keep the connection.
    let mut vecBytes = Header(0x03, 0x0001, 0);
    vecBytes[1] = 0x00; // should be 0xFC

    let nack = ReadHeader(&vecBytes, HeaderLimits::default()).expect_err("bad pattern");
    assert_eq!(nack.Code(), 0x00);
    assert!(nack.ClosesSocket());
}

#[test]
fn an_unknown_payload_type_is_refused_without_closing() {
    let vecBytes = Header(0x03, 0x1234, 0);
    let nack = ReadHeader(&vecBytes, HeaderLimits::default()).expect_err("unknown type");
    assert_eq!(nack.Code(), 0x01);
    assert!(
        !nack.ClosesSocket(),
        "0x01 discards the message, not the socket"
    );
}

#[test]
fn a_wrong_payload_length_closes_the_socket() {
    // An alive check request is defined as zero-length; anything else is NACK 0x04 + close.
    let vecBytes = Header(0x03, 0x0007, 4);
    let nack = ReadHeader(&vecBytes, HeaderLimits::default()).expect_err("wrong length");
    assert_eq!(nack.Code(), 0x04);
    assert!(nack.ClosesSocket());
}

#[test]
fn the_capability_limit_and_the_memory_limit_are_different_codes() {
    // 0x02 is "bigger than I can ever accept", a static capability. 0x03 is "bigger than I can
    // accept right now". Conformance tests distinguish them, and collapsing the two is common.
    let limits = HeaderLimits {
        m_u32MaxDataSize: 1000,
        m_u32AvailableMemory: 100,
    };

    let overCapability = ReadHeader(&Header(0x03, 0x8001, 5000), limits).expect_err("too large");
    assert_eq!(overCapability.Code(), 0x02);
    assert!(!overCapability.ClosesSocket());

    let overMemory = ReadHeader(&Header(0x03, 0x8001, 500), limits).expect_err("no memory");
    assert_eq!(overMemory.Code(), 0x03);
    assert!(!overMemory.ClosesSocket());
}

#[test]
fn an_oversized_length_is_refused_from_the_header_alone() {
    // The check must be decidable from eight bytes. A header claiming four gigabytes must be
    // refused before anything tries to hold the body — that is the whole reason the length
    // check precedes reading the payload.
    let vecBytes = Header(0x03, 0x8001, u32::MAX);
    let nack = ReadHeader(&vecBytes, HeaderLimits::default()).expect_err("absurd length");
    assert_eq!(nack.Code(), 0x02);
    assert_eq!(vecBytes.len(), 8, "and no payload was needed to decide it");
}

#[test]
fn a_vehicle_identification_request_may_carry_the_placeholder_version() {
    // REQ 7.DoIP-156 AL: the version value in a vehicle identification request is always
    // ignored, because a tester that has not discovered the vehicle cannot know what to send.
    // NACKing 0xFF here is a listed trap.
    let vecBytes = Header(c_byProtocolVersionDefault, 0x0001, 0);
    let header = ReadHeader(&vecBytes, HeaderLimits::default())
        .expect("0xFF is legal in a vehicle identification request");

    assert_eq!(header.m_byProtocolVersion, 0xFF);
    assert!(header.m_payloadType.IsVehicleIdentificationRequest());
    assert_eq!(
        ReplyVersionFor(0xFF),
        c_byProtocolVersion2019,
        "but the answer uses this entity's real version, never the placeholder"
    );
}

#[test]
fn an_older_tester_is_answered_in_its_own_version() {
    assert_eq!(ReplyVersionFor(c_byProtocolVersion2012), 0x02);
    assert_eq!(ReplyVersionFor(c_byProtocolVersion2019), 0x03);
}

#[test]
fn both_conformant_lengths_of_each_optional_tail_are_accepted() {
    // Announcements are 32 or 33 bytes, routing activation requests 7 or 11, entity status
    // responses 3 or 7. Accepting only one of each pair is a real interoperability failure —
    // testers send both.
    for (u16Type, arrLengths) in [
        (0x0004u16, [32u32, 33]),
        (0x0005, [7, 11]),
        (0x0006, [9, 13]),
        (0x4002, [3, 7]),
    ] {
        for u32Length in arrLengths {
            assert!(
                ReadHeader(&Header(0x03, u16Type, u32Length), HeaderLimits::default()).is_ok(),
                "payload type 0x{u16Type:04X} must accept a {u32Length}-byte payload"
            );
        }
    }
}

#[test]
fn an_acknowledgement_swaps_the_addresses_of_the_message_it_acknowledges() {
    // Table 23, and the single most common bug in this payload: the acknowledgement's source
    // is the intended *receiver* of the acknowledged message and its target is its *sender*.
    // Echoing the original pair unchanged is directly visible to a conformance test.
    let request = DiagnosticMessage {
        m_u16SourceAddress: 0x0E80,
        m_u16TargetAddress: 0x1000,
        m_vecUserData: vec![0x10, 0x03],
    };

    let vecAck = BuildDiagnosticAck(&request, c_byAckRoutingConfirmation);

    assert_eq!(
        &vecAck[0..2],
        &[0x10, 0x00],
        "the acknowledgement comes FROM the ECU that was addressed"
    );
    assert_eq!(&vecAck[2..4], &[0x0E, 0x80], "and goes TO the tester");
    assert_eq!(vecAck[4], 0x00);
}

#[test]
fn only_an_invalid_source_address_closes_the_socket_among_the_diagnostic_nacks() {
    // REQ 7.DoIP-070 AL — and that rule lives only in the requirement text; Table 26 has no
    // required-action column at all, which is exactly why it gets missed.
    assert!(DiagnosticNack::InvalidSourceAddress.ClosesSocket());

    for nack in [
        DiagnosticNack::UnknownTargetAddress,
        DiagnosticNack::MessageTooLarge,
        DiagnosticNack::OutOfMemory,
        DiagnosticNack::TargetUnreachable,
    ] {
        assert!(
            !nack.ClosesSocket(),
            "NACK 0x{:02X} discards the message and keeps the connection",
            nack.Code()
        );
    }
}

#[test]
fn missing_authentication_is_the_only_routing_denial_that_keeps_the_socket() {
    // Table 49's required action for 0x04 is literally "do not activate routing and register" —
    // the entry stays so authentication can proceed on the same socket. Everything else closes.
    assert!(!RoutingActivationOutcome::DeniedMissingAuthentication.ClosesSocket());
    assert!(!RoutingActivationOutcome::Activated.ClosesSocket());

    for outcome in [
        RoutingActivationOutcome::DeniedUnknownSourceAddress,
        RoutingActivationOutcome::DeniedAllSocketsRegistered,
        RoutingActivationOutcome::DeniedSourceAddressMismatch,
        RoutingActivationOutcome::DeniedSourceAddressInUse,
        RoutingActivationOutcome::DeniedUnsupportedActivationType,
    ] {
        assert!(
            outcome.ClosesSocket(),
            "denial 0x{:02X} resets the connection",
            outcome.Code()
        );
    }
}

#[test]
fn a_routing_activation_request_reads_both_of_its_forms() {
    let vecShort = vec![0x0E, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00];
    let short = RoutingActivationRequest::FromBytes(&vecShort).expect("7 bytes is conformant");
    assert_eq!(short.m_u16SourceAddress, 0x0E80);
    assert_eq!(short.m_byActivationType, 0x00);
    assert_eq!(short.m_optU32ReservedOem, None);

    let mut vecLong = vecShort.clone();
    vecLong.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    let long = RoutingActivationRequest::FromBytes(&vecLong).expect("11 bytes is conformant too");
    assert_eq!(long.m_optU32ReservedOem, Some(0xDEAD_BEEF));
}

#[test]
fn an_announcement_carries_thirty_two_bytes_or_thirty_three_with_the_sync_status() {
    let announcement = VehicleAnnouncement {
        m_arrVin: *b"SIMUDSREFVIN0001X",
        m_u16LogicalAddress: 0x1000,
        m_arrEid: [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
        m_arrGid: [0x02, 0x00, 0x00, 0x00, 0x00, 0x02],
        m_byFurtherActionRequired: 0x00,
        m_optBySyncStatus: None,
    };
    assert_eq!(announcement.ToBytes().len(), 32);

    let withStatus = VehicleAnnouncement {
        m_optBySyncStatus: Some(0x00),
        ..announcement.clone()
    };
    let vecPayload = withStatus.ToBytes();
    assert_eq!(vecPayload.len(), 33);
    assert_eq!(&vecPayload[0..17], b"SIMUDSREFVIN0001X");
    assert_eq!(&vecPayload[17..19], &[0x10, 0x00]);
    assert_eq!(vecPayload[31], 0x00);
}

#[test]
fn a_diagnostic_message_round_trips_with_its_user_data_intact() {
    let message = DiagnosticMessage {
        m_u16SourceAddress: 0x0E80,
        m_u16TargetAddress: 0x1000,
        m_vecUserData: vec![0x22, 0xF1, 0x90],
    };

    let restored = DiagnosticMessage::FromBytes(&message.ToBytes()).expect("round trip");
    assert_eq!(restored, message);
}

#[test]
fn the_functional_address_range_is_recognised() {
    // Table 13 gives 0xE000-0xEFFF to functional group addresses. DoIP has no multicast: a
    // gateway that receives one of these is what broadcasts onto its sub-networks.
    assert!(IsFunctionalAddress(0xE000));
    assert!(IsFunctionalAddress(0xEFFF));
    assert!(!IsFunctionalAddress(0xDFFF));
    assert!(!IsFunctionalAddress(0xF000));
    assert!(!IsFunctionalAddress(0x1000));
}

#[test]
fn an_entity_status_response_reports_sockets_and_an_optional_data_size() {
    let status = EntityStatus {
        m_byNodeType: c_byNodeTypeGateway,
        m_byMaxSockets: 4,
        m_byOpenSockets: 1,
        m_optU32MaxDataSize: Some(4096),
    };
    let vecPayload = status.ToBytes();

    assert_eq!(vecPayload.len(), 7);
    assert_eq!(vecPayload[0], 0x00, "gateway");
    assert_eq!(vecPayload[1], 4);
    assert_eq!(vecPayload[2], 1);
    assert_eq!(&vecPayload[3..], &4096u32.to_be_bytes());
}

#[test]
fn a_generic_header_nack_is_itself_a_well_formed_doip_message() {
    // The NACK carries a generic header like everything else — a bare byte on the wire would
    // be unparseable by the tester that most needs to read it.
    let vecMessage = doip::BuildHeaderNack(
        0x03,
        HeaderNack::UnknownPayloadType {
            u16PayloadType: 0x1234,
        },
    );

    let header = ReadHeader(&vecMessage, HeaderLimits::default()).expect("valid message");
    assert_eq!(header.m_payloadType, PayloadType::GenericHeaderNack);
    assert_eq!(header.m_u32PayloadLength, 1);
    assert_eq!(vecMessage[8], 0x01);
}

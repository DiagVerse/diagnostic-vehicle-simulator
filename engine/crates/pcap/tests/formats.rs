//! Both container formats, and the failures that must be legible.
//!
//! Captures are built here in code rather than committed as binary fixtures: a reviewer can see
//! exactly what is being parsed, and the repo carries no opaque test data.

#![allow(non_snake_case, non_upper_case_globals)]

use pcap::ethernet::TransportKind;
use pcap::{PcapError, ReadCapture};

/// Build an Ethernet + IPv4 + UDP frame carrying `arrPayload`.
fn UdpFrame(arrPayload: &[u8]) -> Vec<u8> {
    let mut vecFrame = Vec::new();
    vecFrame.extend_from_slice(&[0x02; 6]); // destination MAC
    vecFrame.extend_from_slice(&[0x03; 6]); // source MAC
    vecFrame.extend_from_slice(&0x0800u16.to_be_bytes()); // IPv4

    let uUdpLength = 8 + arrPayload.len();
    let uTotalLength = 20 + uUdpLength;

    vecFrame.push(0x45); // version 4, header length 5 words
    vecFrame.push(0x00);
    vecFrame.extend_from_slice(&(uTotalLength as u16).to_be_bytes());
    vecFrame.extend_from_slice(&[0x00, 0x00]); // identification
    vecFrame.extend_from_slice(&[0x00, 0x00]); // flags and fragment offset
    vecFrame.push(64); // time to live
    vecFrame.push(17); // UDP
    vecFrame.extend_from_slice(&[0x00, 0x00]); // checksum, unchecked
    vecFrame.extend_from_slice(&[192, 168, 0, 10]);
    vecFrame.extend_from_slice(&[192, 168, 0, 2]);

    vecFrame.extend_from_slice(&49152u16.to_be_bytes());
    vecFrame.extend_from_slice(&13400u16.to_be_bytes());
    vecFrame.extend_from_slice(&(uUdpLength as u16).to_be_bytes());
    vecFrame.extend_from_slice(&[0x00, 0x00]); // checksum
    vecFrame.extend_from_slice(arrPayload);
    vecFrame
}

/// Build an Ethernet + IPv4 + TCP frame.
fn TcpFrame(arrPayload: &[u8], u32SequenceNumber: u32) -> Vec<u8> {
    let mut vecFrame = Vec::new();
    vecFrame.extend_from_slice(&[0x02; 6]);
    vecFrame.extend_from_slice(&[0x03; 6]);
    vecFrame.extend_from_slice(&0x0800u16.to_be_bytes());

    let uTotalLength = 20 + 20 + arrPayload.len();
    vecFrame.push(0x45);
    vecFrame.push(0x00);
    vecFrame.extend_from_slice(&(uTotalLength as u16).to_be_bytes());
    vecFrame.extend_from_slice(&[0x00, 0x00]);
    vecFrame.extend_from_slice(&[0x00, 0x00]);
    vecFrame.push(64);
    vecFrame.push(6); // TCP
    vecFrame.extend_from_slice(&[0x00, 0x00]);
    vecFrame.extend_from_slice(&[192, 168, 0, 10]);
    vecFrame.extend_from_slice(&[192, 168, 0, 2]);

    vecFrame.extend_from_slice(&51000u16.to_be_bytes());
    vecFrame.extend_from_slice(&13400u16.to_be_bytes());
    vecFrame.extend_from_slice(&u32SequenceNumber.to_be_bytes());
    vecFrame.extend_from_slice(&0u32.to_be_bytes()); // acknowledgement
    vecFrame.push(0x50); // data offset 5 words
    vecFrame.push(0x18); // flags
    vecFrame.extend_from_slice(&[0xFF, 0xFF]); // window
    vecFrame.extend_from_slice(&[0x00, 0x00]); // checksum
    vecFrame.extend_from_slice(&[0x00, 0x00]); // urgent pointer
    vecFrame.extend_from_slice(arrPayload);
    vecFrame
}

/// Wrap frames in a classic pcap file.
fn ClassicPcap(vecFrames: &[Vec<u8>], bIsLittleEndian: bool, u16LinkType: u16) -> Vec<u8> {
    let mut vecFile = Vec::new();
    let Pack32 = |u32Value: u32| -> [u8; 4] {
        if bIsLittleEndian {
            u32Value.to_le_bytes()
        } else {
            u32Value.to_be_bytes()
        }
    };

    // The magic is written in the file's own byte order, which is how a reader detects it.
    vecFile.extend_from_slice(&Pack32(0xA1B2_C3D4));
    vecFile.extend_from_slice(&if bIsLittleEndian {
        [2u8, 0, 4, 0]
    } else {
        [0u8, 2, 0, 4]
    });
    vecFile.extend_from_slice(&Pack32(0)); // timezone
    vecFile.extend_from_slice(&Pack32(0)); // significant figures
    vecFile.extend_from_slice(&Pack32(65535)); // snap length
    vecFile.extend_from_slice(&Pack32(u16LinkType as u32));

    for (uIndex, vecFrame) in vecFrames.iter().enumerate() {
        vecFile.extend_from_slice(&Pack32(100 + uIndex as u32)); // seconds
        vecFile.extend_from_slice(&Pack32(500_000)); // microseconds
        vecFile.extend_from_slice(&Pack32(vecFrame.len() as u32));
        vecFile.extend_from_slice(&Pack32(vecFrame.len() as u32));
        vecFile.extend_from_slice(vecFrame);
    }
    vecFile
}

/// Wrap frames in a pcapng file: section header, interface description, enhanced packets.
fn PcapNg(vecFrames: &[Vec<u8>]) -> Vec<u8> {
    let mut vecFile = Vec::new();

    // Section header: type, length, byte-order magic, version, section length, trailing length.
    vecFile.extend_from_slice(&0x0A0D_0D0Au32.to_le_bytes());
    vecFile.extend_from_slice(&28u32.to_le_bytes());
    vecFile.extend_from_slice(&0x1A2B_3C4Du32.to_le_bytes());
    vecFile.extend_from_slice(&[1, 0, 0, 0]); // version 1.0
    vecFile.extend_from_slice(&(-1i64).to_le_bytes()); // section length unknown
    vecFile.extend_from_slice(&28u32.to_le_bytes());

    // Interface description: Ethernet, snap length, no options.
    vecFile.extend_from_slice(&1u32.to_le_bytes());
    vecFile.extend_from_slice(&20u32.to_le_bytes());
    vecFile.extend_from_slice(&1u16.to_le_bytes()); // link type: Ethernet
    vecFile.extend_from_slice(&0u16.to_le_bytes()); // reserved
    vecFile.extend_from_slice(&65535u32.to_le_bytes());
    vecFile.extend_from_slice(&20u32.to_le_bytes());

    for vecFrame in vecFrames {
        // Enhanced packet blocks are padded to a multiple of four.
        let uPadded = vecFrame.len().div_ceil(4) * 4;
        let uBlockLength = 32 + uPadded;

        vecFile.extend_from_slice(&6u32.to_le_bytes());
        vecFile.extend_from_slice(&(uBlockLength as u32).to_le_bytes());
        vecFile.extend_from_slice(&0u32.to_le_bytes()); // interface id
        vecFile.extend_from_slice(&0u32.to_le_bytes()); // timestamp high
        vecFile.extend_from_slice(&1_000_000u32.to_le_bytes()); // timestamp low: one second
        vecFile.extend_from_slice(&(vecFrame.len() as u32).to_le_bytes());
        vecFile.extend_from_slice(&(vecFrame.len() as u32).to_le_bytes());
        vecFile.extend_from_slice(vecFrame);
        vecFile.extend(std::iter::repeat_n(0u8, uPadded - vecFrame.len()));
        vecFile.extend_from_slice(&(uBlockLength as u32).to_le_bytes());
    }
    vecFile
}

#[test]
fn a_classic_pcap_yields_its_udp_payloads() {
    let vecFile = ClassicPcap(&[UdpFrame(b"hello"), UdpFrame(b"again")], true, 1);
    let vecPackets = ReadCapture(&vecFile).expect("a valid capture");

    assert_eq!(vecPackets.len(), 2);
    assert_eq!(vecPackets[0].m_vecPayload, b"hello");
    assert_eq!(vecPackets[0].m_transport, TransportKind::Udp);
    assert_eq!(vecPackets[0].m_strSourceIp, "192.168.0.10");
    assert_eq!(vecPackets[0].m_strDestinationIp, "192.168.0.2");
    assert_eq!(vecPackets[0].m_u16DestinationPort, 13400);
    assert!(vecPackets[0].TouchesPort(13400));
}

#[test]
fn both_byte_orders_read_identically() {
    // The magic encodes byte order, and a capture written on a big-endian machine must read the
    // same as one written on a little-endian one.
    let vecLittle = ReadCapture(&ClassicPcap(&[UdpFrame(b"payload")], true, 1)).expect("little");
    let vecBig = ReadCapture(&ClassicPcap(&[UdpFrame(b"payload")], false, 1)).expect("big");

    assert_eq!(vecLittle.len(), 1);
    assert_eq!(vecLittle[0].m_vecPayload, vecBig[0].m_vecPayload);
    assert_eq!(vecLittle[0].m_f64TimestampSec, vecBig[0].m_f64TimestampSec);
}

#[test]
fn a_pcapng_yields_the_same_payloads_as_a_pcap() {
    // Wireshark writes pcapng by default, so the two formats must be interchangeable to a
    // caller — that is the whole point of supporting both.
    let vecFromNg = ReadCapture(&PcapNg(&[UdpFrame(b"hello")])).expect("a valid pcapng");
    let vecFromClassic = ReadCapture(&ClassicPcap(&[UdpFrame(b"hello")], true, 1)).expect("pcap");

    assert_eq!(vecFromNg.len(), 1);
    assert_eq!(vecFromNg[0].m_vecPayload, vecFromClassic[0].m_vecPayload);
    assert_eq!(vecFromNg[0].m_u16DestinationPort, 13400);
}

#[test]
fn a_tcp_segment_carries_its_sequence_number() {
    // Reassembly needs it: TCP is a stream, and only the sequence number says where a segment
    // belongs in it.
    let vecFile = ClassicPcap(
        &[TcpFrame(b"first", 1000), TcpFrame(b"second", 1005)],
        true,
        1,
    );
    let vecPackets = ReadCapture(&vecFile).expect("a valid capture");

    assert_eq!(vecPackets.len(), 2);
    assert_eq!(vecPackets[0].m_transport, TransportKind::Tcp);
    assert_eq!(vecPackets[0].m_u32SequenceNumber, 1000);
    assert_eq!(vecPackets[1].m_u32SequenceNumber, 1005);
    assert_eq!(vecPackets[0].FlowKey(), vecPackets[1].FlowKey());
}

#[test]
fn ethernet_padding_is_not_mistaken_for_payload() {
    // A frame shorter than 60 bytes is padded on the wire. The IP total-length field is what
    // bounds the real data; trusting the frame length would append rubbish to every short PDU.
    let mut vecFrame = UdpFrame(b"ab");
    vecFrame.extend(std::iter::repeat_n(0u8, 20));

    let vecPackets = ReadCapture(&ClassicPcap(&[vecFrame], true, 1)).expect("a valid capture");
    assert_eq!(
        vecPackets[0].m_vecPayload, b"ab",
        "the padding must not become payload"
    );
}

#[test]
fn a_vlan_tagged_frame_is_still_read() {
    // A tagged frame is an ordinary frame wearing a hat. Refusing to look under it would lose
    // every packet captured on a trunk port.
    let mut vecTagged = UdpFrame(b"tagged");
    // Splice a VLAN tag in after the MAC addresses.
    let mut vecWithTag = vecTagged[..12].to_vec();
    vecWithTag.extend_from_slice(&0x8100u16.to_be_bytes());
    vecWithTag.extend_from_slice(&0x0064u16.to_be_bytes()); // VLAN 100
    vecWithTag.extend_from_slice(&vecTagged.split_off(12));

    let vecPackets = ReadCapture(&ClassicPcap(&[vecWithTag], true, 1)).expect("a valid capture");
    assert_eq!(vecPackets.len(), 1);
    assert_eq!(vecPackets[0].m_vecPayload, b"tagged");
}

#[test]
fn non_ip_traffic_is_skipped_rather_than_refused() {
    // A real capture is full of ARP and everything else that was on the wire. None of it makes
    // the file unreadable.
    let mut vecArp = vec![0x02u8; 6];
    vecArp.extend_from_slice(&[0x03; 6]);
    vecArp.extend_from_slice(&0x0806u16.to_be_bytes()); // ARP
    vecArp.extend_from_slice(&[0x00; 28]);

    let vecFile = ClassicPcap(&[vecArp, UdpFrame(b"real")], true, 1);
    let vecPackets = ReadCapture(&vecFile).expect("the ARP frame must not break the file");

    assert_eq!(vecPackets.len(), 1);
    assert_eq!(vecPackets[0].m_vecPayload, b"real");
}

#[test]
fn an_unsupported_link_type_is_named_rather_than_silently_empty() {
    // A SocketCAN capture is a perfectly good file that simply is not Ethernet. Saying so is far
    // more use than handing back nothing and leaving someone to work out why.
    let vecFile = ClassicPcap(&[UdpFrame(b"x")], true, 227);

    match ReadCapture(&vecFile) {
        Err(PcapError::UnsupportedLinkType {
            u16LinkType,
            strName,
        }) => {
            assert_eq!(u16LinkType, 227);
            assert_eq!(strName, "SocketCAN");
        }
        other => panic!("expected a named link-type error, got {other:?}"),
    }
}

#[test]
fn something_that_is_not_a_capture_says_so_with_its_leading_bytes() {
    match ReadCapture(b"this is a text file, not a capture") {
        Err(PcapError::NotACapture { strLeadingBytes }) => {
            assert!(!strLeadingBytes.is_empty(), "the bytes help identify it");
        }
        other => panic!("expected NotACapture, got {other:?}"),
    }
}

#[test]
fn a_file_truncated_mid_packet_keeps_what_was_read() {
    // A capture tool killed while writing is common, and everything before the cut is still
    // perfectly good data. Throwing it away would be the wrong trade.
    let mut vecFile = ClassicPcap(&[UdpFrame(b"complete"), UdpFrame(b"cut short")], true, 1);
    vecFile.truncate(vecFile.len() - 12);

    let vecPackets = ReadCapture(&vecFile).expect("the readable part should survive");
    assert_eq!(vecPackets.len(), 1);
    assert_eq!(vecPackets[0].m_vecPayload, b"complete");
}

#[test]
fn a_header_shorter_than_the_file_header_is_refused() {
    match ReadCapture(&[0xD4, 0xC3, 0xB2, 0xA1, 0x02, 0x00]) {
        Err(PcapError::Truncated { .. }) => {}
        other => panic!("expected Truncated, got {other:?}"),
    }
}

#[test]
fn a_capture_with_no_packets_is_empty_rather_than_an_error() {
    // An empty capture is a real thing — the tool ran and saw nothing. That is a fact to report,
    // not a malformed file.
    let vecPackets = ReadCapture(&ClassicPcap(&[], true, 1)).expect("a valid, empty capture");
    assert!(vecPackets.is_empty());
}

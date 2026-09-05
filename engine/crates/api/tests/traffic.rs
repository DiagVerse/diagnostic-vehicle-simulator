//! The live traffic feed carries what a monitor needs, including the parts that are silence.

#![allow(non_snake_case, non_upper_case_globals)]

use api::traffic::{TrafficChannel, TrafficEvent};
use bridge::observer::{FrameDirection, FrameObserver};
use can::CanFrame;

#[test]
fn a_frame_crossing_the_bridge_reaches_a_monitor() {
    let channel = TrafficChannel::New();
    let mut receiver = channel.Subscribe();

    let frame = CanFrame::NewClassic(0.0, 0x7E0, vec![0x02, 0x10, 0x03]);
    channel.OnFrame(FrameDirection::Received, &frame);

    match receiver.try_recv().expect("an event should be waiting") {
        TrafficEvent::Frame {
            direction,
            can_id_hex,
            data_hex,
            length,
            is_flow_control,
            ..
        } => {
            assert_eq!(direction, "rx");
            assert_eq!(can_id_hex, "7E0");
            assert_eq!(data_hex, "02 10 03");
            assert_eq!(length, 3);
            assert!(!is_flow_control);
        }
        other => panic!("expected a frame event, got {other:?}"),
    }
}

#[test]
fn a_flow_control_frame_is_marked_as_one() {
    // Flow control is the tester talking back mid-transfer. A monitor that renders it the same
    // as a data frame makes a segmented exchange much harder to read.
    let channel = TrafficChannel::New();
    let mut receiver = channel.Subscribe();

    let frame = CanFrame::NewClassic(0.0, 0x7E0, vec![0x30, 0x00, 0x00]);
    channel.OnFrame(FrameDirection::Sent, &frame);

    match receiver.try_recv().expect("an event should be waiting") {
        TrafficEvent::Frame {
            direction,
            is_flow_control,
            ..
        } => {
            assert_eq!(direction, "tx");
            assert!(is_flow_control);
        }
        other => panic!("expected a frame event, got {other:?}"),
    }
}

#[test]
fn a_29_bit_identifier_is_not_squashed_into_three_digits() {
    let channel = TrafficChannel::New();
    let mut receiver = channel.Subscribe();

    let frame = CanFrame::NewClassic(0.0, 0x18DAF110, vec![0x30]);
    channel.OnFrame(FrameDirection::Received, &frame);

    match receiver.try_recv().expect("an event should be waiting") {
        TrafficEvent::Frame { can_id_hex, .. } => assert_eq!(can_id_hex, "18DAF110"),
        other => panic!("expected a frame event, got {other:?}"),
    }
}

#[test]
fn publishing_with_nobody_listening_is_not_an_error() {
    // The normal case: an engine nobody is watching must not treat that as a failure, or every
    // request would log a spurious error.
    let channel = TrafficChannel::New();
    assert_eq!(channel.SubscriberCount(), 0);

    channel.OnFrame(
        FrameDirection::Sent,
        &CanFrame::NewClassic(0.0, 0x7E8, vec![0x02, 0x50, 0x03]),
    );
}

#[test]
fn events_serialize_with_a_kind_a_reader_can_switch_on() {
    // The monitor picks its renderer from `kind` rather than guessing from which fields are
    // present, so the tag has to actually be there.
    let event = TrafficEvent::Lagged {
        at_ms: 1_700_000_000_000,
        missed: 412,
    };
    let strJson = serde_json::to_string(&event).expect("an event serializes");

    assert!(strJson.contains(r#""kind":"lagged""#), "got {strJson}");
    assert!(strJson.contains(r#""missed":412"#), "got {strJson}");
    assert!(strJson.contains(r#""atMs":1700000000000"#), "got {strJson}");
}

#[test]
fn a_hardware_exchange_carries_the_decoded_request_not_only_the_frames() {
    // The point of OnExchange. A monitor showing only `02 10 03` would be asking the reader to
    // reassemble ISO-TP and decode UDS by eye, when the bridge has already done both.
    use simulation::{RoutedResponse, RoutingOutcome};

    let channel = TrafficChannel::New();
    let mut receiver = channel.Subscribe();

    let outcome = RoutingOutcome::Handled(vec![RoutedResponse {
        m_strEcuName: "Engine".to_string(),
        m_u32RequestCanId: 0x7E0,
        m_u32ResponseCanId: 0x7E8,
        m_vecResponse: vec![0x50, 0x03],
        m_bySession: 0x03,
        m_bIsSecurityUnlocked: false,
        m_plan: Default::default(),
    }]);
    channel.OnExchange(0x7E0, &[0x10, 0x03], &outcome);

    match receiver.try_recv().expect("an event should be waiting") {
        TrafficEvent::Exchange {
            can_id_hex,
            request_hex,
            addressing,
            routed,
            responses,
            ..
        } => {
            assert_eq!(can_id_hex, "7E0");
            assert_eq!(request_hex, "10 03");
            assert_eq!(addressing, "physical");
            assert!(routed);
            assert_eq!(responses.len(), 1);
            assert_eq!(responses[0].ecu_name, "Engine");
            assert_eq!(responses[0].response_hex, "50 03");
        }
        other => panic!("expected an exchange event, got {other:?}"),
    }
}

#[test]
fn silence_on_the_wire_is_reported_with_its_reason() {
    // Three different problems that look identical on a bus. A monitor that showed them the
    // same way would send someone hunting the wrong one.
    use simulation::RoutingOutcome;

    let channel = TrafficChannel::New();
    let mut receiver = channel.Subscribe();

    let arrCases = [
        (RoutingOutcome::Stopped, "stopped"),
        (RoutingOutcome::NoTarget, "unrouted"),
        (
            RoutingOutcome::Silenced {
                strEcuName: "Engine".to_string(),
                strReason: "'Engine' is switched off".to_string(),
            },
            "silenced",
        ),
    ];

    for (outcome, strExpected) in arrCases {
        channel.OnExchange(0x7E0, &[0x22, 0xF1, 0x90], &outcome);

        match receiver.try_recv().expect("an event should be waiting") {
            TrafficEvent::Exchange {
                addressing,
                routed,
                reason,
                ..
            } => {
                assert_eq!(addressing, strExpected);
                assert!(!routed);
                assert!(
                    reason.is_some_and(|strReason| !strReason.is_empty()),
                    "{strExpected} must explain itself"
                );
            }
            other => panic!("expected an exchange event, got {other:?}"),
        }
    }
}

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

// ==========================================================================================
// History. A monitor is opened because something looked wrong, which is necessarily after it
// happened — so a feed that starts blank always omits the thing the person came to look at.
// ==========================================================================================

#[test]
fn a_monitor_attaching_later_is_given_what_it_missed() {
    let channel = TrafficChannel::New();

    for u32CanId in [0x7E0u32, 0x7E1, 0x7E2] {
        channel.OnFrame(
            FrameDirection::Received,
            &CanFrame::NewClassic(0.0, u32CanId, vec![0x02, 0x10, 0x03]),
        );
    }

    // Nobody was listening for any of that.
    let (vecHistory, u64Dropped, _receiver) = channel.SubscribeWithHistory();

    assert_eq!(vecHistory.len(), 3);
    assert_eq!(u64Dropped, 0);
    match &vecHistory[0] {
        TrafficEvent::Frame { can_id_hex, .. } => assert_eq!(can_id_hex, "7E0"),
        other => panic!("history should be in order, got {other:?}"),
    }
}

#[test]
fn the_replay_and_the_live_feed_neither_overlap_nor_leave_a_gap() {
    // The reason `Publish` holds the history lock across the broadcast send. Snapshotting and
    // subscribing separately leaves a window where an event is either seen twice or not at
    // all, and a monitor that quietly drops or duplicates around its own attach point is worse
    // than one that shows nothing.
    let channel = TrafficChannel::New();

    channel.OnFrame(
        FrameDirection::Received,
        &CanFrame::NewClassic(0.0, 0x111, vec![0x01]),
    );

    let (vecHistory, _dropped, mut receiver) = channel.SubscribeWithHistory();

    channel.OnFrame(
        FrameDirection::Received,
        &CanFrame::NewClassic(0.0, 0x222, vec![0x02]),
    );

    assert_eq!(vecHistory.len(), 1, "only what happened before attaching");
    match &vecHistory[0] {
        TrafficEvent::Frame { can_id_hex, .. } => assert_eq!(can_id_hex, "111"),
        other => panic!("expected a frame, got {other:?}"),
    }

    match receiver.try_recv().expect("the later event should be live") {
        TrafficEvent::Frame { can_id_hex, .. } => assert_eq!(can_id_hex, "222"),
        other => panic!("expected a frame, got {other:?}"),
    }
    assert!(
        receiver.try_recv().is_err(),
        "the replayed event must not also arrive live"
    );
}

#[test]
fn the_history_is_a_ring_that_reports_what_it_dropped() {
    // Bounded on purpose. What matters is that the boundedness is *visible*: a monitor shown a
    // partial replay with no indication would read it as the whole session.
    let channel = TrafficChannel::New();

    // One more than the ring holds, so exactly one falls out of the front.
    for uIndex in 0..20_001u32 {
        channel.OnFrame(
            FrameDirection::Received,
            &CanFrame::NewClassic(0.0, uIndex % 0x7FF, vec![0x01]),
        );
    }

    let (vecHistory, u64Dropped, _receiver) = channel.SubscribeWithHistory();
    assert_eq!(vecHistory.len(), 20_000, "the ring holds its capacity");
    assert_eq!(u64Dropped, 1, "and says what it pushed out");
    assert_eq!(channel.HistoryLength(), 20_000);
}

#[test]
fn a_replay_summary_says_how_much_was_lost_before_the_monitor_attached() {
    let event = TrafficEvent::Replayed {
        at_ms: 1_700_000_000_000,
        count: 20_000,
        dropped_before: 412,
    };
    let strJson = serde_json::to_string(&event).expect("an event serializes");

    assert!(strJson.contains(r#""kind":"replayed""#), "got {strJson}");
    assert!(strJson.contains(r#""count":20000"#), "got {strJson}");
    assert!(strJson.contains(r#""droppedBefore":412"#), "got {strJson}");
}

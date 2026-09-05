//! ISO-TP transmit: turning one PDU into the frames that carry it.
//!
//! Until now this crate only observed finished exchanges. Transmitting makes the engine a
//! participant, which brings an obligation the observer never had: the peer decides how fast
//! it will accept the data, and the transmitter must obey.
//!
//! Written as an explicit state machine driven by events rather than as a linear `async fn`,
//! for two reasons. Each stage is individually visible, which is what the project's diagnostic
//! flow rules ask for. And nothing here touches a clock — the caller owns the waiting — so the
//! whole of the flow-control protocol is testable without one.

#![allow(non_snake_case, non_upper_case_globals)]

use std::time::Duration;

use crate::params::{
    c_u8MaxWaitFrames, c_uConsecutiveFramePayload, c_uFirstFramePayload, c_uMaxClassicPduLength,
    c_uMaxSingleFrameLength, ApplyPadding, DecodeSeparationTime, IsoTpParameters,
};

/// FlowStatus values a peer can send.
const c_byFlowStatusContinue: u8 = 0x00;
const c_byFlowStatusWait: u8 = 0x01;
const c_byFlowStatusOverflow: u8 = 0x02;

/// PCI type nibbles.
const c_byPciSingleFrame: u8 = 0x00;
const c_byPciFirstFrame: u8 = 0x10;
const c_byPciConsecutiveFrame: u8 = 0x20;

/// Why a transfer could not be completed.
///
/// These are **link** failures, not diagnostic ones. A tester that stalls has not made the
/// ECU's response non-conformant, so these must never be reported as ISO 14229 timing
/// violations — they are reported alongside, in their own type, for exactly that reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IsoTpTransportError {
    /// The PDU is longer than a classic FirstFrame can announce.
    #[error("a {uLength}-byte PDU exceeds the {c_uMaxClassicPduLength} bytes a classic ISO-TP first frame can announce")]
    PduTooLong { uLength: usize },

    /// Nothing to send.
    #[error("an empty PDU cannot be transmitted")]
    EmptyPdu,

    /// The peer never asked for the rest.
    #[error("no flow control arrived within the timeout after {uFramesSent} frame(s); the transfer was abandoned")]
    FlowControlTimeout { uFramesSent: usize },

    /// The peer asked to wait too many times running.
    #[error("the receiver asked to wait {u8WaitCount} times in a row, more than the {c_u8MaxWaitFrames} allowed")]
    TooManyWaitFrames { u8WaitCount: u8 },

    /// The peer cannot hold the message.
    #[error("the receiver reported its buffer would overflow, so the transfer was abandoned")]
    ReceiverOverflow,

    /// The peer sent a flow status that means nothing.
    #[error("the receiver sent flow status 0x{byFlowStatus:X}, which is reserved")]
    InvalidFlowStatus { byFlowStatus: u8 },
}

/// Where a transfer has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransmitState {
    /// Nothing in flight.
    Idle,
    /// A first frame or a block has gone out; the peer owes a flow control.
    AwaitingFlowControl,
    /// Cleared to send; consecutive frames may go out, spaced by the separation time.
    SendingBlock,
    /// Every byte has been handed over.
    Complete,
    /// The link failed.
    Aborted(IsoTpTransportError),
}

/// Sends one PDU as ISO-TP frames, obeying whatever the peer asks for.
pub struct IsoTpTransmitter {
    /// What this end pads with. The peer's block size and separation time arrive in its flow
    /// control and are re-read from every one — they are not sticky across blocks.
    m_params: IsoTpParameters,
    m_vecPdu: Vec<u8>,
    m_uSentBytes: usize,
    m_u8SequenceNumber: u8,
    /// Frames still allowed in this block; `None` when the peer set no limit.
    m_optU8RemainingInBlock: Option<u8>,
    m_separationTime: Duration,
    m_u8WaitCount: u8,
    m_uFramesSent: usize,
    m_state: TransmitState,
}

impl IsoTpTransmitter {
    /// Create a transmitter that pads the way `params` says.
    pub fn New(params: IsoTpParameters) -> Self {
        IsoTpTransmitter {
            m_params: params,
            m_vecPdu: Vec::new(),
            m_uSentBytes: 0,
            m_u8SequenceNumber: 1,
            m_optU8RemainingInBlock: None,
            m_separationTime: Duration::ZERO,
            m_u8WaitCount: 0,
            m_uFramesSent: 0,
            m_state: TransmitState::Idle,
        }
    }

    /// Where the transfer has got to.
    pub fn State(&self) -> &TransmitState {
        &self.m_state
    }

    /// How long to wait before the next consecutive frame.
    pub fn SeparationTime(&self) -> Duration {
        self.m_separationTime
    }

    /// Start sending a PDU, returning the first frame to put on the wire.
    ///
    /// A short PDU is a single frame and the transfer is done. A longer one is a first frame,
    /// after which the peer owes a flow control before anything else may be sent.
    pub fn Begin(&mut self, vecPdu: &[u8]) -> Result<Vec<u8>, IsoTpTransportError> {
        if vecPdu.is_empty() {
            return Err(IsoTpTransportError::EmptyPdu);
        }
        if vecPdu.len() > c_uMaxClassicPduLength {
            return Err(IsoTpTransportError::PduTooLong {
                uLength: vecPdu.len(),
            });
        }

        self.m_vecPdu = vecPdu.to_vec();
        self.m_uSentBytes = 0;
        self.m_u8SequenceNumber = 1;
        self.m_u8WaitCount = 0;
        self.m_uFramesSent = 1;

        if vecPdu.len() <= c_uMaxSingleFrameLength {
            self.m_state = TransmitState::Complete;
            self.m_uSentBytes = vecPdu.len();
            return Ok(self.BuildSingleFrame());
        }

        self.m_state = TransmitState::AwaitingFlowControl;
        Ok(self.BuildFirstFrame())
    }

    /// Take a flow-control frame from the peer.
    ///
    /// Its block size and separation time apply to the block it authorises only; they are
    /// re-read from every one rather than remembered.
    pub fn OnFlowControl(&mut self, vecFrame: &[u8]) -> Result<(), IsoTpTransportError> {
        if self.m_state != TransmitState::AwaitingFlowControl {
            // A stray flow control with nothing in flight. Ignoring it is deliberate: aborting
            // an unrelated transfer because of one would be worse than doing nothing.
            tracing::debug!("ignoring a flow control frame with no transfer awaiting one");
            return Ok(());
        }
        if vecFrame.len() < 3 {
            tracing::warn!(
                len = vecFrame.len(),
                "ignoring a malformed flow control frame"
            );
            return Ok(());
        }

        let byFlowStatus = vecFrame[0] & 0x0F;
        match byFlowStatus {
            c_byFlowStatusContinue => self.OnContinueToSend(vecFrame[1], vecFrame[2]),
            c_byFlowStatusWait => self.OnWait(),
            c_byFlowStatusOverflow => self.Abort(IsoTpTransportError::ReceiverOverflow),
            byReserved => self.Abort(IsoTpTransportError::InvalidFlowStatus {
                byFlowStatus: byReserved,
            }),
        }
    }

    /// The peer cleared this end to send a block.
    fn OnContinueToSend(
        &mut self,
        u8BlockSize: u8,
        bySeparationTime: u8,
    ) -> Result<(), IsoTpTransportError> {
        self.m_u8WaitCount = 0;
        self.m_optU8RemainingInBlock = if u8BlockSize == 0 {
            None
        } else {
            Some(u8BlockSize)
        };
        self.m_separationTime = DecodeSeparationTime(bySeparationTime);
        self.m_state = TransmitState::SendingBlock;

        tracing::debug!(
            blockSize = u8BlockSize,
            separationTimeMs = self.m_separationTime.as_millis(),
            "cleared to send a block"
        );
        Ok(())
    }

    /// The peer asked for more time.
    fn OnWait(&mut self) -> Result<(), IsoTpTransportError> {
        self.m_u8WaitCount += 1;
        if self.m_u8WaitCount > c_u8MaxWaitFrames {
            return self.Abort(IsoTpTransportError::TooManyWaitFrames {
                u8WaitCount: self.m_u8WaitCount,
            });
        }

        tracing::debug!(
            waits = self.m_u8WaitCount,
            "the receiver asked to wait; the flow-control timeout restarts"
        );
        Ok(())
    }

    /// The flow control never came.
    pub fn OnFlowControlTimeout(&mut self) -> Result<(), IsoTpTransportError> {
        self.Abort(IsoTpTransportError::FlowControlTimeout {
            uFramesSent: self.m_uFramesSent,
        })
    }

    /// The next consecutive frame, if this end is allowed to send one now.
    ///
    /// `None` means either the transfer is finished or the block is used up and the peer owes
    /// another flow control — [`State`](Self::State) says which.
    pub fn NextConsecutiveFrame(&mut self) -> Option<Vec<u8>> {
        if self.m_state != TransmitState::SendingBlock {
            return None;
        }
        if self.m_uSentBytes >= self.m_vecPdu.len() {
            self.m_state = TransmitState::Complete;
            return None;
        }

        if let Some(u8Remaining) = self.m_optU8RemainingInBlock {
            if u8Remaining == 0 {
                // The block is used up; the peer owes another flow control before any more.
                self.m_state = TransmitState::AwaitingFlowControl;
                return None;
            }
            self.m_optU8RemainingInBlock = Some(u8Remaining - 1);
        }

        let vecFrame = self.BuildConsecutiveFrame();
        self.m_uFramesSent += 1;

        if self.m_uSentBytes >= self.m_vecPdu.len() {
            self.m_state = TransmitState::Complete;
        }
        Some(vecFrame)
    }

    /// Stop the transfer and remember why.
    fn Abort(&mut self, error: IsoTpTransportError) -> Result<(), IsoTpTransportError> {
        tracing::warn!(%error, framesSent = self.m_uFramesSent, "ISO-TP transfer abandoned");
        self.m_state = TransmitState::Aborted(error.clone());
        Err(error)
    }

    /// A whole PDU in one frame: the length in the low nibble, then the bytes.
    fn BuildSingleFrame(&self) -> Vec<u8> {
        let mut vecFrame = vec![c_byPciSingleFrame | (self.m_vecPdu.len() as u8)];
        vecFrame.extend_from_slice(&self.m_vecPdu);
        ApplyPadding(&mut vecFrame, self.m_params.m_optByPaddingByte);
        vecFrame
    }

    /// The opening frame of a long PDU: a 12-bit total length, then the first six bytes.
    fn BuildFirstFrame(&mut self) -> Vec<u8> {
        let uTotalLength = self.m_vecPdu.len();
        let mut vecFrame = vec![
            c_byPciFirstFrame | ((uTotalLength >> 8) as u8 & 0x0F),
            (uTotalLength & 0xFF) as u8,
        ];
        vecFrame.extend_from_slice(&self.m_vecPdu[..c_uFirstFramePayload]);
        self.m_uSentBytes = c_uFirstFramePayload;
        // A first frame is full by construction, so it never needs padding.
        vecFrame
    }

    /// One more slice of the PDU, numbered so the peer can spot a gap.
    fn BuildConsecutiveFrame(&mut self) -> Vec<u8> {
        let mut vecFrame = vec![c_byPciConsecutiveFrame | (self.m_u8SequenceNumber & 0x0F)];

        let uRemaining = self.m_vecPdu.len() - self.m_uSentBytes;
        let uTake = uRemaining.min(c_uConsecutiveFramePayload);
        vecFrame.extend_from_slice(&self.m_vecPdu[self.m_uSentBytes..self.m_uSentBytes + uTake]);
        self.m_uSentBytes += uTake;

        // The sequence number wraps at 15 and is not reset at a block boundary.
        self.m_u8SequenceNumber = self.m_u8SequenceNumber.wrapping_add(1) & 0x0F;

        // Only the last consecutive frame can be short.
        ApplyPadding(&mut vecFrame, self.m_params.m_optByPaddingByte);
        vecFrame
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::c_byDefaultPaddingByte;

    /// Drive a whole transfer with a peer that imposes no limits, collecting every frame.
    fn SendWithOpenFlowControl(vecPdu: &[u8]) -> Vec<Vec<u8>> {
        let mut transmitter = IsoTpTransmitter::New(IsoTpParameters::default());
        let mut vecFrames = vec![transmitter.Begin(vecPdu).expect("a sendable PDU")];

        if *transmitter.State() == TransmitState::Complete {
            return vecFrames;
        }

        transmitter
            .OnFlowControl(&[0x30, 0x00, 0x00])
            .expect("clear to send");
        while let Some(vecFrame) = transmitter.NextConsecutiveFrame() {
            vecFrames.push(vecFrame);
        }
        vecFrames
    }

    #[test]
    fn a_short_pdu_is_one_padded_frame() {
        let vecFrames = SendWithOpenFlowControl(&[0x50, 0x03, 0x00, 0x32, 0x01, 0xF4]);

        assert_eq!(vecFrames.len(), 1);
        assert_eq!(
            vecFrames[0],
            vec![
                0x06,
                0x50,
                0x03,
                0x00,
                0x32,
                0x01,
                0xF4,
                c_byDefaultPaddingByte
            ]
        );
    }

    #[test]
    fn padding_can_be_turned_off() {
        let mut transmitter = IsoTpTransmitter::New(IsoTpParameters {
            m_optByPaddingByte: None,
            ..IsoTpParameters::default()
        });
        let vecFrame = transmitter.Begin(&[0x50, 0x03]).expect("a sendable PDU");
        assert_eq!(vecFrame, vec![0x02, 0x50, 0x03], "sent short, not padded");
    }

    #[test]
    fn a_vin_response_becomes_a_first_frame_and_two_consecutive_frames() {
        // 62 F1 90 plus a 17-character VIN is 20 bytes: 6 in the first frame, 7 in each of two
        // consecutive frames.
        let mut vecPdu = vec![0x62, 0xF1, 0x90];
        vecPdu.extend_from_slice(b"1HGCM82633A004352");

        let vecFrames = SendWithOpenFlowControl(&vecPdu);

        assert_eq!(vecFrames.len(), 3);
        assert_eq!(
            vecFrames[0],
            vec![0x10, 0x14, 0x62, 0xF1, 0x90, b'1', b'H', b'G'],
            "first frame announces 0x014 = 20 bytes"
        );
        assert_eq!(
            vecFrames[1],
            vec![0x21, b'C', b'M', b'8', b'2', b'6', b'3', b'3']
        );
        assert_eq!(
            vecFrames[2],
            vec![0x22, b'A', b'0', b'0', b'4', b'3', b'5', b'2']
        );
    }

    #[test]
    fn only_the_last_consecutive_frame_is_padded() {
        // A 19-byte PDU leaves the final frame one byte short.
        let mut vecPdu = vec![0x62, 0xF1, 0x90];
        vecPdu.extend_from_slice(b"1HGCM82633A00435");

        let vecFrames = SendWithOpenFlowControl(&vecPdu);

        assert_eq!(vecFrames.len(), 3);
        assert_eq!(
            vecFrames[1].len(),
            8,
            "a full consecutive frame needs no padding"
        );
        assert_eq!(
            vecFrames[2],
            vec![
                0x22,
                b'A',
                b'0',
                b'0',
                b'4',
                b'3',
                b'5',
                c_byDefaultPaddingByte
            ]
        );
    }

    #[test]
    fn nothing_is_sent_before_the_peer_asks_for_it() {
        let mut vecPdu = vec![0x62, 0xF1, 0x90];
        vecPdu.extend_from_slice(b"1HGCM82633A004352");

        let mut transmitter = IsoTpTransmitter::New(IsoTpParameters::default());
        transmitter.Begin(&vecPdu).expect("a sendable PDU");

        assert_eq!(*transmitter.State(), TransmitState::AwaitingFlowControl);
        assert!(
            transmitter.NextConsecutiveFrame().is_none(),
            "a consecutive frame before the flow control would be a protocol violation"
        );
    }

    #[test]
    fn a_block_size_stops_the_transfer_until_the_next_flow_control() {
        // 34 bytes: 6 in the first frame, then 28 across four consecutive frames.
        let mut vecPdu = vec![0x62, 0xF1, 0x8C];
        vecPdu.extend((1..=31u8).collect::<Vec<u8>>());
        assert_eq!(vecPdu.len(), 34);

        let mut transmitter = IsoTpTransmitter::New(IsoTpParameters::default());
        transmitter.Begin(&vecPdu).expect("a sendable PDU");

        // Two frames per block.
        transmitter.OnFlowControl(&[0x30, 0x02, 0x00]).expect("cts");
        assert_eq!(transmitter.NextConsecutiveFrame().unwrap()[0], 0x21);
        assert_eq!(transmitter.NextConsecutiveFrame().unwrap()[0], 0x22);
        assert!(
            transmitter.NextConsecutiveFrame().is_none(),
            "the block is used up"
        );
        assert_eq!(*transmitter.State(), TransmitState::AwaitingFlowControl);

        // The next flow control's values replace the first's rather than being remembered.
        transmitter.OnFlowControl(&[0x30, 0x00, 0x14]).expect("cts");
        assert_eq!(transmitter.SeparationTime().as_millis(), 20);
        assert_eq!(transmitter.NextConsecutiveFrame().unwrap()[0], 0x23);
        assert_eq!(transmitter.NextConsecutiveFrame().unwrap()[0], 0x24);
        assert!(transmitter.NextConsecutiveFrame().is_none());
        assert_eq!(*transmitter.State(), TransmitState::Complete);
    }

    #[test]
    fn the_sequence_number_wraps_at_fifteen_and_ignores_block_boundaries() {
        // Long enough to need more than sixteen consecutive frames.
        let vecPdu: Vec<u8> = (0..130u8).collect();
        let vecFrames = SendWithOpenFlowControl(&vecPdu);

        let vecSequenceNumbers: Vec<u8> = vecFrames[1..]
            .iter()
            .map(|vecFrame| vecFrame[0] & 0x0F)
            .collect();

        assert_eq!(&vecSequenceNumbers[0..3], &[1, 2, 3]);
        // ...through 15, then round to 0 rather than back to 1.
        assert_eq!(&vecSequenceNumbers[14..17], &[15, 0, 1]);
    }

    #[test]
    fn a_wait_pauses_the_transfer_without_ending_it() {
        let vecPdu: Vec<u8> = (0..20u8).collect();
        let mut transmitter = IsoTpTransmitter::New(IsoTpParameters::default());
        transmitter.Begin(&vecPdu).expect("a sendable PDU");

        transmitter
            .OnFlowControl(&[0x31, 0x00, 0x00])
            .expect("wait");
        assert_eq!(*transmitter.State(), TransmitState::AwaitingFlowControl);
        assert!(transmitter.NextConsecutiveFrame().is_none());

        transmitter.OnFlowControl(&[0x30, 0x00, 0x00]).expect("cts");
        assert!(transmitter.NextConsecutiveFrame().is_some());
    }

    #[test]
    fn too_many_waits_in_a_row_abandon_the_transfer() {
        let vecPdu: Vec<u8> = (0..20u8).collect();
        let mut transmitter = IsoTpTransmitter::New(IsoTpParameters::default());
        transmitter.Begin(&vecPdu).expect("a sendable PDU");

        for _ in 0..c_u8MaxWaitFrames {
            transmitter
                .OnFlowControl(&[0x31, 0x00, 0x00])
                .expect("wait");
        }
        let resError = transmitter.OnFlowControl(&[0x31, 0x00, 0x00]);

        assert!(matches!(
            resError,
            Err(IsoTpTransportError::TooManyWaitFrames { .. })
        ));
        assert!(transmitter.NextConsecutiveFrame().is_none());
    }

    #[test]
    fn an_overflow_report_abandons_the_transfer_without_an_error_on_the_wire() {
        let vecPdu: Vec<u8> = (0..20u8).collect();
        let mut transmitter = IsoTpTransmitter::New(IsoTpParameters::default());
        transmitter.Begin(&vecPdu).expect("a sendable PDU");

        assert_eq!(
            transmitter.OnFlowControl(&[0x32, 0x00, 0x00]),
            Err(IsoTpTransportError::ReceiverOverflow)
        );
        // The receiver declared its own limit. Nothing further goes out, and no negative
        // response is invented — a transport failure has no application error path.
        assert!(transmitter.NextConsecutiveFrame().is_none());
    }

    #[test]
    fn a_reserved_flow_status_abandons_the_transfer() {
        let vecPdu: Vec<u8> = (0..20u8).collect();
        let mut transmitter = IsoTpTransmitter::New(IsoTpParameters::default());
        transmitter.Begin(&vecPdu).expect("a sendable PDU");

        assert!(matches!(
            transmitter.OnFlowControl(&[0x35, 0x00, 0x00]),
            Err(IsoTpTransportError::InvalidFlowStatus { byFlowStatus: 5 })
        ));
    }

    #[test]
    fn a_flow_control_that_never_arrives_abandons_the_transfer_and_sends_nothing_more() {
        let vecPdu: Vec<u8> = (0..20u8).collect();
        let mut transmitter = IsoTpTransmitter::New(IsoTpParameters::default());
        transmitter.Begin(&vecPdu).expect("a sendable PDU");

        assert!(matches!(
            transmitter.OnFlowControlTimeout(),
            Err(IsoTpTransportError::FlowControlTimeout { uFramesSent: 1 })
        ));
        // No retry: ISO-TP has none, and the peer's receive state machine is already lost.
        assert!(transmitter.NextConsecutiveFrame().is_none());
    }

    #[test]
    fn a_stray_flow_control_is_ignored_rather_than_wrecking_the_transfer() {
        let mut transmitter = IsoTpTransmitter::New(IsoTpParameters::default());
        assert_eq!(transmitter.OnFlowControl(&[0x30, 0x00, 0x00]), Ok(()));
        assert_eq!(*transmitter.State(), TransmitState::Idle);
    }

    #[test]
    fn a_pdu_too_long_for_a_classic_first_frame_is_refused() {
        let vecPdu = vec![0x00; c_uMaxClassicPduLength + 1];
        let mut transmitter = IsoTpTransmitter::New(IsoTpParameters::default());

        assert!(matches!(
            transmitter.Begin(&vecPdu),
            Err(IsoTpTransportError::PduTooLong { .. })
        ));
    }
}

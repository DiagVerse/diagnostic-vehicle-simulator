//! ISO-TP receive as a bus participant.
//!
//! Distinct from [`crate::ReassembleStream`], and deliberately so. That one reads a finished
//! exchange after the fact and must never generate anything; this one is half of a live
//! conversation and has to answer — a multi-frame request goes nowhere until this end sends
//! flow control.
//!
//! Like the transmit side, no clock: the caller owns the timeouts, so every rule here is
//! testable without one.

#![allow(non_snake_case, non_upper_case_globals)]

use crate::params::{
    c_uConsecutiveFramePayload, c_uMaxClassicPduLength, ApplyPadding, IsoTpParameters,
};

/// PCI type nibbles, as they appear in the top half of the first byte.
const c_byPciSingleFrame: u8 = 0x0;
const c_byPciFirstFrame: u8 = 0x1;
const c_byPciConsecutiveFrame: u8 = 0x2;
const c_byPciFlowControl: u8 = 0x3;

/// FlowStatus values this end sends.
const c_byFlowStatusContinue: u8 = 0x30;
const c_byFlowStatusOverflow: u8 = 0x32;

/// Why an inbound message was abandoned.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IsoTpReceiveError {
    /// A consecutive frame arrived out of order, so bytes are missing.
    #[error("consecutive frame sequence number {u8Received} arrived where {u8Expected} was expected; the message is incomplete and was abandoned")]
    WrongSequenceNumber { u8Expected: u8, u8Received: u8 },

    /// A frame arrived that makes no sense where it did.
    #[error("a consecutive frame arrived with no message in progress")]
    UnexpectedConsecutiveFrame,
}

/// What arrived, and what this end owes the sender because of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiveOutcome {
    /// Nothing to do: a frame that is not part of an inbound message, or one that only added
    /// bytes to a message still in progress.
    Nothing,
    /// A whole PDU arrived.
    Completed { vecPdu: Vec<u8> },
    /// Send this flow-control frame, then expect more.
    SendFlowControl { vecFrame: Vec<u8> },
    /// The message is bigger than this end will hold; send this refusal and expect no more.
    Refused { vecFrame: Vec<u8> },
    /// The message was abandoned.
    Aborted(IsoTpReceiveError),
}

/// A message being assembled.
struct Pending {
    m_vecBuffer: Vec<u8>,
    m_uRemaining: usize,
    m_u8NextSequenceNumber: u8,
    /// Consecutive frames still expected before another flow control is due; `None` when this
    /// end set no block limit.
    m_optU8RemainingInBlock: Option<u8>,
}

/// Reassembles inbound frames and generates the flow control they require.
pub struct IsoTpReceiver {
    m_params: IsoTpParameters,
    /// The longest message this end will assemble. Defaults to everything a classic first
    /// frame can announce, but a real ECU has a finite buffer and lowering this is how that is
    /// simulated — it is also the only way the overflow refusal is reachable, since a 12-bit
    /// length field cannot exceed the classic maximum.
    m_uMaxPduLength: usize,
    /// Functional addressing reaches every ECU at once, so there is no single peer to flow
    /// control and a multi-frame request cannot work. Such a receiver accepts single frames
    /// only.
    m_bIsFunctional: bool,
    m_optPending: Option<Pending>,
}

impl IsoTpReceiver {
    /// A receiver for one ECU's physical request identifier.
    pub fn NewPhysical(params: IsoTpParameters) -> Self {
        IsoTpReceiver {
            m_params: params,
            m_uMaxPduLength: c_uMaxClassicPduLength,
            m_bIsFunctional: false,
            m_optPending: None,
        }
    }

    /// Limit how large a message this end will assemble, as a real ECU's buffer does.
    pub fn WithMaxPduLength(mut self, uMaxPduLength: usize) -> Self {
        self.m_uMaxPduLength = uMaxPduLength.min(c_uMaxClassicPduLength);
        self
    }

    /// A receiver for a broadcast identifier: single frames only, and it never flow-controls.
    pub fn NewFunctional(params: IsoTpParameters) -> Self {
        IsoTpReceiver {
            m_params: params,
            m_uMaxPduLength: c_uMaxClassicPduLength,
            m_bIsFunctional: true,
            m_optPending: None,
        }
    }

    /// True while a multi-frame message is part-way through.
    pub fn IsAssembling(&self) -> bool {
        self.m_optPending.is_some()
    }

    /// Abandon anything in progress, e.g. because the simulation was stopped.
    pub fn Reset(&mut self) {
        self.m_optPending = None;
    }

    /// Take one inbound frame.
    pub fn OnFrame(&mut self, vecData: &[u8]) -> ReceiveOutcome {
        if vecData.is_empty() {
            return ReceiveOutcome::Nothing;
        }

        match vecData[0] >> 4 {
            c_byPciSingleFrame => self.OnSingleFrame(vecData),
            c_byPciFirstFrame => self.OnFirstFrame(vecData),
            c_byPciConsecutiveFrame => self.OnConsecutiveFrame(vecData),
            // A flow control belongs to the transmit side; the caller routes it there.
            c_byPciFlowControl => ReceiveOutcome::Nothing,
            _ => ReceiveOutcome::Nothing,
        }
    }

    /// A whole PDU in one frame. No flow control is ever owed for one.
    fn OnSingleFrame(&mut self, vecData: &[u8]) -> ReceiveOutcome {
        // A single frame abandons anything in progress, matching the offline reassembler so
        // that a live capture and a later replay of it agree.
        self.m_optPending = None;

        let uLength = (vecData[0] & 0x0F) as usize;
        if uLength == 0 || uLength + 1 > vecData.len() {
            tracing::warn!(len = uLength, "ignoring a malformed single frame");
            return ReceiveOutcome::Nothing;
        }

        // The length comes from the PCI, never from the frame's size: the rest is padding.
        ReceiveOutcome::Completed {
            vecPdu: vecData[1..1 + uLength].to_vec(),
        }
    }

    /// The opening frame of a long message.
    fn OnFirstFrame(&mut self, vecData: &[u8]) -> ReceiveOutcome {
        if self.m_bIsFunctional {
            // A broadcast has no single peer to flow control, and several ECUs answering with
            // one at once would collide. Dropped silently, with nothing on the wire.
            tracing::debug!(
                "dropping a multi-frame request on a broadcast identifier; ISO 15765-2 allows single frames only"
            );
            return ReceiveOutcome::Nothing;
        }

        if vecData.len() < 2 {
            return ReceiveOutcome::Nothing;
        }

        let uTotalLength = (((vecData[0] & 0x0F) as usize) << 8) | (vecData[1] as usize);
        if uTotalLength <= c_uConsecutiveFramePayload {
            // It would have fitted a single frame, so this is malformed.
            tracing::warn!(
                len = uTotalLength,
                "ignoring a first frame that should have been a single frame"
            );
            return ReceiveOutcome::Nothing;
        }

        if uTotalLength > self.m_uMaxPduLength {
            tracing::warn!(
                announced = uTotalLength,
                limit = self.m_uMaxPduLength,
                "refusing an inbound message larger than this end will hold"
            );
            self.m_optPending = None;
            return ReceiveOutcome::Refused {
                vecFrame: self.BuildFlowControl(c_byFlowStatusOverflow),
            };
        }

        let vecFirst = vecData[2..].to_vec();
        let uTaken = vecFirst.len().min(uTotalLength);
        self.m_optPending = Some(Pending {
            m_vecBuffer: vecFirst[..uTaken].to_vec(),
            m_uRemaining: uTotalLength - uTaken,
            m_u8NextSequenceNumber: 1,
            m_optU8RemainingInBlock: self.BlockAllowance(),
        });

        ReceiveOutcome::SendFlowControl {
            vecFrame: self.BuildFlowControl(c_byFlowStatusContinue),
        }
    }

    /// One more slice of a message in progress.
    fn OnConsecutiveFrame(&mut self, vecData: &[u8]) -> ReceiveOutcome {
        let mut pending = match self.m_optPending.take() {
            Some(pending) => pending,
            None => {
                tracing::debug!("ignoring a consecutive frame with no message in progress");
                return ReceiveOutcome::Nothing;
            }
        };

        let u8Received = vecData[0] & 0x0F;
        if u8Received != pending.m_u8NextSequenceNumber {
            // Bytes are missing, so whatever is assembled is wrong. Abandon it rather than
            // hand a plausible-looking but incorrect PDU upward.
            return ReceiveOutcome::Aborted(IsoTpReceiveError::WrongSequenceNumber {
                u8Expected: pending.m_u8NextSequenceNumber,
                u8Received,
            });
        }
        pending.m_u8NextSequenceNumber = pending.m_u8NextSequenceNumber.wrapping_add(1) & 0x0F;

        let vecPayload = &vecData[1..];
        let uTake = vecPayload.len().min(pending.m_uRemaining);
        pending.m_vecBuffer.extend_from_slice(&vecPayload[..uTake]);
        pending.m_uRemaining -= uTake;

        if pending.m_uRemaining == 0 {
            return ReceiveOutcome::Completed {
                vecPdu: pending.m_vecBuffer,
            };
        }

        // A block limit means another flow control is owed once this block is used up.
        if let Some(u8Remaining) = pending.m_optU8RemainingInBlock {
            let u8Left = u8Remaining.saturating_sub(1);
            if u8Left == 0 {
                pending.m_optU8RemainingInBlock = self.BlockAllowance();
                self.m_optPending = Some(pending);
                return ReceiveOutcome::SendFlowControl {
                    vecFrame: self.BuildFlowControl(c_byFlowStatusContinue),
                };
            }
            pending.m_optU8RemainingInBlock = Some(u8Left);
        }

        self.m_optPending = Some(pending);
        ReceiveOutcome::Nothing
    }

    /// How many consecutive frames this end will accept before asking again.
    fn BlockAllowance(&self) -> Option<u8> {
        if self.m_params.m_u8BlockSize == 0 {
            None
        } else {
            Some(self.m_params.m_u8BlockSize)
        }
    }

    /// Build a flow-control frame carrying this end's block size and separation time.
    fn BuildFlowControl(&self, byFlowStatus: u8) -> Vec<u8> {
        let mut vecFrame = vec![
            byFlowStatus,
            self.m_params.m_u8BlockSize,
            self.m_params.m_bySeparationTimeMin,
        ];
        ApplyPadding(&mut vecFrame, self.m_params.m_optByPaddingByte);
        vecFrame
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::c_byDefaultPaddingByte;

    fn Physical() -> IsoTpReceiver {
        IsoTpReceiver::NewPhysical(IsoTpParameters::default())
    }

    #[test]
    fn a_single_frame_completes_without_any_flow_control() {
        let mut receiver = Physical();
        let outcome = receiver.OnFrame(&[0x02, 0x10, 0x03, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA]);

        assert_eq!(
            outcome,
            ReceiveOutcome::Completed {
                vecPdu: vec![0x10, 0x03]
            }
        );
    }

    #[test]
    fn padding_never_becomes_part_of_the_pdu() {
        // The length comes from the PCI, never from how big the frame is. This is the trap
        // that turns a 10-byte write request into a 13-byte one.
        let mut receiver = Physical();

        let first = receiver.OnFrame(&[0x10, 0x0A, 0x2E, 0xF1, 0x90, b'1', b'H', b'G']);
        assert!(matches!(first, ReceiveOutcome::SendFlowControl { .. }));

        let second = receiver.OnFrame(&[0x21, b'C', b'M', b'8', b'2', 0xAA, 0xAA, 0xAA]);
        assert_eq!(
            second,
            ReceiveOutcome::Completed {
                vecPdu: vec![0x2E, 0xF1, 0x90, b'1', b'H', b'G', b'C', b'M', b'8', b'2']
            }
        );
    }

    #[test]
    fn a_first_frame_is_answered_with_flow_control_carrying_this_ends_limits() {
        let mut receiver = Physical();
        let outcome = receiver.OnFrame(&[0x10, 0x14, 0x62, 0xF1, 0x90, b'1', b'H', b'G']);

        // No block limit and no separation time: this end is a buffer on a host with
        // gigabytes, and claiming a limit it does not have would be a fabrication.
        assert_eq!(
            outcome,
            ReceiveOutcome::SendFlowControl {
                vecFrame: vec![
                    0x30,
                    0x00,
                    0x00,
                    c_byDefaultPaddingByte,
                    c_byDefaultPaddingByte,
                    c_byDefaultPaddingByte,
                    c_byDefaultPaddingByte,
                    c_byDefaultPaddingByte
                ]
            }
        );
        assert!(receiver.IsAssembling());
    }

    #[test]
    fn a_configured_block_size_asks_again_at_the_end_of_each_block() {
        let mut receiver = IsoTpReceiver::NewPhysical(IsoTpParameters {
            m_u8BlockSize: 2,
            m_bySeparationTimeMin: 0x14,
            ..IsoTpParameters::default()
        });

        // 24 bytes: 6 in the first frame, then 18 across three consecutive frames.
        let first = receiver.OnFrame(&[0x10, 0x18, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
        match first {
            ReceiveOutcome::SendFlowControl { vecFrame } => {
                assert_eq!(&vecFrame[0..3], &[0x30, 0x02, 0x14]);
            }
            other => panic!("expected flow control, got {other:?}"),
        }

        assert_eq!(
            receiver.OnFrame(&[0x21, 7, 8, 9, 10, 11, 12, 13]),
            ReceiveOutcome::Nothing
        );
        // Two frames in, the block is used up and another flow control is owed.
        assert!(matches!(
            receiver.OnFrame(&[0x22, 14, 15, 16, 17, 18, 19, 20]),
            ReceiveOutcome::SendFlowControl { .. }
        ));
        assert_eq!(
            receiver.OnFrame(&[0x23, 21, 22, 23, 24, 0xAA, 0xAA, 0xAA]),
            ReceiveOutcome::Completed {
                vecPdu: (1..=24u8).collect()
            }
        );
    }

    #[test]
    fn a_gap_in_the_sequence_abandons_the_message_rather_than_guessing() {
        let mut receiver = Physical();
        receiver.OnFrame(&[0x10, 0x14, 0x62, 0xF1, 0x90, b'1', b'H', b'G']);

        // Sequence number 2 where 1 was due: bytes are missing, so whatever is assembled is
        // wrong and handing it upward would be worse than losing it.
        let outcome = receiver.OnFrame(&[0x22, b'C', b'M', b'8', b'2', b'6', b'3', b'3']);
        assert_eq!(
            outcome,
            ReceiveOutcome::Aborted(IsoTpReceiveError::WrongSequenceNumber {
                u8Expected: 1,
                u8Received: 2
            })
        );
    }

    #[test]
    fn a_broadcast_never_flow_controls_a_multi_frame_request() {
        // A functional request reaches every ECU at once. There is no single peer to flow
        // control, and several ECUs answering with one would collide on the bus.
        let mut receiver = IsoTpReceiver::NewFunctional(IsoTpParameters::default());

        assert_eq!(
            receiver.OnFrame(&[0x10, 0x14, 0x62, 0xF1, 0x90, 0x01, 0x02, 0x03]),
            ReceiveOutcome::Nothing,
            "dropped silently, with nothing on the wire"
        );
        assert!(!receiver.IsAssembling());

        // Single frames still work perfectly.
        assert_eq!(
            receiver.OnFrame(&[0x02, 0x3E, 0x00, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA]),
            ReceiveOutcome::Completed {
                vecPdu: vec![0x3E, 0x00]
            }
        );
    }

    #[test]
    fn a_message_bigger_than_this_end_will_hold_is_refused_not_attempted() {
        // A 12-bit length field cannot exceed the classic maximum, so this refusal is only
        // reachable for an ECU with a smaller buffer — which is what a real one has.
        let mut receiver = Physical().WithMaxPduLength(64);
        let outcome = receiver.OnFrame(&[0x10, 0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);

        match outcome {
            ReceiveOutcome::Refused { vecFrame } => assert_eq!(vecFrame[0], 0x32),
            other => panic!("expected an overflow refusal, got {other:?}"),
        }
        assert!(
            !receiver.IsAssembling(),
            "nothing is buffered for a refused message"
        );
    }

    #[test]
    fn a_new_message_abandons_an_unfinished_one() {
        let mut receiver = Physical();
        receiver.OnFrame(&[0x10, 0x14, 0x62, 0xF1, 0x90, b'1', b'H', b'G']);

        // Matching the offline reassembler, so a live capture and a later replay agree.
        assert_eq!(
            receiver.OnFrame(&[0x02, 0x3E, 0x00, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA]),
            ReceiveOutcome::Completed {
                vecPdu: vec![0x3E, 0x00]
            }
        );
        assert!(!receiver.IsAssembling());
    }

    #[test]
    fn a_stray_consecutive_frame_is_ignored() {
        let mut receiver = Physical();
        assert_eq!(
            receiver.OnFrame(&[0x21, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]),
            ReceiveOutcome::Nothing
        );
    }
}

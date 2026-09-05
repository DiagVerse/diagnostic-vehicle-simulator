//! Streaming decoder for the lines an adapter sends.
//!
//! Streaming matters more than it sounds. A serial read returns whatever bytes have arrived:
//! one line routinely arrives split across two reads, and two lines routinely arrive in one.
//! Assuming one read is one line is the single most common bug in SLCAN implementations, so
//! this decoder buffers and splits on the terminator, and nothing above it ever sees a partial
//! line.

#![allow(non_snake_case, non_upper_case_globals)]

use can::CanFrame;

use crate::{c_byBell, c_byTerminator, c_uMaxLineLength, SlcanDecodeError};

/// Something an adapter said.
#[derive(Debug, Clone, PartialEq)]
pub enum SlcanEvent {
    /// A CAN frame was received on the bus.
    Frame(CanFrame),
    /// The adapter acknowledged a command.
    Ack,
    /// The adapter rejected a command.
    Nack,
    /// A line arrived that could not be read as any of the above.
    Malformed(SlcanDecodeError),
}

/// Accumulates bytes and yields whole lines.
#[derive(Debug, Default)]
pub struct SlcanDecoder {
    m_strPartial: String,
    /// How many oversized lines have been discarded, so a device that never terminates one
    /// shows up as a number in the log rather than as growing memory.
    m_uDiscardedLines: usize,
}

impl SlcanDecoder {
    /// Create an empty decoder.
    pub fn New() -> Self {
        SlcanDecoder::default()
    }

    /// How many lines have been discarded for being too long.
    pub fn DiscardedLineCount(&self) -> usize {
        self.m_uDiscardedLines
    }

    /// Feed whatever the port returned, and take everything that completed.
    ///
    /// `f64TimestampSec` stamps any frames decoded from this chunk. The host's clock is used
    /// rather than an adapter timestamp: the adapter's counter wraps every 60 seconds, and a
    /// wrap mid-transfer would corrupt every latency measurement taken from it.
    pub fn Feed(&mut self, vecBytes: &[u8], f64TimestampSec: f64) -> Vec<SlcanEvent> {
        let mut vecEvents = Vec::new();

        for byByte in vecBytes {
            match *byByte {
                c_byTerminator => {
                    let strLine = std::mem::take(&mut self.m_strPartial);
                    if let Some(event) = DecodeLine(&strLine, f64TimestampSec) {
                        vecEvents.push(event);
                    }
                }
                c_byBell => vecEvents.push(SlcanEvent::Nack),
                byOther => self.PushCharacter(byOther),
            }
        }

        vecEvents
    }

    /// Add one character to the line being built, discarding a line that has grown past any
    /// plausible length.
    fn PushCharacter(&mut self, byByte: u8) {
        if self.m_strPartial.len() >= c_uMaxLineLength {
            self.m_uDiscardedLines += 1;
            tracing::warn!(
                discarded = self.m_uDiscardedLines,
                "SLCAN line exceeded {c_uMaxLineLength} characters with no terminator; discarding it"
            );
            self.m_strPartial.clear();
            return;
        }
        self.m_strPartial.push(byByte as char);
    }
}

/// Decode one complete line. `None` for an empty line, which is how an adapter acknowledges a
/// command with no data to return.
fn DecodeLine(strLine: &str, f64TimestampSec: f64) -> Option<SlcanEvent> {
    if strLine.is_empty() {
        return Some(SlcanEvent::Ack);
    }

    // 'z' and 'Z' acknowledge a transmit request. Some firmwares send them, some send a bare
    // acknowledgement, some send nothing; treat all three as success.
    if strLine == "z" || strLine == "Z" {
        return Some(SlcanEvent::Ack);
    }

    let byKind = strLine.as_bytes()[0];
    match byKind {
        b't' | b'T' | b'r' | b'R' => Some(match DecodeFrameLine(strLine, f64TimestampSec) {
            Ok(frame) => SlcanEvent::Frame(frame),
            Err(error) => {
                tracing::warn!(%error, "could not decode an SLCAN frame line");
                SlcanEvent::Malformed(error)
            }
        }),
        // Version, serial and status replies are informational; the bridge logs them but the
        // frame path does not care.
        b'V' | b'v' | b'N' | b'F' => Some(SlcanEvent::Ack),
        _ => Some(SlcanEvent::Ack),
    }
}

/// Decode a frame line: a kind character, the identifier, one length digit, then the data.
///
/// Parsed by position — SLCAN has no delimiters at all.
fn DecodeFrameLine(strLine: &str, f64TimestampSec: f64) -> Result<CanFrame, SlcanDecodeError> {
    let vecChars: Vec<char> = strLine.chars().collect();
    let byKind = strLine.as_bytes()[0];

    let bIsExtended = byKind == b'T' || byKind == b'R';
    let bIsRemote = byKind == b'r' || byKind == b'R';
    let uIdLength = if bIsExtended { 8 } else { 3 };

    // kind + identifier + one length digit
    let uHeaderLength = 1 + uIdLength + 1;
    if vecChars.len() < uHeaderLength {
        return Err(SlcanDecodeError::TooShort {
            strLine: strLine.to_string(),
            strKind: if bIsExtended { "29-bit" } else { "11-bit" }.to_string(),
        });
    }

    let strId: String = vecChars[1..1 + uIdLength].iter().collect();
    let u32CanId = u32::from_str_radix(&strId, 16).map_err(|_| SlcanDecodeError::NotHex {
        strLine: strLine.to_string(),
    })?;

    let strLength: String = vecChars[1 + uIdLength..uHeaderLength].iter().collect();
    let uLength = usize::from_str_radix(&strLength, 16).map_err(|_| SlcanDecodeError::NotHex {
        strLine: strLine.to_string(),
    })?;
    if uLength > 8 {
        return Err(SlcanDecodeError::BadLength {
            strLine: strLine.to_string(),
            uLength,
        });
    }

    // A remote frame requests data rather than carrying it, so it has no payload to read.
    let vecData = if bIsRemote {
        Vec::new()
    } else {
        DecodeDataBytes(strLine, &vecChars[uHeaderLength..], uLength)?
    };

    Ok(CanFrame {
        m_f64TimestampSec: f64TimestampSec,
        m_u32CanId: u32CanId,
        m_bIsExtended: bIsExtended,
        m_bIsFd: false,
        m_vecData: vecData,
        m_optBIsRequest: None,
    })
}

/// Read exactly `uLength` bytes of hex, ignoring anything after them — with timestamps enabled
/// an adapter appends four more hex digits, which are deliberately not used.
fn DecodeDataBytes(
    strLine: &str,
    vecRest: &[char],
    uLength: usize,
) -> Result<Vec<u8>, SlcanDecodeError> {
    let uRequiredChars = uLength * 2;
    if vecRest.len() < uRequiredChars {
        return Err(SlcanDecodeError::LengthMismatch {
            strLine: strLine.to_string(),
            uDeclared: uLength,
            uPresent: vecRest.len() / 2,
        });
    }

    let mut vecData = Vec::with_capacity(uLength);
    for uIndex in 0..uLength {
        let strByte: String = vecRest[uIndex * 2..uIndex * 2 + 2].iter().collect();
        let byByte = u8::from_str_radix(&strByte, 16).map_err(|_| SlcanDecodeError::NotHex {
            strLine: strLine.to_string(),
        })?;
        vecData.push(byByte);
    }
    Ok(vecData)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn FramesOf(vecEvents: Vec<SlcanEvent>) -> Vec<CanFrame> {
        vecEvents
            .into_iter()
            .filter_map(|event| match event {
                SlcanEvent::Frame(frame) => Some(frame),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_standard_frame_round_trips() {
        let frame = CanFrame::NewClassic(
            0.0,
            0x7E8,
            vec![0x06, 0x62, 0xF1, 0x90, 0x45, 0x41, 0x41, 0x00],
        );

        let strLine = crate::EncodeFrame(&frame);
        assert_eq!(strLine, "t7E880662F19045414100\r");

        let mut decoder = SlcanDecoder::New();
        let vecFrames = FramesOf(decoder.Feed(strLine.as_bytes(), 0.0));
        assert_eq!(vecFrames, vec![frame]);
    }

    #[test]
    fn an_extended_frame_round_trips() {
        let frame = CanFrame::NewClassic(
            0.0,
            0x18DAF110,
            vec![0x02, 0x10, 0x03, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA],
        );

        let strLine = crate::EncodeFrame(&frame);
        assert_eq!(strLine, "T18DAF1108021003AAAAAAAAAA\r");

        let mut decoder = SlcanDecoder::New();
        assert_eq!(FramesOf(decoder.Feed(strLine.as_bytes(), 0.0)), vec![frame]);
    }

    #[test]
    fn a_line_split_across_two_reads_still_decodes_once() {
        // The most common SLCAN bug: a serial read returns whatever has arrived, so one line
        // routinely arrives in two pieces and two lines routinely arrive in one.
        let mut decoder = SlcanDecoder::New();

        let vecFirst = FramesOf(decoder.Feed(b"t7E8806", 0.0));
        assert!(vecFirst.is_empty(), "half a line is not a frame yet");

        let vecSecond = FramesOf(decoder.Feed(b"62F19045414100\rt7E00", 0.0));
        assert_eq!(
            vecSecond.len(),
            1,
            "the completed line yields exactly one frame"
        );
        assert_eq!(vecSecond[0].m_u32CanId, 0x7E8);

        // The trailing partial is retained and completes on the next read.
        let vecThird = FramesOf(decoder.Feed(b"\r", 0.0));
        assert_eq!(vecThird.len(), 1);
        assert_eq!(vecThird[0].m_u32CanId, 0x7E0);
        assert!(vecThird[0].m_vecData.is_empty());
    }

    #[test]
    fn two_lines_in_one_read_yield_two_frames() {
        let mut decoder = SlcanDecoder::New();
        let vecFrames = FramesOf(decoder.Feed(b"t7E0202100D\rt7E8207E00\r", 0.0));
        assert_eq!(vecFrames.len(), 2);
        assert_eq!(vecFrames[0].m_vecData, vec![0x02, 0x10]);
        assert_eq!(vecFrames[1].m_u32CanId, 0x7E8);
    }

    #[test]
    fn lowercase_hex_is_accepted_even_though_it_is_never_emitted() {
        let mut decoder = SlcanDecoder::New();
        let vecFrames = FramesOf(decoder.Feed(b"t7e080210030000000000\r", 0.0));
        assert_eq!(vecFrames.len(), 1);
        assert_eq!(vecFrames[0].m_u32CanId, 0x7E0);
        assert_eq!(vecFrames[0].m_vecData[0..3], [0x02, 0x10, 0x03]);
    }

    #[test]
    fn an_adapter_timestamp_is_ignored_rather_than_trusted() {
        // With timestamps enabled the adapter appends four hex digits. Its counter wraps every
        // 60 seconds, so a wrap mid-transfer would corrupt any latency measured from it; the
        // host clock is used instead.
        let mut decoder = SlcanDecoder::New();
        let vecFrames = FramesOf(decoder.Feed(b"t7E0802100300000000001A2B\r", 12.5));
        assert_eq!(vecFrames.len(), 1);
        assert_eq!(vecFrames[0].m_vecData.len(), 8);
        assert_eq!(vecFrames[0].m_f64TimestampSec, 12.5);
    }

    #[test]
    fn malformed_lines_are_reported_and_never_panic() {
        let arrBadLines: [&[u8]; 4] = [
            b"t7E08021003\r", // declares 8 bytes, carries 5
            b"t7E09FF\r",     // length 9 is impossible on classic CAN
            b"t7E0802100\r",  // an odd number of hex digits
            b"tZZ\r",         // not hex at all
        ];

        for vecLine in arrBadLines {
            let mut decoder = SlcanDecoder::New();
            let vecEvents = decoder.Feed(vecLine, 0.0);
            assert!(
                vecEvents
                    .iter()
                    .any(|event| matches!(event, SlcanEvent::Malformed(_))),
                "expected a reported failure for {:?}",
                String::from_utf8_lossy(vecLine)
            );
        }
    }

    #[test]
    fn a_rejection_byte_is_reported_as_one() {
        let mut decoder = SlcanDecoder::New();
        let vecEvents = decoder.Feed(&[crate::c_byBell], 0.0);
        assert_eq!(vecEvents, vec![SlcanEvent::Nack]);
    }

    #[test]
    fn a_device_that_never_terminates_a_line_does_not_grow_memory() {
        let mut decoder = SlcanDecoder::New();
        decoder.Feed(&vec![b'A'; c_uMaxLineLength * 3], 0.0);
        assert!(decoder.DiscardedLineCount() >= 2);
    }

    #[test]
    fn every_frame_survives_a_round_trip() {
        // The property that matters: encoding then decoding must give back what went in.
        for u32CanId in [0x000, 0x7DF, 0x7FF, 0x18DAF110, 0x1FFFFFFF] {
            for uLength in 0..=8usize {
                let vecData: Vec<u8> = (0..uLength).map(|uIndex| uIndex as u8 * 17).collect();
                let frame = CanFrame::NewClassic(0.0, u32CanId, vecData);

                let mut decoder = SlcanDecoder::New();
                let vecDecoded = FramesOf(decoder.Feed(crate::EncodeFrame(&frame).as_bytes(), 0.0));
                assert_eq!(vecDecoded, vec![frame], "id {u32CanId:X} length {uLength}");
            }
        }
    }

    #[test]
    fn opening_closes_first_then_sets_the_bitrate() {
        // An adapter left open by a crashed process rejects the bitrate command, and that
        // rejection is indistinguishable from "wrong bitrate" without the defensive close.
        assert_eq!(
            crate::OpenCommands(crate::SlcanBitrate::Kbit500),
            vec!["C\r", "S6\r", "O\r"]
        );
    }

    #[test]
    fn status_flags_are_named_rather_than_printed_as_a_number() {
        assert_eq!(crate::DescribeStatusFlags(0x00), Vec::<&str>::new());
        assert_eq!(crate::DescribeStatusFlags(0x80), vec!["bus off"]);
        assert_eq!(
            crate::DescribeStatusFlags(0x0C),
            vec!["error warning", "data overrun"]
        );
    }
}

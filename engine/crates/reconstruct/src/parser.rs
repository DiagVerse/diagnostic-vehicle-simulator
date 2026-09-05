//! CAN-log parsers.
//!
//! Three common text formats are supported (see ADR 0003):
//!   - Vector `.asc`   — `<time> <ch> <id> Rx/Tx d <dlc> <b0> <b1> ...`
//!   - Linux candump  — `(<time>) <iface> <id>#<hexbytes>`
//!   - Timestamped diagnostic trace — `HH:MM:SS.ffffff>>0x18dad4f1 -> 02 10 01 55 …`,
//!     as produced by service tools. Unlike the other two it carries an explicit direction
//!     marker (`>>` from the tester, `<<` from the ECU).
//!
//! Lines that do not parse (headers, comments, blank lines, unsupported frame kinds) are
//! skipped rather than causing a hard error, so real-world logs with banners still load.

#![allow(non_snake_case, non_upper_case_globals)]

use can::CanFrame;

/// Errors that can occur while parsing a log.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// No frames could be parsed from the input.
    #[error("no CAN frames found in log (unrecognised format or empty)")]
    NoFrames,
}

/// Parse a CAN log (either supported format) into time-ordered frames.
pub fn ParseCanLog(strContent: &str) -> Result<Vec<CanFrame>, ParseError> {
    let mut vecFrames = Vec::new();

    for strLine in strContent.lines() {
        let strTrimmed = strLine.trim();
        if strTrimmed.is_empty() {
            continue;
        }

        // Each format is recognised by a marker no other format uses: candump parenthesises
        // its timestamp and joins id and data with '#', and the diagnostic trace separates
        // its identifier from its data with "->" after a direction marker.
        let optFrame = if strTrimmed.starts_with('(') && strTrimmed.contains('#') {
            ParseCandumpLine(strTrimmed)
        } else if strTrimmed.contains(">>") || strTrimmed.contains("<<") {
            ParseDiagnosticTraceLine(strTrimmed)
        } else {
            ParseAscLine(strTrimmed)
        };

        if let Some(frame) = optFrame {
            vecFrames.push(frame);
        }
    }

    if vecFrames.is_empty() {
        return Err(ParseError::NoFrames);
    }
    Ok(vecFrames)
}

/// Parse one candump line: `(1610000000.000000) can0 7E0#0210030000000000`.
fn ParseCandumpLine(strLine: &str) -> Option<CanFrame> {
    let vecTokens: Vec<&str> = strLine.split_whitespace().collect();
    if vecTokens.len() < 3 {
        return None;
    }

    let strTime = vecTokens[0].trim_start_matches('(').trim_end_matches(')');
    let f64Time = strTime.parse::<f64>().ok()?;

    let strIdAndData = vecTokens[2];
    let (strId, strData) = strIdAndData.split_once('#')?;

    let u32Id = u32::from_str_radix(strId, 16).ok()?;
    let vecData = ParseHexBytes(strData)?;

    Some(CanFrame::NewClassic(f64Time, u32Id, vecData))
}

/// Parse one timestamped diagnostic-trace line:
/// `16:11:50.767561>>0x18dad4f1 -> 02 10 01 55 55 55 55 55`.
///
/// The direction marker (`>>` tester to ECU, `<<` ECU to tester) is what makes this format
/// worth supporting: the other two leave the reconstruction to infer who was talking.
///
/// Timestamps are a wall clock with no date, so they are converted to seconds since midnight.
/// A trace that crosses midnight would go backwards; that is left alone rather than guessed
/// at, since the frames are re-sorted by time and a wrong guess would reorder a real exchange.
fn ParseDiagnosticTraceLine(strLine: &str) -> Option<CanFrame> {
    let (strTime, strRest, bIsRequest) = SplitOnDirectionMarker(strLine)?;

    let f64TimestampSec = ParseWallClockSeconds(strTime)?;

    let (strId, strData) = strRest.split_once("->")?;
    let strIdDigits = strId
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    let u32CanId = u32::from_str_radix(strIdDigits, 16).ok()?;

    let vecData = ParseSpacedHexBytes(strData)?;

    Some(CanFrame::NewDirected(
        f64TimestampSec,
        u32CanId,
        vecData,
        bIsRequest,
    ))
}

/// Split a trace line at its direction marker, returning the timestamp, the rest of the line,
/// and whether the frame came from the tester.
fn SplitOnDirectionMarker(strLine: &str) -> Option<(&str, &str, bool)> {
    if let Some((strTime, strRest)) = strLine.split_once(">>") {
        return Some((strTime, strRest, true));
    }
    let (strTime, strRest) = strLine.split_once("<<")?;
    Some((strTime, strRest, false))
}

/// Convert `HH:MM:SS.ffffff` to seconds since midnight.
fn ParseWallClockSeconds(strTime: &str) -> Option<f64> {
    let vecParts: Vec<&str> = strTime.trim().split(':').collect();
    if vecParts.len() != 3 {
        return None;
    }

    let f64Hours = vecParts[0].parse::<f64>().ok()?;
    let f64Minutes = vecParts[1].parse::<f64>().ok()?;
    let f64Seconds = vecParts[2].parse::<f64>().ok()?;

    Some(f64Hours * 3600.0 + f64Minutes * 60.0 + f64Seconds)
}

/// Parse space-separated hex bytes (`02 10 01 55 …`) into bytes. Returns `None` if any token
/// is not a byte, so a malformed line is skipped rather than half-read.
fn ParseSpacedHexBytes(strData: &str) -> Option<Vec<u8>> {
    let mut vecBytes = Vec::new();
    for strToken in strData.split_whitespace() {
        vecBytes.push(u8::from_str_radix(strToken, 16).ok()?);
    }

    if vecBytes.is_empty() {
        return None;
    }
    Some(vecBytes)
}

/// Parse one Vector `.asc` line: `0.001000 1 7E0 Rx d 8 02 10 03 00 00 00 00 00`.
fn ParseAscLine(strLine: &str) -> Option<CanFrame> {
    let vecTokens: Vec<&str> = strLine.split_whitespace().collect();

    // Locate the data marker "d"; the DLC follows it, then that many data bytes.
    let uMarker = vecTokens.iter().position(|&t| t == "d")?;
    if uMarker < 3 || uMarker + 1 >= vecTokens.len() {
        return None;
    }

    let f64Time = vecTokens[0].parse::<f64>().ok()?;
    // Identifier: token before the direction; strip a trailing 'x' (extended marker).
    let strId = vecTokens[2].trim_end_matches('x');
    let u32Id = u32::from_str_radix(strId, 16).ok()?;

    let uDlc = vecTokens[uMarker + 1].parse::<usize>().ok()?;
    let uFirstByte = uMarker + 2;
    if uFirstByte + uDlc > vecTokens.len() {
        return None;
    }

    let mut vecData = Vec::with_capacity(uDlc);
    for strByte in &vecTokens[uFirstByte..uFirstByte + uDlc] {
        vecData.push(u8::from_str_radix(strByte, 16).ok()?);
    }

    Some(CanFrame::NewClassic(f64Time, u32Id, vecData))
}

/// Parse a contiguous hex string (even length) into bytes.
fn ParseHexBytes(strHex: &str) -> Option<Vec<u8>> {
    if !strHex.len().is_multiple_of(2) {
        return None;
    }
    let vecChars: Vec<char> = strHex.chars().collect();
    let mut vecBytes = Vec::with_capacity(vecChars.len() / 2);
    let mut iIndex = 0;
    while iIndex < vecChars.len() {
        let strByte: String = vecChars[iIndex..iIndex + 2].iter().collect();
        vecBytes.push(u8::from_str_radix(&strByte, 16).ok()?);
        iIndex += 2;
    }
    Some(vecBytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_diagnostic_trace_line_with_its_direction() {
        let vecFrames =
            ParseCanLog("16:11:50.767561>>0x18dad4f1 -> 02 10 01 55 55 55 55 55 ").unwrap();

        assert_eq!(vecFrames.len(), 1);
        let frame = &vecFrames[0];
        assert_eq!(frame.m_u32CanId, 0x18DAD4F1);
        assert!(frame.m_bIsExtended);
        assert_eq!(
            frame.m_vecData,
            vec![0x02, 0x10, 0x01, 0x55, 0x55, 0x55, 0x55, 0x55]
        );
        // The tester sent it, and the log said so rather than leaving it to be inferred.
        assert_eq!(frame.m_optBIsRequest, Some(true));
        // 16:11:50.767561 as seconds since midnight.
        let f64Expected = 16.0 * 3600.0 + 11.0 * 60.0 + 50.767561;
        assert!((frame.m_f64TimestampSec - f64Expected).abs() < 1e-6);
    }

    #[test]
    fn a_diagnostic_trace_response_is_marked_as_one() {
        let vecFrames =
            ParseCanLog("16:11:50.768366<<0x000765 -> 06 50 01 00 32 01 F4 00 ").unwrap();

        assert_eq!(vecFrames[0].m_u32CanId, 0x765);
        assert!(!vecFrames[0].m_bIsExtended);
        assert_eq!(vecFrames[0].m_optBIsRequest, Some(false));
    }

    #[test]
    fn malformed_diagnostic_trace_lines_are_skipped_not_half_read() {
        let strLog = "not a log line at all\n\
                      16:11:50.767561>>0x18dad4f1 -> 02 10 01 ZZ\n\
                      16:11:50.767561>>0x18dad4f1 -> \n\
                      16:11:50.767561>>0x18dad4f1 -> 02 10 01 55 55 55 55 55";
        let vecFrames = ParseCanLog(strLog).unwrap();
        assert_eq!(vecFrames.len(), 1, "only the well-formed line should parse");
    }

    #[test]
    fn parses_candump() {
        let log = "(1610000000.000000) can0 7E0#0210030000000000\n\
                   (1610000000.001000) can0 7E8#065003001F4000";
        let frames = ParseCanLog(log).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].m_u32CanId, 0x7E0);
        assert_eq!(frames[0].m_vecData[0], 0x02);
        assert_eq!(frames[1].m_u32CanId, 0x7E8);
    }

    #[test]
    fn parses_asc_and_skips_headers() {
        let log = "date Tue Jan 01 00:00:00 2024\n\
                   base hex timestamps absolute\n\
                   0.001000 1 7E0 Rx d 3 02 10 03\n\
                   0.002000 1 7E8 Rx d 4 03 50 03 00";
        let frames = ParseCanLog(log).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].m_u32CanId, 0x7E0);
        assert_eq!(frames[0].m_vecData, vec![0x02, 0x10, 0x03]);
        assert_eq!(frames[1].m_vecData, vec![0x03, 0x50, 0x03, 0x00]);
    }

    #[test]
    fn empty_log_errors() {
        assert!(ParseCanLog("\n\n# just comments\n").is_err());
    }
}

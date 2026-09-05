//! CAN-log parsers.
//!
//! Two common text formats are supported (see ADR 0003):
//!   - Vector `.asc`  — `<time> <ch> <id> Rx/Tx d <dlc> <b0> <b1> ...`
//!   - Linux candump — `(<time>) <iface> <id>#<hexbytes>`
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

        // candump lines start with a parenthesised timestamp and contain '#'.
        let optFrame = if strTrimmed.starts_with('(') && strTrimmed.contains('#') {
            ParseCandumpLine(strTrimmed)
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

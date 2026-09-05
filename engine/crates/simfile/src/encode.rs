//! Reading the shorthands a person writes in a simulation file.
//!
//! Hex with or without spaces, a wildcard pattern, and — the one with real domain content —
//! a trouble code written the way a technician would say it rather than as three raw bytes.

#![allow(non_snake_case, non_upper_case_globals)]

/// Ready-made status bytes for the states a fault is usually in, so a file can say
/// `"activeConfirmed"` instead of a magic `0x2F`.
///
/// The first three values are the ones ISO 14229-1's own worked examples use, which is why
/// they are these numbers and not something rounder.
const c_arrStatusPresets: [(&str, u8); 6] = [
    // Nothing has run yet: the honest state immediately after a clear, and emphatically not
    // 0x00 — see the note on `ParseStatusByte`.
    ("neverTested", 0x10 | 0x40),
    // Seen this cycle or the last, not yet confirmed.
    ("pendingOnly", 0x04 | 0x20),
    // Failing right now, and it has been failing this cycle.
    ("failingThisCycle", 0x02 | 0x04 | 0x20),
    // Failing now and confirmed in long-term memory: the ordinary "there is a fault" state.
    ("activeConfirmed", 0x01 | 0x02 | 0x04 | 0x08 | 0x20),
    // The same, with the ECU asking for the warning lamp.
    (
        "activeConfirmedWithLamp",
        0x01 | 0x02 | 0x04 | 0x08 | 0x20 | 0x80,
    ),
    // Stored from an earlier failure, not failing now.
    ("historyConfirmed", 0x08 | 0x20),
];

/// The status-of-DTC bits (ISO 14229-1), so an author can say what they mean rather than
/// reaching for a magic number.
const c_arrStatusBitNames: [(&str, u8); 8] = [
    ("testFailed", 0x01),
    ("testFailedThisOperationCycle", 0x02),
    ("pendingDTC", 0x04),
    ("confirmedDTC", 0x08),
    ("testNotCompletedSinceLastClear", 0x10),
    ("testFailedSinceLastClear", 0x20),
    ("testNotCompletedThisOperationCycle", 0x40),
    ("warningIndicatorRequested", 0x80),
];

/// Parse a byte written in hex, with or without an `0x`.
pub fn ParseHexByte(strValue: &str) -> Result<u8, String> {
    let strTrimmed = strValue.trim();
    let strDigits = strTrimmed
        .strip_prefix("0x")
        .or_else(|| strTrimmed.strip_prefix("0X"))
        .unwrap_or(strTrimmed);

    u8::from_str_radix(strDigits, 16).map_err(|_| format!("'{strValue}' is not a hex byte"))
}

/// Parse a run of hex bytes, spaces optional.
pub fn ParseHexBytes(strValue: &str) -> Result<Vec<u8>, String> {
    let strClean: String = strValue.chars().filter(|c| !c.is_whitespace()).collect();
    if strClean.is_empty() {
        return Ok(Vec::new());
    }
    if !strClean.len().is_multiple_of(2) {
        return Err(format!(
            "'{strValue}' has an odd number of hex digits, so it is not a whole number of bytes"
        ));
    }

    let vecChars: Vec<char> = strClean.chars().collect();
    let mut vecBytes = Vec::with_capacity(vecChars.len() / 2);
    for arrPair in vecChars.chunks(2) {
        let strByte: String = arrPair.iter().collect();
        let byByte = u8::from_str_radix(&strByte, 16)
            .map_err(|_| format!("'{strByte}' in '{strValue}' is not a hex byte"))?;
        vecBytes.push(byByte);
    }
    Ok(vecBytes)
}

/// Parse a request pattern, where a byte written `**` matches any value.
pub fn ParseHexPattern(strValue: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    let strClean: String = strValue.chars().filter(|c| !c.is_whitespace()).collect();
    if strClean.is_empty() {
        return Err("the request pattern is empty".to_string());
    }
    if !strClean.len().is_multiple_of(2) {
        return Err(format!("'{strValue}' has an odd number of characters"));
    }

    let vecChars: Vec<char> = strClean.chars().collect();
    let mut vecPattern = Vec::new();
    let mut vecMask = Vec::new();

    for arrPair in vecChars.chunks(2) {
        let strToken: String = arrPair.iter().collect();
        if matches!(strToken.as_str(), "**" | "??" | ".." | "xx" | "XX") {
            vecPattern.push(0x00);
            vecMask.push(0x00);
            continue;
        }
        let byByte = u8::from_str_radix(&strToken, 16)
            .map_err(|_| format!("'{strToken}' is not a hex byte or a wildcard (**)"))?;
        vecPattern.push(byByte);
        vecMask.push(0xFF);
    }
    Ok((vecPattern, vecMask))
}

/// Parse a trouble code written the way a technician says it, or as raw bytes.
///
/// `P0123` is the familiar form. The letter picks the system — Powertrain, Chassis, Body,
/// Network — and becomes the top two bits of the first byte; the four digits that follow fill
/// the remaining six bits and the second byte. A third byte says *how* the thing failed, and is
/// written after a dash: `P0123-11`. Left off, it is zero, which means "no failure type
/// specified" rather than any particular fault.
///
/// A raw `0x012311` is accepted too, for a code whose text form nobody agrees on.
pub fn ParseDtcCode(strCode: &str) -> Result<u32, String> {
    let strTrimmed = strCode.trim();

    if let Some(strDigits) = strTrimmed
        .strip_prefix("0x")
        .or_else(|| strTrimmed.strip_prefix("0X"))
    {
        let u32Code = u32::from_str_radix(strDigits, 16)
            .map_err(|_| format!("'{strCode}' is not a hex trouble code"))?;
        if u32Code > 0xFF_FFFF {
            return Err(format!(
                "'{strCode}' is wider than the three bytes a DTC has"
            ));
        }
        return Ok(u32Code);
    }

    ParseTextDtcCode(strTrimmed)
}

/// Parse the `P0123-11` form.
fn ParseTextDtcCode(strCode: &str) -> Result<u32, String> {
    let (strMain, byFailureType) = match strCode.split_once('-') {
        Some((strMain, strFailureType)) => (
            strMain,
            u8::from_str_radix(strFailureType, 16).map_err(|_| {
                format!("'{strFailureType}' in '{strCode}' is not a hex failure type")
            })?,
        ),
        None => (strCode, 0x00),
    };

    let vecChars: Vec<char> = strMain.chars().collect();
    if vecChars.len() != 5 {
        return Err(format!(
            "'{strCode}' should be a letter and four digits, such as P0123 or P0123-11"
        ));
    }

    let byCategory = match vecChars[0].to_ascii_uppercase() {
        'P' => 0b00,
        'C' => 0b01,
        'B' => 0b10,
        'U' => 0b11,
        chOther => {
            return Err(format!(
                "'{chOther}' is not a system letter; use P (powertrain), C (chassis), B (body) or U (network)"
            ))
        }
    };

    let strDigits: String = vecChars[1..].iter().collect();
    let u16Digits = u16::from_str_radix(&strDigits, 16)
        .map_err(|_| format!("'{strDigits}' in '{strCode}' is not four hex digits"))?;

    // The first digit only has room for two bits, which is why it never exceeds 3 in a real
    // code — the top two bits of that nibble belong to the system letter.
    if u16Digits > 0x3FFF {
        return Err(format!(
            "'{strCode}' has a first digit above 3; those two bits carry the system letter"
        ));
    }

    let u32Code = ((byCategory as u32) << 22) | ((u16Digits as u32) << 8) | byFailureType as u32;
    Ok(u32Code)
}

/// Render a three-byte trouble code back into the familiar text form.
pub fn FormatDtcCode(u32Code: u32) -> String {
    let chCategory = match (u32Code >> 22) & 0b11 {
        0b00 => 'P',
        0b01 => 'C',
        0b10 => 'B',
        _ => 'U',
    };
    let u16Digits = ((u32Code >> 8) & 0x3FFF) as u16;
    let byFailureType = (u32Code & 0xFF) as u8;

    format!("{chCategory}{u16Digits:04X}-{byFailureType:02X}")
}

/// Parse a status-of-DTC byte: a preset name, hex, or the individual bits joined with `|`.
///
/// One trap worth knowing about, because it silently produces a fault nobody can find. Bits 4
/// and 6 are *negative* — they mean "has **not** run" — so `0x00` does not mean "no
/// information", it means "the test has run, this cycle and since the last clear, and has never
/// failed". A tester reads faults by masking, and a server returns only those where
/// `status & mask` is non-zero, so a DTC stored with `0x00` is invisible to every possible
/// read. `neverTested` is what an author reaching for "nothing has happened yet" actually
/// wants.
pub fn ParseStatusByte(strValue: &str) -> Result<u8, String> {
    let strTrimmed = strValue.trim();

    let optPreset = c_arrStatusPresets
        .iter()
        .find(|(strName, _)| strName.eq_ignore_ascii_case(strTrimmed));
    if let Some((_, byStatus)) = optPreset {
        return Ok(*byStatus);
    }

    // Hex next: every bit and preset name contains characters hex cannot, so nothing could be
    // read as both and there is no ambiguity to resolve.
    if let Ok(byStatus) = ParseHexByte(strTrimmed) {
        WarnIfInvisible(byStatus);
        return Ok(byStatus);
    }

    let mut byStatus = 0u8;
    for strName in strTrimmed.split(['|', ',']) {
        let strName = strName.trim();
        if strName.is_empty() {
            continue;
        }

        let optBit = c_arrStatusBitNames
            .iter()
            .find(|(strBitName, _)| strBitName.eq_ignore_ascii_case(strName));
        match optBit {
            Some((_, byMask)) => byStatus |= byMask,
            None => {
                let strBits: Vec<&str> = c_arrStatusBitNames
                    .iter()
                    .map(|(strBitName, _)| *strBitName)
                    .collect();
                let strPresets: Vec<&str> = c_arrStatusPresets
                    .iter()
                    .map(|(strPresetName, _)| *strPresetName)
                    .collect();
                return Err(format!(
                    "'{strName}' is not a status bit or preset. Presets: {}. Bits: {}",
                    strPresets.join(", "),
                    strBits.join(", ")
                ));
            }
        }
    }

    WarnIfInvisible(byStatus);
    Ok(byStatus)
}

/// Say so when a status byte would hide the fault it describes.
fn WarnIfInvisible(byStatus: u8) {
    if byStatus == 0x00 {
        tracing::warn!(
            "a DTC status of 0x00 is invisible to every read: a tester selects faults by masking, and no mask matches zero. 'neverTested' (0x50) is probably what was meant"
        );
    }
}

/// Name the bits set in a status-of-DTC byte.
pub fn DescribeStatusByte(byStatus: u8) -> Vec<&'static str> {
    c_arrStatusBitNames
        .iter()
        .filter(|(_, byMask)| (byStatus & byMask) != 0)
        .map(|(strName, _)| *strName)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_familiar_trouble_code_becomes_three_bytes() {
        // P is 00 in the top two bits, then 0123 fills the rest, then the failure type.
        assert_eq!(ParseDtcCode("P0123"), Ok(0x01_2300));
        assert_eq!(ParseDtcCode("P0123-11"), Ok(0x01_2311));
        // C, B and U shift the top two bits.
        assert_eq!(ParseDtcCode("C0123"), Ok(0x41_2300));
        assert_eq!(ParseDtcCode("B0123"), Ok(0x81_2300));
        assert_eq!(ParseDtcCode("U0123"), Ok(0xC1_2300));
    }

    #[test]
    fn the_codes_iso_14229_1_spells_out_encode_exactly_as_it_says() {
        // These three appear in ISO 14229-1 with their text form and their hex side by side
        // (Tables 287-289). They are the only DTCs in this project whose encoding can be
        // checked against the standard rather than against our own reasoning.
        assert_eq!(ParseDtcCode("P0805-11"), Ok(0x08_0511));
        assert_eq!(ParseDtcCode("P0A9B-17"), Ok(0x0A_9B17));
        assert_eq!(ParseDtcCode("P2522-1F"), Ok(0x25_221F));

        // And back again.
        assert_eq!(FormatDtcCode(0x08_0511), "P0805-11");
        assert_eq!(FormatDtcCode(0x0A_9B17), "P0A9B-17");
        assert_eq!(FormatDtcCode(0x25_221F), "P2522-1F");
    }

    #[test]
    fn a_trouble_code_round_trips_through_its_text_form() {
        for strCode in ["P0123-00", "C1234-11", "B0000-00", "U3FFF-FF"] {
            let u32Code = ParseDtcCode(strCode).expect("a valid code");
            assert_eq!(FormatDtcCode(u32Code), strCode);
        }
    }

    #[test]
    fn a_raw_trouble_code_is_accepted_for_the_cases_nobody_agrees_on() {
        assert_eq!(ParseDtcCode("0x012311"), Ok(0x01_2311));
        assert!(ParseDtcCode("0x1012311").is_err(), "wider than three bytes");
    }

    #[test]
    fn a_malformed_trouble_code_says_what_is_wrong() {
        assert!(ParseDtcCode("X0123").unwrap_err().contains("system letter"));
        assert!(ParseDtcCode("P012").unwrap_err().contains("four digits"));
        // The first digit shares its nibble with the system letter, so it never exceeds 3.
        assert!(ParseDtcCode("P4123").unwrap_err().contains("first digit"));
    }

    #[test]
    fn a_status_byte_can_be_written_as_the_bits_it_means() {
        assert_eq!(ParseStatusByte("0x2F"), Ok(0x2F));
        assert_eq!(ParseStatusByte("2F"), Ok(0x2F), "the 0x is optional");
        assert_eq!(ParseStatusByte("confirmedDTC"), Ok(0x08));
        assert_eq!(
            ParseStatusByte("confirmedDTC | testFailed"),
            Ok(0x08 | 0x01)
        );
        assert!(ParseStatusByte("nonsense")
            .unwrap_err()
            .contains("status bit"));
    }

    #[test]
    fn a_status_byte_can_be_written_as_the_state_the_fault_is_in() {
        // The first three come from ISO 14229-1's own worked examples, which is why they are
        // these numbers rather than something rounder.
        assert_eq!(ParseStatusByte("pendingOnly"), Ok(0x24));
        assert_eq!(ParseStatusByte("failingThisCycle"), Ok(0x26));
        assert_eq!(ParseStatusByte("activeConfirmed"), Ok(0x2F));
        assert_eq!(
            ParseStatusByte("activeconfirmed"),
            Ok(0x2F),
            "case does not matter"
        );

        // Bits 4 and 6 mean "has not run", so the state before anything has happened is 0x50
        // and not 0x00.
        assert_eq!(ParseStatusByte("neverTested"), Ok(0x50));
        assert_eq!(ParseStatusByte("historyConfirmed"), Ok(0x28));
    }

    #[test]
    fn an_unknown_status_name_lists_both_the_presets_and_the_bits() {
        let strError = ParseStatusByte("nonsense").unwrap_err();
        assert!(strError.contains("activeConfirmed"), "{strError}");
        assert!(strError.contains("confirmedDTC"), "{strError}");
    }

    #[test]
    fn status_bits_are_named_rather_than_printed_as_a_number() {
        assert_eq!(DescribeStatusByte(0x09), vec!["testFailed", "confirmedDTC"]);
    }

    #[test]
    fn hex_is_read_with_or_without_spaces_and_rejected_when_it_is_not_hex() {
        assert_eq!(ParseHexBytes("62 F1 90"), Ok(vec![0x62, 0xF1, 0x90]));
        assert_eq!(ParseHexBytes("62F190"), Ok(vec![0x62, 0xF1, 0x90]));
        assert!(ParseHexBytes("62F1 9").unwrap_err().contains("odd number"));
        assert!(ParseHexBytes("ZZ").is_err());
    }

    #[test]
    fn a_wildcard_pattern_carries_its_own_mask() {
        assert_eq!(
            ParseHexPattern("22 ** **"),
            Ok((vec![0x22, 0x00, 0x00], vec![0xFF, 0x00, 0x00]))
        );
    }
}

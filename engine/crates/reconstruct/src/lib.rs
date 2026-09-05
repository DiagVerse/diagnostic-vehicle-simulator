//! CAN-log reconstruction: turn a recorded CAN log into a Unified Vehicle Model.
//!
//! Pipeline (ADR 0003): parse -> ISO-TP reassemble -> correlate UDS pairs -> populate model.
//! All reconstructed facts are `Confidence::Observed`.

#![allow(non_snake_case, non_upper_case_globals)]

pub mod parser;
pub mod pipeline;

use core_domain::model::Vehicle;

pub use parser::{ParseCanLog, ParseError};
pub use pipeline::ReconstructFromFrames;

/// Errors from the end-to-end reconstruction.
#[derive(Debug, thiserror::Error)]
pub enum ReconstructError {
    /// The log could not be parsed.
    #[error(transparent)]
    Parse(#[from] ParseError),
}

/// Reconstruct a vehicle model directly from CAN-log text (either supported format).
pub fn ReconstructFromLogText(strContent: &str) -> Result<Vehicle, ReconstructError> {
    let vecFrames = ParseCanLog(strContent)?;
    Ok(ReconstructFromFrames(&vecFrames))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_to_end_from_candump_text() {
        // Request/response in candump format (single frames, 8-byte padded payloads).
        let log = "(0.001) can0 7E0#0210030000000000\n\
                   (0.002) can0 7E8#0350030000000000\n\
                   (0.003) can0 7E0#0322F19000000000\n\
                   (0.004) can0 7E8#0662F19041424300";
        let vehicle = ReconstructFromLogText(log).unwrap();
        assert_eq!(vehicle.m_vecEcus.len(), 1);
        let ecu = &vehicle.m_vecEcus[0];
        assert!(ecu.m_vecSupportedServices.contains(&0x10));
        assert!(ecu.FindDid(0xF190).is_some());
    }
}

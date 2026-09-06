//! `dvsim reconstruct <file>` — reconstruct a Unified Vehicle Model from a recorded trace and
//! print it as JSON.
//!
//! Takes either a CAN log or a pcap/pcapng capture of DoIP traffic, and works out which from
//! the file itself. Asking the user to say is asking them to know something the bytes already
//! state.

#![allow(non_snake_case, non_upper_case_globals)]

use std::path::Path;

use anyhow::Context;

/// Read the file at `path`, reconstruct a vehicle model, and print it as pretty JSON.
pub fn Run(path: &Path) -> anyhow::Result<()> {
    let vecBytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;

    let vehicle = if IsCapture(&vecBytes) {
        tracing::info!("reading {} as a packet capture", path.display());
        reconstruct::doip::ReconstructFromCapture(&vecBytes)
            .with_context(|| format!("failed to reconstruct from the capture {}", path.display()))?
    } else {
        let strContent = String::from_utf8(vecBytes).with_context(|| {
            format!(
                "{} is neither a packet capture nor UTF-8 text",
                path.display()
            )
        })?;
        reconstruct::ReconstructFromLogText(&strContent)
            .with_context(|| format!("failed to reconstruct from {}", path.display()))?
    };

    tracing::info!(
        ecus = vehicle.m_vecEcus.len(),
        "reconstructed vehicle from {}",
        path.display()
    );

    let strJson = vehicle
        .ToJson()
        .context("failed to serialize vehicle model")?;
    println!("{strJson}");
    Ok(())
}

/// True when these bytes begin like a pcap or pcapng file.
///
/// The four pcap magics cover both byte orders and both timestamp resolutions; `0x0A0D0D0A` is
/// a pcapng section header. A CAN log is text and starts with none of them.
fn IsCapture(vecBytes: &[u8]) -> bool {
    if vecBytes.len() < 4 {
        return false;
    }
    let u32Leading = u32::from_be_bytes([vecBytes[0], vecBytes[1], vecBytes[2], vecBytes[3]]);
    matches!(
        u32Leading,
        0xA1B2_C3D4 | 0xD4C3_B2A1 | 0xA1B2_3C4D | 0x4D3C_B2A1 | 0x0A0D_0D0A
    )
}

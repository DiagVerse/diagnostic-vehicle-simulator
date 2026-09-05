//! `dvsim reconstruct <canlog-file>` — parse a CAN log and print the reconstructed Unified
//! Vehicle Model as JSON. This is the offline half of the Phase 2 MVP (README §12): from a
//! recorded trace, discover the ECU(s) and their observed diagnostic behaviour.

#![allow(non_snake_case, non_upper_case_globals)]

use std::path::Path;

use anyhow::Context;

/// Read the log at `path`, reconstruct a vehicle model, and print it as pretty JSON.
pub fn Run(path: &Path) -> anyhow::Result<()> {
    let strContent = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read CAN log {}", path.display()))?;

    let vehicle = reconstruct::ReconstructFromLogText(&strContent)
        .with_context(|| format!("failed to reconstruct from {}", path.display()))?;

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

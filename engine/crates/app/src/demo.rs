//! `dvsim demo` — a scripted diagnostic exchange against a virtual ECU, driven by the UDS
//! protocol **plugin loaded dynamically** from `plugins.d/`. This proves the whole runtime
//! path end to end: discover the cdylib, resolve its protocol handler, and run a stateful
//! session. If the UDS plugin is not present it explains how to build/copy it.

#![allow(non_snake_case, non_upper_case_globals)]

use std::path::Path;

use application::PluginHost;
use core_domain::model::{DataIdentifier, DiagnosticTroubleCode, Ecu, SecurityLevel, SessionType};
use core_domain::Confidence;
use ecu::VirtualEcu;

/// Run the demo against plugins discovered in `pluginsDir`.
pub fn Run(pluginsDir: &Path) -> anyhow::Result<()> {
    let host = PluginHost::load_from_dir(pluginsDir);

    let protocol = match host.FindProtocol("uds") {
        Some(protocol) => protocol,
        None => {
            anyhow::bail!(
                "no 'uds' protocol plugin loaded from {}. Build it and copy the library in:\n  \
                 cargo build -p uds-plugin\n  \
                 cp target/debug/libuds_plugin.* {}/",
                pluginsDir.display(),
                pluginsDir.display()
            );
        }
    };

    let mut ecu = VirtualEcu::New(MakeDemoEcu());
    println!(
        "Virtual ECU '{}' started (session=0x01)\n",
        ecu.Config().m_strName
    );

    // A representative diagnostic session, each step showing request -> response.
    let vecScript: &[(&str, &[u8])] = &[
        ("ReadDataByIdentifier VIN", &[0x22, 0xF1, 0x90]),
        (
            "SecurityAccess seed (default session -> denied)",
            &[0x27, 0x01],
        ),
        ("DiagnosticSessionControl -> extended", &[0x10, 0x03]),
        ("SecurityAccess requestSeed", &[0x27, 0x01]),
        (
            "SecurityAccess sendKey",
            &[0x27, 0x02, 0xAA, 0xBB, 0xCC, 0xDD],
        ),
        ("ReadDTCInformation byStatusMask", &[0x19, 0x02, 0xFF]),
        ("ECUReset", &[0x11, 0x01]),
        ("TesterPresent", &[0x3E, 0x00]),
    ];

    for (strLabel, vecRequest) in vecScript {
        let vecResponse = ecu.ProcessRequest(&protocol, vecRequest);
        println!("{strLabel}");
        println!("  -> {}", FormatHex(vecRequest));
        if vecResponse.is_empty() {
            println!("  <- (positive response suppressed)");
        } else {
            println!("  <- {}", FormatHex(&vecResponse));
        }
        println!(
            "     [session=0x{:02X} securityUnlocked={}]\n",
            ecu.CurrentSession(),
            ecu.IsSecurityUnlocked()
        );
    }

    Ok(())
}

/// Build a small demo ECU with one DID, one DTC, and one security level.
fn MakeDemoEcu() -> Ecu {
    let mut ecu = Ecu::New("Engine_ECU", 0x1001);
    ecu.m_vecSupportedServices = vec![0x10, 0x11, 0x19, 0x22, 0x27, 0x31, 0x3E];
    ecu.m_vecSupportedSessions = vec![
        SessionType::Default,
        SessionType::Programming,
        SessionType::Extended,
    ];
    ecu.m_mapDids.insert(
        0xF190,
        DataIdentifier {
            m_u16Id: 0xF190,
            m_vecValue: b"VIN0123456789XYZ".to_vec(),
            m_confidence: Confidence::Observed,
        },
    );
    ecu.m_vecDtcs.push(DiagnosticTroubleCode {
        m_u32Code: 0x123456,
        m_byStatus: 0x2F,
        m_confidence: Confidence::Observed,
    });
    ecu.m_vecSecurityLevels.push(SecurityLevel {
        m_byRequestSeedSubFunction: 0x01,
        m_vecSeed: vec![0x11, 0x22, 0x33, 0x44],
        m_vecExpectedKey: vec![0xAA, 0xBB, 0xCC, 0xDD],
    });
    ecu
}

/// Format bytes as space-separated uppercase hex for readable console output.
fn FormatHex(vecBytes: &[u8]) -> String {
    vecBytes
        .iter()
        .map(|byByte| format!("{byByte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

//! Read a real capture file from disk, if one is present.
//!
//! Skipped when the file is absent so CI stays green — this is a local cross-check against an
//! independently produced capture, not something the repo can carry.

#![allow(non_snake_case, non_upper_case_globals)]

#[test]
fn the_local_reference_capture_reads_if_it_is_present() {
    let strPath = format!(
        "{}/.claude/doip-expert/reference/sample-doip.pcap",
        std::env::var("HOME").unwrap_or_default()
    );
    let vecBytes = match std::fs::read(&strPath) {
        Ok(vecBytes) => vecBytes,
        Err(_) => {
            eprintln!("skipping: no reference capture at {strPath}");
            return;
        }
    };

    let vecPackets = pcap::ReadCapture(&vecBytes).expect("the reference capture should read");
    eprintln!(
        "read {} packets from the reference capture",
        vecPackets.len()
    );

    assert_eq!(
        vecPackets.len(),
        17,
        "the reference capture holds 17 packets"
    );
    assert!(
        vecPackets.iter().all(|packet| packet.TouchesPort(13400)),
        "every packet in it is DoIP traffic"
    );
}

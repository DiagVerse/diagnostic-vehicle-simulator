//! Finding out how fast the link to an adapter actually runs.
//!
//! The line speed between the host and a USB-CAN dongle is not the CAN bitrate and is not
//! discoverable from the operating system: a USB-to-UART bridge reports no baud rate, it
//! merely generates whichever one it is told to. The only way to learn the adapter's is to
//! talk to it at a candidate speed and see whether it answers sense.
//!
//! Why it matters enough to probe rather than assume: at 115200 baud an SLCAN line for a
//! 29-bit frame takes 2.34 ms to cross, so the link carries about 427 frames per second, while
//! a 500 kbit/s CAN bus delivers roughly 3600. A long multi-frame request sent back to back
//! overruns the dongle and arrives with holes in it. Running the line as fast as the adapter
//! will go is what removes that gap.

#![allow(non_snake_case, non_upper_case_globals)]

use std::time::{Duration, Instant};

use serial_can::SerialTransport;

/// Line speeds to try, fastest first.
///
/// Fastest first is safe because the reply is checked for shape, not merely for presence: a
/// UART reading at the wrong speed delivers mangled bytes, and mangled bytes are not a version
/// reply. The list is the set commonly built into SLCAN firmware; an adapter running something
/// exotic is handled by naming the rate explicitly instead.
pub const c_arrCandidateBaudRates: [u32; 7] = [
    1_000_000, 921_600, 500_000, 460_800, 250_000, 230_400, 115_200,
];

/// What to fall back to when nothing answers.
///
/// The rate this engine used unconditionally before it learned to ask, so a link that cannot
/// be probed behaves exactly as it always did.
pub const c_u32FallbackBaudRate: u32 = 115_200;

/// How long to wait for one candidate to answer.
///
/// An adapter replies to `V` within a millisecond or two; this is generous enough to survive a
/// busy host and short enough that walking the whole list stays about a second.
const c_probeTimeout: Duration = Duration::from_millis(150);

/// How the line speed in use was arrived at. Reported so an operator can tell a measured
/// number from a guess — the two call for very different next steps when the link misbehaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaudRateSource {
    /// The adapter answered at this speed.
    Detected,
    /// The caller named it, and it was not questioned.
    Requested,
    /// Nothing answered at any candidate speed, so the fallback is in use.
    NoAdapterReply,
    /// There is no UART on this link — a pseudo-terminal or an in-memory pipe — so the number
    /// is meaningless and only recorded to have something to report.
    NoRealSerialLine,
}

impl BaudRateSource {
    /// A phrase for a log line or the UI.
    pub fn Describe(self) -> &'static str {
        match self {
            BaudRateSource::Detected => "detected from the adapter",
            BaudRateSource::Requested => "set by the operator",
            BaudRateSource::NoAdapterReply => "assumed; the adapter did not answer the probe",
            BaudRateSource::NoRealSerialLine => "not applicable; this link has no UART",
        }
    }
}

/// The line speed a link will be opened at, and how that was decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialLinkSpeed {
    /// Bits per second between host and adapter. Not the CAN bitrate.
    pub m_u32BaudRate: u32,
    /// Where the number came from.
    pub m_source: BaudRateSource,
    /// What the adapter said when it identified itself, if it did.
    pub m_optStrAdapterVersion: Option<String>,
}

/// Work out how fast to talk to the adapter on a port.
///
/// Tries each candidate speed in turn, opening the port, asking the adapter for its version
/// and checking the shape of what comes back. The first speed that produces a well-formed
/// reply is the adapter's. Nothing is left open: the port is reopened by the caller once the
/// answer is known, which keeps this function free of any ownership of the live link.
///
/// Never fails. A port that cannot be opened, an adapter that does not implement `V`, and a
/// pseudo-terminal with no UART behind it all resolve to the fallback speed with the reason
/// recorded, because refusing to connect over an unanswered probe would be a worse outcome
/// than connecting the way this engine always used to.
pub fn DetectSerialLinkSpeed(strPortName: &str) -> SerialLinkSpeed {
    for u32Candidate in c_arrCandidateBaudRates {
        let mut boxTransport = match serial_can::OpenPort(strPortName, u32Candidate) {
            Ok(boxTransport) => boxTransport,
            Err(error) => {
                tracing::debug!(
                    port = %strPortName,
                    baud = u32Candidate,
                    %error,
                    "could not open the port to probe it"
                );
                continue;
            }
        };

        // A link with no UART cannot be probed and does not need to be. Checked after the open
        // rather than before because only the open reveals which kind of port this is.
        if !boxTransport.HasConfigurableBaudRate() {
            tracing::info!(
                port = %strPortName,
                "no UART on this link, so its line speed is not a real number and was not probed"
            );
            return SerialLinkSpeed {
                m_u32BaudRate: c_u32FallbackBaudRate,
                m_source: BaudRateSource::NoRealSerialLine,
                m_optStrAdapterVersion: None,
            };
        }

        match AskForVersion(boxTransport.as_mut()) {
            Some(strVersion) => {
                tracing::info!(
                    port = %strPortName,
                    baud = u32Candidate,
                    adapter = %strVersion,
                    "adapter answered; using this line speed"
                );
                return SerialLinkSpeed {
                    m_u32BaudRate: u32Candidate,
                    m_source: BaudRateSource::Detected,
                    m_optStrAdapterVersion: Some(strVersion),
                };
            }
            None => tracing::debug!(
                port = %strPortName,
                baud = u32Candidate,
                "no version reply at this line speed"
            ),
        }
    }

    tracing::warn!(
        port = %strPortName,
        baud = c_u32FallbackBaudRate,
        candidates = c_arrCandidateBaudRates.len(),
        "no line speed produced a version reply; falling back to the rate this engine used before it probed"
    );
    SerialLinkSpeed {
        m_u32BaudRate: c_u32FallbackBaudRate,
        m_source: BaudRateSource::NoAdapterReply,
        m_optStrAdapterVersion: None,
    }
}

/// The speed the caller asked for, recorded without questioning it.
pub fn RequestedSerialLinkSpeed(u32BaudRate: u32) -> SerialLinkSpeed {
    SerialLinkSpeed {
        m_u32BaudRate: u32BaudRate,
        m_source: BaudRateSource::Requested,
        m_optStrAdapterVersion: None,
    }
}

/// Ask one open transport to identify itself, returning its version line if it does.
///
/// The channel is closed first. An adapter left open by a previous run streams received frames
/// continuously, and a probe that had to pick its answer out of that traffic would be far less
/// certain than one talking to a quiet line.
fn AskForVersion(transport: &mut dyn SerialTransport) -> Option<String> {
    if transport.Write(slcan::CloseCommand().as_bytes()).is_err() {
        return None;
    }
    DrainFor(transport, Duration::from_millis(20));

    // Ask every way we know. A firmware that does not implement one of these often implements
    // another, and one unanswered command is the difference between a measured line speed and
    // an assumed one.
    for strCommand in slcan::IdentityCommands() {
        if transport.Write(strCommand.as_bytes()).is_err() {
            return None;
        }
    }

    // Accumulated across reads, not per read. An adapter's reply routinely arrives split in
    // two, and a probe that only looked inside a single read would call a working line speed a
    // failure roughly whenever the split fell mid-reply.
    let mut vecReceived: Vec<u8> = Vec::new();
    let mut arrBuffer = [0u8; 256];
    let deadline = Instant::now() + c_probeTimeout;

    while Instant::now() < deadline {
        let uCount = match transport.Read(&mut arrBuffer) {
            Ok(uCount) => uCount,
            Err(_) => return None,
        };
        if uCount == 0 {
            continue;
        }

        vecReceived.extend_from_slice(&arrBuffer[..uCount]);
        if let Some(strVersion) = FindVersionReply(&vecReceived) {
            return Some(strVersion);
        }

        // A reply cannot be longer than a line, so anything beyond that is noise from the
        // wrong line speed and is dropped rather than accumulated forever.
        if vecReceived.len() > slcan::c_uMaxLineLength {
            return None;
        }
    }

    None
}

/// Find a well-formed version reply among complete lines received so far.
fn FindVersionReply(vecReceived: &[u8]) -> Option<String> {
    for vecLine in vecReceived.split(|byByte| *byByte == slcan::c_byTerminator) {
        let strLine = String::from_utf8_lossy(vecLine);
        if slcan::IsIdentityReply(&strLine) {
            return Some(strLine.into_owned());
        }
    }
    None
}

/// Read and discard whatever arrives for a short while, so a probe starts on a quiet line.
fn DrainFor(transport: &mut dyn SerialTransport, duration: Duration) {
    let mut arrBuffer = [0u8; 256];
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        match transport.Read(&mut arrBuffer) {
            Ok(_) => {}
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_can::loopback::LoopbackTransport;

    #[test]
    fn an_in_memory_link_reports_that_its_speed_is_not_a_real_number() {
        // A loopback has no UART, so it must not be probed and must not be described as if a
        // measurement had been taken.
        let source = BaudRateSource::NoRealSerialLine;
        assert_eq!(source.Describe(), "not applicable; this link has no UART");
    }

    #[test]
    fn an_adapter_that_identifies_itself_is_recognised() {
        let (mut nearEnd, mut farEnd) = LoopbackTransport::NewPair();

        // The far end plays an adapter, and must answer *after* the probe has asked: the probe
        // drains the line first, so a reply queued up front would be thrown away — which is
        // the correct behaviour and worth not writing a test that hides it.
        let joinFarEnd = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            farEnd.Write(b"V1013\r").expect("the far end writes");
        });

        let optStrVersion = AskForVersion(&mut nearEnd);
        joinFarEnd.join().expect("the far end thread");
        assert_eq!(optStrVersion, Some("V1013".to_string()));
    }

    #[test]
    fn a_reply_split_across_two_reads_is_still_recognised() {
        let (mut nearEnd, mut farEnd) = LoopbackTransport::NewPair();

        let joinFarEnd = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            farEnd.Write(b"V10").expect("first half");
            std::thread::sleep(Duration::from_millis(20));
            farEnd.Write(b"13\r").expect("second half");
        });

        let optStrVersion = AskForVersion(&mut nearEnd);
        joinFarEnd.join().expect("the far end thread");
        assert_eq!(
            optStrVersion,
            Some("V1013".to_string()),
            "a reply arriving in two pieces is the normal case on a real UART"
        );
    }

    #[test]
    fn mangled_bytes_from_the_wrong_line_speed_are_not_mistaken_for_a_reply() {
        let (mut nearEnd, mut farEnd) = LoopbackTransport::NewPair();

        // What a UART delivers when the baud rate is wrong: framing noise, still terminated.
        let joinFarEnd = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            farEnd
                .Write(&[0xFE, 0xC0, 0x80, 0xFF, b'\r'])
                .expect("noise");
        });

        let optStrVersion = AskForVersion(&mut nearEnd);
        joinFarEnd.join().expect("the far end thread");
        assert_eq!(optStrVersion, None);
    }

    #[test]
    fn a_requested_speed_is_recorded_as_the_operators_choice_not_a_measurement() {
        let speed = RequestedSerialLinkSpeed(500_000);
        assert_eq!(speed.m_u32BaudRate, 500_000);
        assert_eq!(speed.m_source, BaudRateSource::Requested);
        assert_eq!(speed.m_optStrAdapterVersion, None);
    }
}

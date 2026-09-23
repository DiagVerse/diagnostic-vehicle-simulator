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

/// How long to let an adapter reconfigure its own UART after acknowledging a `U` command.
///
/// It acknowledges at the old speed and switches afterwards, so reopening the port immediately
/// can catch it mid-change and read the acknowledgement's own trailing bits as noise.
const c_lineSpeedSettleTime: Duration = Duration::from_millis(100);

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

/// What happened when an adapter was asked to change its own UART speed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineSpeedChange {
    /// The adapter acknowledged, and answered at the new speed. The link is faster.
    Confirmed {
        /// The speed now in use, in bits per second.
        u32BaudRate: u32,
        /// What it said when asked to identify itself at the new speed.
        strAdapterVersion: String,
    },
    /// The adapter answered BEL: this firmware does not offer that speed. Nothing changed.
    Refused {
        /// The speed that was asked for.
        u32BaudRate: u32,
    },
    /// The adapter said nothing, so the change could not be confirmed, and the link was put
    /// back the way it was.
    ///
    /// The important half is the putting back. An unconfirmed change leaves two possibilities —
    /// the adapter switched and is merely mute, or it never switched at all — and the host
    /// cannot tell them apart. Guessing wrong means talking at the wrong line speed, which
    /// delivers garbage in both directions: far worse than staying slow.
    NotConfirmed {
        /// The speed that was asked for and could not be verified.
        u32BaudRate: u32,
        /// The speed the link is still running at.
        u32RevertedToBaudRate: u32,
    },
    /// There is no UART on this link — a pseudo-terminal or an in-memory pipe — so there is
    /// nothing to command.
    NoRealSerialLine,
}

impl LineSpeedChange {
    /// A sentence for a log line or the UI.
    pub fn Describe(&self) -> String {
        match self {
            LineSpeedChange::Confirmed {
                u32BaudRate,
                strAdapterVersion,
            } => format!("the adapter is now running at {u32BaudRate} baud ({strAdapterVersion})"),
            LineSpeedChange::Refused { u32BaudRate } => {
                format!("the adapter refused {u32BaudRate} baud; its firmware does not offer it")
            }
            LineSpeedChange::NotConfirmed {
                u32BaudRate,
                u32RevertedToBaudRate,
            } => format!(
                "the adapter did not answer at {u32BaudRate} baud, so the link was put back to \
                 {u32RevertedToBaudRate}; this adapter cannot confirm a speed change"
            ),
            LineSpeedChange::NoRealSerialLine => {
                "this link has no UART, so there is no line speed to command".to_string()
            }
        }
    }

    /// The speed the link ended up at, whatever the outcome.
    pub fn EffectiveBaudRate(&self, u32CurrentBaudRate: u32) -> u32 {
        match self {
            LineSpeedChange::Confirmed { u32BaudRate, .. } => *u32BaudRate,
            LineSpeedChange::Refused { .. } => u32CurrentBaudRate,
            LineSpeedChange::NotConfirmed {
                u32RevertedToBaudRate,
                ..
            } => *u32RevertedToBaudRate,
            LineSpeedChange::NoRealSerialLine => u32CurrentBaudRate,
        }
    }
}

/// Ask an adapter to run its UART faster, and only believe it if it answers at the new speed.
///
/// Probing finds the speed an adapter is *already* using; this changes it. That is the
/// difference between discovering a 115200-baud link and turning it into a 230400-baud one,
/// and on a link that is the bottleneck by a factor of eight it is the cheapest fix available.
///
/// The sequence, and why each step is there:
///
/// 1. Send `U<n>` at the speed already in use. The adapter acknowledges at the **old** speed
///    and only then switches, so the acknowledgement must be read before the port is reopened.
/// 2. A BEL means this firmware does not offer that speed. Nothing changed, and the caller is
///    told which speed was refused rather than a bare failure.
/// 3. Reopen the host's port at the new speed and ask the adapter to identify itself. Only a
///    well-formed reply counts — at the wrong line speed a UART delivers mangled bytes, and
///    `IsIdentityReply` is strict precisely so those cannot be mistaken for success.
/// 4. If nothing answers, **put the port back**. See `LineSpeedChange::NotConfirmed`.
///
/// Note for anyone extending this: on several firmwares the setting survives a power cycle, so
/// an adapter left at a raised speed will not talk to other software that assumes 115200 until
/// it is set back. That is why this is an explicit action and never happens on its own.
pub fn CommandLineSpeed(
    strPortName: &str,
    u32CurrentBaudRate: u32,
    speed: slcan::SlcanLineSpeed,
) -> LineSpeedChange {
    let u32TargetBaudRate = speed.ToBitsPerSecond();

    let mut boxTransport = match serial_can::OpenPort(strPortName, u32CurrentBaudRate) {
        Ok(boxTransport) => boxTransport,
        Err(error) => {
            tracing::warn!(port = %strPortName, %error, "could not open the port to command its line speed");
            return LineSpeedChange::NotConfirmed {
                u32BaudRate: u32TargetBaudRate,
                u32RevertedToBaudRate: u32CurrentBaudRate,
            };
        }
    };

    if !boxTransport.HasConfigurableBaudRate() {
        tracing::info!(
            port = %strPortName,
            "no UART on this link, so its line speed cannot be commanded"
        );
        return LineSpeedChange::NoRealSerialLine;
    }

    // A channel left open by a previous run streams frames continuously, and an
    // acknowledgement picked out of that traffic would not be one.
    let _ = boxTransport.Write(slcan::CloseCommand().as_bytes());
    DrainFor(boxTransport.as_mut(), Duration::from_millis(20));

    if boxTransport
        .Write(slcan::LineSpeedCommand(speed).as_bytes())
        .is_err()
    {
        return LineSpeedChange::NotConfirmed {
            u32BaudRate: u32TargetBaudRate,
            u32RevertedToBaudRate: u32CurrentBaudRate,
        };
    }

    match ReadAcknowledgement(boxTransport.as_mut()) {
        Some(false) => {
            tracing::info!(
                port = %strPortName,
                baud = u32TargetBaudRate,
                "the adapter refused this line speed"
            );
            return LineSpeedChange::Refused {
                u32BaudRate: u32TargetBaudRate,
            };
        }
        Some(true) => {}
        // No answer to the command itself. The adapter may still have switched, so the
        // verification below is attempted rather than skipped — a mute acknowledgement and a
        // mute adapter are not the same thing, and only the reopen can tell them apart.
        None => tracing::debug!(
            port = %strPortName,
            "no acknowledgement of the line speed command; trying the new speed anyway"
        ),
    }

    // The port must be closed before it is reopened at another speed, and the adapter needs a
    // moment to reconfigure its own UART after acknowledging.
    drop(boxTransport);
    std::thread::sleep(c_lineSpeedSettleTime);

    let mut boxAtNewSpeed = match serial_can::OpenPort(strPortName, u32TargetBaudRate) {
        Ok(boxAtNewSpeed) => boxAtNewSpeed,
        Err(error) => {
            tracing::warn!(port = %strPortName, %error, "could not reopen the port at the new line speed");
            return LineSpeedChange::NotConfirmed {
                u32BaudRate: u32TargetBaudRate,
                u32RevertedToBaudRate: u32CurrentBaudRate,
            };
        }
    };

    match AskForVersion(boxAtNewSpeed.as_mut()) {
        Some(strAdapterVersion) => {
            tracing::info!(
                port = %strPortName,
                baud = u32TargetBaudRate,
                adapter = %strAdapterVersion,
                "the adapter answered at the new line speed"
            );
            LineSpeedChange::Confirmed {
                u32BaudRate: u32TargetBaudRate,
                strAdapterVersion,
            }
        }
        None => {
            // Put it back. This is the whole reason the operation is safe to offer: an
            // unconfirmed speed is indistinguishable from a wrong one, and running at a wrong
            // one delivers garbage rather than slowness.
            drop(boxAtNewSpeed);
            RevertLineSpeed(strPortName, u32CurrentBaudRate);
            tracing::warn!(
                port = %strPortName,
                baud = u32TargetBaudRate,
                revertedTo = u32CurrentBaudRate,
                "no reply at the new line speed; the adapter was put back"
            );
            LineSpeedChange::NotConfirmed {
                u32BaudRate: u32TargetBaudRate,
                u32RevertedToBaudRate: u32CurrentBaudRate,
            }
        }
    }
}

/// Command an adapter back to the speed it was on, best effort.
///
/// Sent at the speed we asked it to move to, because that is where it is if it moved — and if
/// it did not move, it is not listening at that speed and the command is harmlessly lost. Any
/// failure here is deliberately not reported: the caller is already handling a failure, and a
/// second error about the cleanup would bury the first.
fn RevertLineSpeed(strPortName: &str, u32OriginalBaudRate: u32) {
    let speed = match slcan::SlcanLineSpeed::FromBitsPerSecond(u32OriginalBaudRate) {
        Some(speed) => speed,
        // The original speed is not one `U` can name — 921600, say, reached some other way.
        // There is no command that returns the adapter to it.
        None => {
            tracing::warn!(
                port = %strPortName,
                baud = u32OriginalBaudRate,
                "the original line speed is not one the U command can select, so it cannot be restored"
            );
            return;
        }
    };

    for u32At in [c_u32FallbackBaudRate, u32OriginalBaudRate] {
        if let Ok(mut boxTransport) = serial_can::OpenPort(strPortName, u32At) {
            let _ = boxTransport.Write(slcan::LineSpeedCommand(speed).as_bytes());
            DrainFor(boxTransport.as_mut(), Duration::from_millis(20));
        }
    }
}

/// Wait for a single-byte acknowledgement or refusal.
fn ReadAcknowledgement(transport: &mut dyn SerialTransport) -> Option<bool> {
    let mut vecReceived: Vec<u8> = Vec::new();
    let mut arrBuffer = [0u8; 64];
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

        if let Some(bIsAccepted) = slcan::ReadAcknowledgement(&vecReceived) {
            return Some(bIsAccepted);
        }
        if vecReceived.len() > slcan::c_uMaxLineLength {
            return None;
        }
    }
    None
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

    // ------------------------------------------------------ commanding a faster UART

    #[test]
    fn the_command_digits_are_lawicels_and_map_to_real_speeds() {
        // Wrong digits here are silent: the adapter accepts the command and runs at a speed
        // nobody asked for, and every frame after that is garbage.
        assert_eq!(slcan::SlcanLineSpeed::Baud230400.ToCommandDigit(), '0');
        assert_eq!(slcan::SlcanLineSpeed::Baud115200.ToCommandDigit(), '1');
        assert_eq!(
            slcan::LineSpeedCommand(slcan::SlcanLineSpeed::Baud230400),
            "U0\r"
        );
    }

    #[test]
    fn a_speed_the_command_cannot_name_is_refused_rather_than_rounded() {
        // 921600 is a real rate an adapter may already run at, and `U` cannot select it.
        // Rounding down to 230400 and reporting success would leave an operator believing the
        // link is four times faster than it is — and blaming the simulator for what goes
        // missing.
        assert_eq!(slcan::SlcanLineSpeed::FromBitsPerSecond(921_600), None);
        assert_eq!(
            slcan::SlcanLineSpeed::FromBitsPerSecond(230_400),
            Some(slcan::SlcanLineSpeed::Baud230400)
        );
    }

    #[test]
    fn nothing_yet_is_not_a_refusal() {
        // Both answers are one byte and there is no third. Reading "no bytes so far" as BEL is
        // how a working adapter gets reported as unsupported on a host that read too early.
        assert_eq!(slcan::ReadAcknowledgement(b""), None);
        assert_eq!(slcan::ReadAcknowledgement(b"\r"), Some(true));
        assert_eq!(slcan::ReadAcknowledgement(&[slcan::c_byBell]), Some(false));
        assert_eq!(
            slcan::ReadAcknowledgement(&[b'z', slcan::c_byBell]),
            Some(false),
            "a refusal still counts when it arrives behind leftover noise"
        );
    }

    #[test]
    fn an_acknowledgement_is_read_back_from_the_adapter() {
        let (mut nearEnd, mut farEnd) = LoopbackTransport::NewPair();
        let joinFarEnd = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            farEnd.Write(b"\r").expect("the adapter acknowledges");
        });

        let optAccepted = ReadAcknowledgement(&mut nearEnd);
        joinFarEnd.join().expect("the far end thread");
        assert_eq!(optAccepted, Some(true));
    }

    #[test]
    fn a_bell_is_read_as_a_refusal() {
        let (mut nearEnd, mut farEnd) = LoopbackTransport::NewPair();
        let joinFarEnd = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            farEnd
                .Write(&[slcan::c_byBell])
                .expect("the adapter refuses");
        });

        let optAccepted = ReadAcknowledgement(&mut nearEnd);
        joinFarEnd.join().expect("the far end thread");
        assert_eq!(
            optAccepted,
            Some(false),
            "a firmware that does not offer this speed says so, and must not be read as silence"
        );
    }

    #[test]
    fn a_mute_adapter_leaves_the_link_where_it_was() {
        // The property that makes this safe to offer at all. An unconfirmed speed change is
        // indistinguishable from a failed one, and guessing wrong means the host talks at a
        // rate the adapter is not using — which delivers garbage in both directions rather
        // than merely being slow.
        //
        // A named pseudo-terminal that does not exist stands in for the mute adapter: nothing
        // answers, which is exactly the case being pinned.
        let change = CommandLineSpeed(
            "/dev/null-no-such-adapter",
            115_200,
            slcan::SlcanLineSpeed::Baud230400,
        );

        match change {
            LineSpeedChange::NotConfirmed {
                u32BaudRate,
                u32RevertedToBaudRate,
            } => {
                assert_eq!(u32BaudRate, 230_400, "what was asked for");
                assert_eq!(u32RevertedToBaudRate, 115_200, "and what is still in use");
            }
            other => panic!("expected the link to be left alone, got {other:?}"),
        }
        assert_eq!(
            change.EffectiveBaudRate(115_200),
            115_200,
            "the caller keeps talking at the speed that works"
        );
    }

    #[test]
    fn every_outcome_says_what_happened_in_words() {
        // These reach an operator who has just pressed a button and needs to know whether to
        // press it again, try a slower speed, or stop.
        let confirmed = LineSpeedChange::Confirmed {
            u32BaudRate: 230_400,
            strAdapterVersion: "V1013".to_string(),
        };
        assert!(confirmed.Describe().contains("230400"));

        let refused = LineSpeedChange::Refused {
            u32BaudRate: 230_400,
        };
        assert!(refused.Describe().contains("does not offer"));

        let notConfirmed = LineSpeedChange::NotConfirmed {
            u32BaudRate: 230_400,
            u32RevertedToBaudRate: 115_200,
        };
        let strWords = notConfirmed.Describe();
        assert!(strWords.contains("put back"), "got: {strWords}");
        assert!(strWords.contains("115200"), "and names where it ended up");
    }
}

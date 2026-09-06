//! The shape of a simulation file.
//!
//! Deliberately its own types rather than the vehicle model's. The model serializes with
//! Hungarian field names, which is right for an internal document and wrong for one a person
//! types: a simfile is written by hand, so its field names are the ones a person would guess.
//! It is also a stable contract — the model can be refactored without breaking files people
//! have written — and it can accept conveniences the model has no business knowing about,
//! like a DTC written `P0123-00` or a value written as text.

#![allow(non_snake_case, non_upper_case_globals)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The version this engine writes. Bumped for a change that adds expressive power.
///
/// Version 2 added vehicle architecture: an ECU may declare itself the gateway for other
/// networks, a network may declare itself the link a tester attaches to, and addressing became
/// a superset — an ECU carries CAN identifiers, a DoIP logical address, or both.
pub const c_uCurrentVersion: u32 = 2;

/// The oldest version this engine still reads. Version 1 files describe a flat CAN vehicle,
/// which is a valid version 2 vehicle that happens to declare no architecture, so they keep
/// working unchanged.
pub const c_uMinSupportedVersion: u32 = 1;

/// A whole vehicle, as written in a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimFileDto {
    /// Which version of this format the file is written in.
    pub simfile_version: u32,
    /// What to call the vehicle.
    pub vehicle: String,
    /// The buses. Optional: a file may describe ECUs without saying how they are wired.
    #[serde(default)]
    pub networks: Vec<NetworkDto>,
    /// The ECUs.
    pub ecus: Vec<EcuDto>,
    /// What the vehicle tells a DoIP tester about itself. Left out, nothing is programmed —
    /// which is a real state, and announced as such rather than as a plausible-looking VIN.
    #[serde(default)]
    pub identity: Option<IdentityDto>,
}

/// How a vehicle identifies itself over DoIP (ISO 13400-2 Table 5).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityDto {
    /// The 17-character VIN, as text.
    #[serde(default)]
    pub vin: Option<String>,
    /// The entity identification: six bytes in hex, conventionally a MAC address.
    #[serde(default)]
    pub eid: Option<String>,
    /// The group identification: six bytes in hex, shared by every entity of one vehicle.
    #[serde(default)]
    pub gid: Option<String>,
    /// ISO 13400-2 Table 6, in hex. `00` no further action; `10` central security required.
    #[serde(default)]
    pub further_action: Option<String>,
    /// ISO 13400-2 Table 7, in hex. `00` synchronized; `10` not — which tells a tester to wait
    /// and ask again, and is worth being able to inject.
    #[serde(default)]
    pub vin_gid_sync_status: Option<String>,
}

/// One bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkDto {
    /// The key ECUs refer to this bus by.
    pub id: String,
    /// What to call it on screen.
    pub name: String,
    /// `"CAN"`, `"CAN-FD"` or `"Ethernet"`.
    pub kind: String,
    /// True for a link a tester attaches to directly — the diagnostic socket, or the Ethernet
    /// interface a DoIP tester opens.
    ///
    /// Left out on every network, the engine treats each link nothing gateways onto as an
    /// entry point, so a file that does not model gateways needs no entry point either.
    #[serde(default)]
    pub entry_point: bool,
    /// Arbitration bit rate. Left out means unknown, which is rendered as unknown rather than
    /// filled in with a plausible default.
    #[serde(default)]
    pub bitrate_bps: Option<u32>,
    /// The CAN-FD data-phase bit rate, for a link that has one.
    #[serde(default)]
    pub data_bitrate_bps: Option<u32>,
}

/// One ECU.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EcuDto {
    /// What to call it: "Engine", "BCM", "ABS".
    pub name: String,
    /// The id of the bus it sits on. Left out means nobody has said.
    #[serde(default)]
    pub network: Option<String>,
    /// The networks this ECU forwards diagnostics onto, making it a gateway.
    ///
    /// This is what gives a vehicle depth: an ECU on one of these networks is reached by the
    /// tester only through this one. A gateway is usually on an Ethernet link and forwards
    /// onto several CAN segments, but nothing here requires that — it forwards onto whatever
    /// it says it does.
    #[serde(default)]
    pub gateway_for: Vec<String>,
    /// How a tester addresses it on CAN. Left out for an ECU reachable only over DoIP.
    #[serde(default)]
    pub can: Option<CanAddressDto>,
    /// How a tester addresses it over DoIP. Left out for an ECU reachable only over CAN.
    #[serde(default)]
    pub doip: Option<DoIpAddressDto>,
    /// Version 1 spelling of `can.request`. Kept so existing files load unchanged.
    #[serde(default)]
    pub request_can_id: Option<String>,
    /// Version 1 spelling of `can.response`.
    #[serde(default)]
    pub response_can_id: Option<String>,
    /// Version 1 spelling of `can.addressing`.
    #[serde(default)]
    pub addressing: Option<String>,
    /// Version 1 spelling of `doip.logicalAddress`, as a decimal number.
    #[serde(default)]
    pub logical_address: Option<u16>,
    /// Sessions it can enter: `"default"`, `"programming"`, `"extended"`, `"safety"`.
    #[serde(default)]
    pub sessions: Vec<String>,
    /// Which services are reachable in which session, keyed by session name.
    ///
    /// Left out, every supported service works in every session. A session listed here is
    /// restricted to what it lists, and anything else is refused with NRC 0x7F — which is how
    /// a real ECU keeps flashing and actuation out of the default session. A session *not*
    /// mentioned stays unrestricted, so locking down `extended` does not silently lock
    /// `default` too.
    #[serde(default)]
    pub session_services: BTreeMap<String, Vec<String>>,
    /// Service identifiers it supports, in hex. Left out, it gets the ones the engine's UDS
    /// plugin implements.
    #[serde(default)]
    pub services: Option<Vec<String>>,
    /// Data identifiers it answers, keyed by DID in hex.
    #[serde(default)]
    pub dids: BTreeMap<String, ValueDto>,
    /// Trouble codes it reports.
    #[serde(default)]
    pub dtcs: Vec<DtcDto>,
    /// Security levels it offers.
    #[serde(default)]
    pub security: Vec<SecurityDto>,
    /// Its timing, if it should differ from the defaults.
    #[serde(default)]
    pub timing: Option<TimingDto>,
    /// Answers to particular requests, for services the engine does not implement or for
    /// behaviour that differs from the default.
    #[serde(default)]
    pub responses: Vec<ResponseDto>,
}

/// How a tester reaches an ECU on CAN.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanAddressDto {
    /// The identifier a tester addresses it on, in hex.
    pub request: String,
    /// The identifier it answers on, in hex.
    pub response: String,
    /// `"Normal11Bit"` or `"NormalFixed29Bit"`. Left out, it follows from the identifier width.
    #[serde(default)]
    pub addressing: Option<String>,
    /// A broadcast identifier it also listens on, in hex. Left out, the legislated 0x7DF is
    /// used for an 11-bit ECU in the OBD range and nothing is assumed for anything else.
    #[serde(default)]
    pub functional: Option<String>,
}

/// How a tester reaches an ECU over DoIP.
///
/// Declaring this makes the ECU part of the vehicle's architecture and puts it in the topology
/// diagram. The engine's wire-level simulation is CAN today, so an ECU with only this is shown
/// as declared but not yet driveable rather than silently dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DoIpAddressDto {
    /// The ISO 13400 logical address, in hex: `"0x1056"`.
    pub logical_address: String,
}

/// A byte string, written either as hex or as text.
///
/// A bare string is hex, which is what diagnostics work in. Text has to say so, because
/// `"0110"` is a perfectly good pair of bytes *and* a perfectly good piece of text, and
/// guessing which was meant is exactly the kind of silent wrong answer this format should not
/// produce.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ValueDto {
    /// `"01 02 03"` — hex bytes.
    Hex(String),
    /// `{ "text": "1HGCM82633A004352" }` — characters, stored as their bytes.
    Text {
        /// The characters to store.
        text: String,
    },
}

/// One trouble code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DtcDto {
    /// `"P0123"`, `"P0123-00"` or a raw `"0x012300"`.
    pub code: String,
    /// The status-of-DTC byte in hex. Left out means confirmed and stored.
    #[serde(default)]
    pub status: Option<String>,
}

/// One security level.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecurityDto {
    /// The requestSeed sub-function, in hex. Must be odd: even values are sendKey.
    pub request_seed: String,
    /// The seed this level hands out.
    pub seed: String,
    /// The key it expects back.
    pub key: String,
}

/// Timing overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimingDto {
    #[serde(default)]
    pub p2_ms: Option<u32>,
    #[serde(default)]
    pub p2_star_ms: Option<u32>,
    #[serde(default)]
    pub p4_ms: Option<u32>,
    #[serde(default)]
    pub response_delay_ms: Option<u32>,
}

/// One answer to one request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResponseDto {
    /// The request bytes to match. A byte written `**` is a wildcard.
    pub request: String,
    /// The bytes to answer with. Left out means answer with silence.
    #[serde(default)]
    pub response: Option<String>,
    /// Treat `request` as a prefix and accept anything longer.
    ///
    /// Off by default, so a pattern matches only a request of exactly its length — which is
    /// what you want for a fixed-shape request like `3E 00`. Turn it on for the services whose
    /// length is not fixed: `2E` carries a value of any size, `36` a block of any size, `31`
    /// may carry routine parameters. Without it those can only ever be answered for one
    /// particular length, which is not a simulation of the service at all.
    #[serde(default)]
    pub match_trailing_bytes: bool,
    /// Runs of request bytes copied into the response after it is built.
    ///
    /// Real positive responses echo parts of the request — the DID in a `0x6E`, the block
    /// sequence counter in a `0x76`, the routine identifier in a `0x71`. With a wildcard
    /// pattern the response would otherwise have to hard-code one value and answer every
    /// request with it, which a tester checking its own echo will catch immediately.
    #[serde(default)]
    pub echo: Vec<EchoSpanDto>,
    /// Why this answer exists, for whoever reads the file next.
    #[serde(default)]
    pub note: String,
}

/// One run of request bytes copied into the response.
///
/// Named to match the same concept on the HTTP boundary and in the UI, so the three describe
/// one thing in one vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EchoSpanDto {
    /// Where the run starts in the request, counting the service identifier as byte 0.
    pub request_offset: usize,
    /// How many bytes to copy.
    pub length: usize,
    /// Where the run lands in the response, counting its service identifier as byte 0.
    pub response_offset: usize,
}

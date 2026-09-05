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

/// The format version. Bumped only for a change that would break existing files.
pub const c_uCurrentVersion: u32 = 1;

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
    /// The identifier a tester addresses it on, in hex.
    pub request_can_id: String,
    /// The identifier it answers on, in hex.
    pub response_can_id: String,
    /// `"Normal11Bit"` or `"NormalFixed29Bit"`. Left out, it follows from the identifier width.
    #[serde(default)]
    pub addressing: Option<String>,
    /// Its DoIP logical address, if it has one worth recording.
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
    /// Why this answer exists, for whoever reads the file next.
    #[serde(default)]
    pub note: String,
}

//! DoIP — Diagnostics over IP (ISO 13400-2:2019).
//!
//! The message codec: generic header, payload types and the payloads themselves. No sockets and
//! no state — this crate turns bytes into messages and back, so the wire format can be tested
//! exhaustively without a network, and so the server that does own sockets has nothing to do
//! but decide.
//!
//! On the wire DoIP replaces ISO-TP rather than UDS: a diagnostic message carries a whole
//! ISO 14229-1 payload with no segmentation, because TCP has already done that job.
//!
//! Requirement identifiers in the doc comments are ISO 13400-2:2019's own, of the form
//! `REQ <layer>.DoIP-<n> <LAYER>`, so a reader can find the sentence being implemented.

#![allow(non_snake_case, non_upper_case_globals)]

pub mod connection;
pub mod header;
pub mod messages;
pub mod payload;
pub mod timing;

pub use connection::{Connection, ConnectionState};
pub use header::{GenericHeader, HeaderLimits, HeaderNack, ReadHeader, WriteMessage};
pub use payload::PayloadType;

/// The unsecured TCP data port and the UDP discovery port (Table 39, Table 41).
pub const c_u16PortDoIp: u16 = 13400;

/// The TLS-secured TCP data port. Not served yet; named so the number is not invented twice.
pub const c_u16PortDoIpTls: u16 = 3496;

/// Build a generic header negative acknowledge message.
pub fn BuildHeaderNack(byProtocolVersion: u8, nack: HeaderNack) -> Vec<u8> {
    WriteMessage(
        header::ReplyVersionFor(byProtocolVersion),
        PayloadType::GenericHeaderNack,
        &[nack.Code()],
    )
}

//! The DoIP entity: this simulation, on an Ethernet wire.
//!
//! Split the same way the CAN side is. `entity` decides — it holds the connection table and
//! answers messages, with no sockets and no clock of its own, so a whole diagnostic session can
//! be driven in a test. `server` owns the UDP and TCP sockets and does what it is told.

#![allow(non_snake_case, non_upper_case_globals)]

pub mod entity;
pub mod server;
pub mod settings;

pub use entity::{DoIpEntity, Reaction};
pub use server::{DoIpServer, ServerHandle};
pub use settings::DoIpSettings;

//! Thin adapter over whatsapp-rust. The only module allowed to import
//! `whatsapp_rust` directly; everything else goes through these re-exports.

pub mod jid;
pub mod message;
pub mod proxy;

pub use whatsapp_rust as sdk;
pub use whatsapp_rust::waproto::whatsapp as proto;
pub use whatsapp_rust::{Jid, Server};

//! Thin adapter over whatsapp-rust. The only module allowed to import `whatsapp_rust` directly.

/// Re-export so the rest of the crate goes through this module.
pub use whatsapp_rust as sdk;

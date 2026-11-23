//! Business logic mirroring GOWA `usecase/` (validation, JID resolution,
//! message building). HTTP handlers stay thin and call into here.

pub mod app;
pub mod chat;
pub mod device;
pub mod names;
pub mod send;
pub mod send_media;
pub mod user;

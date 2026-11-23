//! Business logic mirroring GOWA `usecase/` (validation, JID resolution,
//! message building). HTTP handlers stay thin and call into here.

pub mod app;
pub mod device;
pub mod send;

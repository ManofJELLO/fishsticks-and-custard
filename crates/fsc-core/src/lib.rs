//! Shared pieces used by the `fscd` daemon/UI and the `fsc` control CLI.

pub mod claude;
pub mod config;
pub mod ipc;
pub mod memory;
pub mod models;
pub mod phase;

pub use phase::Phase;

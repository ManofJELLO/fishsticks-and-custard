//! Shared pieces used by the `fscd` daemon/UI and the `fsc` control CLI.

pub mod agent;
pub mod config;
pub mod ipc;
pub mod lang;
pub mod memory;
pub mod models;
pub mod permission;
pub mod phase;
pub mod voices;

pub use phase::Phase;

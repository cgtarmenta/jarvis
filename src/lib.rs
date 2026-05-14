//! Jarvis — voice assistant orchestrator.
//!
//! The library half of the crate exposes the building blocks (`config`,
//! `recorder`, `stt`, `tts`, `agents`, `pipeline`, `daemon`) so integration
//! tests and future GUI front-ends can call them directly without spawning
//! the CLI binary.

pub mod agents;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod dispatcher;
pub mod handlers;
pub mod pipeline;
pub mod recorder;
pub mod session;
pub mod setup;
pub mod specs;
pub mod stt;
pub mod tts;
pub mod wake;
pub mod workers;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

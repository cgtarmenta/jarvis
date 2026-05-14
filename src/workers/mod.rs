//! Worker manifests and registry — declarative descriptions of the agents
//! Jarvis can dispatch to.
//!
//! See `specs/active/0008-orchestrator-c-worker-manifests-and-auto.md` for
//! the design rationale. Today only the manifest type and placeholder
//! substitution live here; the registry, handle trait, autodiscovery, and
//! CLI surface land in subsequent commits as we work through the C slices.

pub mod manifest;
pub mod registry;

pub use manifest::{KNOWN_PLACEHOLDERS, SessionIdCapture, SessionIdSource, WorkerManifest};
pub use registry::{DisabledWorker, WorkerRegistry, load_default};

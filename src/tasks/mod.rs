//! Async task registry — persistent records of worker invocations
//! that were spawned-and-forgotten rather than waited on
//! synchronously.
//!
//! Spec 0011 (orchestrator E1) makes this the foundation of the
//! "Jarvis, dile a gemini que analice este log y avísame" workflow.
//! Today this commit ships only the data model + persistence; E1-2
//! adds the registry, E1-3 the spawn + watcher mechanics, E1-4 the
//! CLI surface, and E1-5 the voice trigger that creates tasks.

pub mod cleanup;
pub mod format;
pub mod record;
pub mod registry;
pub mod resolve;
pub mod spawn;
pub mod triggers;

pub use cleanup::{autoprune_terminal_tasks, clean_old_tasks};
pub use format::{humanise_age, humanise_age_spanish, truncate_chars};
pub use record::{Task, TaskStatus, task_id};
pub use registry::TaskRegistry;
pub use resolve::{ResolveResult, resolve_task_reference};
pub use spawn::spawn_async_task;
pub use triggers::is_async_trigger;

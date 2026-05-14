//! Claude Code agent — thin shim over the worker registry.
//!
//! As of spec 0008 (orchestrator C), the canonical definition of "how to
//! talk to Claude" lives in `~/.config/jarvis/workers/claude.toml` (a
//! bundled starter manifest is dropped there on first run). This module
//! now exists only as a bridge between the legacy [`Agent`] trait — which
//! the pipeline still consumes — and the new [`WorkerHandle`] trait that
//! every worker (built-in or external manifest) implements.
//!
//! When hija A lands and the pipeline goes through the dispatcher /
//! registry / worker-handle path directly, this shim becomes dead code
//! and can be removed.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::warn;

use super::claude_attach::{self, Attachment};
use super::{Agent, opt_bool, opt_string};
use crate::config;
use crate::session::Turn;
use crate::workers::{
    ManifestWorker, WorkerHandle, WorkerInvocation, WorkerManifest, WorkerRegistry,
};

/// `[agent].options` keys that have been migrated into the worker
/// manifest. If a user still has them set in `config.toml` we log a
/// deprecation warning at construction time and *ignore* the values —
/// they have to edit `~/.config/jarvis/workers/claude.toml` to get the
/// effect. Per the spec 0008 refinement, this is the (a) deprecation
/// path agreed with the user.
const DEPRECATED_OPTIONS: &[&str] = &["binary", "system_prompt", "extra_args", "timeout"];

pub struct ClaudeAgent {
    /// The actual worker implementation, loaded from the registry.
    /// `Arc<dyn WorkerHandle>` so the same handle can be shared with
    /// future callers (the dispatcher in hija A) without re-loading the
    /// registry every turn.
    inner: Arc<dyn WorkerHandle>,
    /// Working directory for attachment resolution and (when set) the
    /// spawned worker's cwd. Stays in `[agent].options` because it
    /// drives Jarvis-side decisions (`claude_attach::resolve` uses it
    /// to find the right project namespace under `~/.claude/projects/`).
    cwd: Option<String>,
    /// `[agent].options.auto_resume`: when true, resolve the active
    /// Claude session UUID from the newest JSONL in this thread's
    /// project namespace at every turn. Same semantics as before C-4.
    auto_resume: bool,
}

impl ClaudeAgent {
    pub fn from_options(opts: toml::Table) -> Result<Self> {
        warn_deprecated_options(&opts);

        let cwd = opt_string(&opts, "cwd", None)?;
        let auto_resume = opt_bool(&opts, "auto_resume", false)?;

        // Make sure the workers/ directory exists and has the starter
        // claude.toml. Idempotent — does nothing if the file is already
        // there, so users who have customised the manifest keep their
        // edits across daemon restarts.
        let _ = config::ensure_workers_dir()
            .with_context(|| "ensuring ~/.config/jarvis/workers/ exists");

        // Load every manifest under workers/. Failures inside the
        // registry are surfaced as `disabled` entries, not errors —
        // the daemon should always boot.
        let registry = WorkerRegistry::load_from_dir(
            &config::workers_dir().unwrap_or_else(|_| PathBuf::from(".")),
        );

        let inner = match registry.get("claude") {
            Some(w) => w,
            None => {
                // Either the manifest is missing, malformed, or its
                // binary is absent from PATH. Match the legacy "warn
                // and proceed" behaviour by falling back to the
                // bundled starter manifest in-memory: this lets the
                // agent construct successfully and fail loudly at
                // *invoke* time if `claude` truly isn't installed.
                let disabled_reason = registry
                    .disabled_workers()
                    .iter()
                    .find(|d| d.id.as_deref() == Some("claude"))
                    .map(|d| d.reason.clone())
                    .unwrap_or_else(|| "no claude manifest in registry".to_string());
                warn!(
                    reason = %disabled_reason,
                    "claude worker not active in registry — using bundled starter as fallback. Runtime spawn may still fail if `claude` is missing."
                );
                let manifest = WorkerManifest::from_toml_str(config::STARTER_CLAUDE_MANIFEST)
                    .context("parsing bundled STARTER_CLAUDE_MANIFEST")?;
                let worker = ManifestWorker::new(manifest, PathBuf::from("<bundled>"))?;
                Arc::new(worker) as Arc<dyn WorkerHandle>
            }
        };

        Ok(Self {
            inner,
            cwd,
            auto_resume,
        })
    }

    /// Resolve the active `Attachment` once per turn. Cache state file
    /// is consulted on every call (cheap) so `jarvis agent attach`
    /// changes apply immediately without restarting the daemon.
    fn current_attachment(&self) -> Attachment {
        let state = claude_attach::load_state().ok().flatten();
        claude_attach::resolve(state.as_ref(), self.cwd.as_deref(), self.auto_resume)
    }
}

impl Agent for ClaudeAgent {
    fn name(&self) -> &'static str {
        "claude"
    }

    /// The Claude session UUID Jarvis is currently attached to (via
    /// `claude_attach::resolve`). The pipeline reads this *before*
    /// invoking `respond` so the resulting turn record carries
    /// `worker_session_id = Some(uuid)` and the session's
    /// `active_workers["claude"]` map slot gets populated.
    /// Returns `None` when there's no active attachment (the
    /// "stateless" Claude path).
    fn current_session_id(&self) -> Option<String> {
        self.current_attachment().to_uuid()
    }

    fn respond(&self, prompt: &str, history: &[Turn]) -> Result<String> {
        // Compose history into the prompt: claude --print is stateless
        // per invocation, so we embed prior turns as labelled "User:" /
        // "Assistant:" blocks and end with the current "User:" turn.
        // The model handles the conversational frame natively. This is
        // the same shape ClaudeAgent used pre-shim; the manifest takes
        // the resulting text on stdin via the {prompt}-not-in-argv
        // detection in `ManifestWorker::invoke`.
        let full_prompt = if history.is_empty() {
            prompt.to_string()
        } else {
            let mut buf = String::new();
            for turn in history {
                buf.push_str(turn.role.label());
                buf.push_str(": ");
                buf.push_str(&turn.content);
                buf.push_str("\n\n");
            }
            buf.push_str("User: ");
            buf.push_str(prompt);
            buf
        };

        // Resolve which Claude session UUID to resume (if any). The
        // shim still owns this logic because it depends on `cwd` and
        // `auto_resume`, both of which are Jarvis-side concerns
        // outside the manifest's scope.
        let attachment = self.current_attachment();
        let session_id = attachment.to_uuid();
        if let Some(uuid) = &session_id {
            tracing::info!(session = %uuid, "claude --resume");
        }

        let response = self.inner.invoke(&WorkerInvocation {
            prompt: &full_prompt,
            session_id: session_id.as_deref(),
            cwd: self.cwd.as_deref(),
        })?;
        Ok(response.text)
    }
}

fn warn_deprecated_options(opts: &toml::Table) {
    for key in DEPRECATED_OPTIONS {
        if opts.contains_key(*key) {
            warn!(
                option = %key,
                "[agent].options.{key} is deprecated and ignored. Edit \
                 ~/.config/jarvis/workers/claude.toml to customise Claude's \
                 command line, system prompt, or arguments."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `warn_deprecated_options` should not panic for any combination
    /// of deprecated keys, present or absent. The log output is
    /// observed by humans, not asserted on here — the test exists to
    /// keep the helper from crashing on edge-case inputs.
    #[test]
    fn warn_deprecated_handles_all_keys() {
        let mut opts = toml::Table::new();
        // All deprecated keys present.
        for key in DEPRECATED_OPTIONS {
            opts.insert((*key).into(), toml::Value::String("anything".into()));
        }
        warn_deprecated_options(&opts);

        // None present.
        warn_deprecated_options(&toml::Table::new());

        // One present.
        let mut single = toml::Table::new();
        single.insert("system_prompt".into(), toml::Value::String("X".into()));
        warn_deprecated_options(&single);
    }

    /// The bundled starter manifest must parse cleanly via the same
    /// validator the registry uses. If this fails the fallback path
    /// in `from_options` is broken — and shipping a malformed
    /// starter would be a release-blocking bug.
    #[test]
    fn bundled_starter_manifest_parses() {
        let m = WorkerManifest::from_toml_str(config::STARTER_CLAUDE_MANIFEST)
            .expect("bundled starter parses");
        assert_eq!(m.id, "claude");
        assert!(m.stateful, "claude is stateful (resumable session)");
        assert!(m.async_eligible, "claude is async-eligible");
        assert!(!m.tty, "claude --print uses plain pipes");
        // command should resume; initial_command should not.
        assert!(m.command.iter().any(|a| a == "--resume"));
        assert!(
            m.initial_command
                .as_ref()
                .expect("initial_command present")
                .iter()
                .all(|a| a != "--resume"),
            "initial_command must omit --resume on first turn"
        );
    }
}

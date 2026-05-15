//! Cascade integration for the LLM dispatcher — spec 0013 / B-4.
//!
//! This module bridges the [`LlmBackend`] trait (B-1/B-2/B-3) into the
//! cascade as a stage-2 [`Dispatcher`]. Three jobs:
//!
//! 1. **Backend invocation** — snapshot the registry into
//!    [`WorkerInfo`], call `backend.classify`, validate the returned
//!    id against the live registry (so a hallucinated id from the
//!    model becomes a cascade fallthrough rather than an `invoke` on
//!    a non-existent worker).
//! 2. **In-memory cache** — keyed by `(prompt, sorted(worker_ids))`,
//!    60s TTL. The cache key sorts the worker ids so registry
//!    insertion-order doesn't break cache hits; it includes the ids
//!    so a worker added or disabled mid-daemon invalidates entries
//!    that referenced the old set.
//! 3. **Failure swallowing** — backend errors and unknown-id replies
//!    log at WARN and return `Ok(None)`. The cascade then falls
//!    through to stage 3 with the default worker. This is the
//!    contract every layer above relies on: an LLM router never
//!    kills the user's turn.
//!
//! The dispatcher is constructed once per daemon process (via the
//! pipeline's `LLM_STAGE` OnceLock) so the cache survives across
//! turns. Without that lifting, the 60s TTL would be useless — the
//! cache would be dropped at the end of every `run_turn`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use tracing::warn;

use super::{LlmBackend, OpenAiCompatBackend, OpencodeCliBackend, OzCliBackend, WorkerInfo};
use crate::dispatcher::{DispatchDecision, Dispatcher};
use crate::session::Session;
use crate::workers::WorkerRegistry;

/// Cache TTL for classifier decisions. Per the spec; long enough to
/// absorb conversational repetition ("¿qué hora es?" twice in a
/// minute), short enough that worker-registry changes (a new
/// manifest dropped into `~/.config/jarvis/workers/`) take effect
/// without a daemon restart even when the user hadn't otherwise
/// triggered an eviction.
const CACHE_TTL: Duration = Duration::from_secs(60);

/// Hard cap on the cache map size — prevents pathological cases
/// (someone scripting many distinct utterances) from growing the
/// process memory without bound. When the cap is hit, the oldest
/// 25% of entries get evicted. Cheap because the map is small.
const CACHE_MAX_ENTRIES: usize = 1024;

/// Key for the cache map. The worker-id list is sorted at
/// construction so registry insertion order doesn't cause cache
/// misses (the OpenAI-compat backend's classifier prompt also
/// receives a sorted shape; consistency between the cache key and
/// what the model saw matters for the cache to actually hit).
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct CacheKey {
    prompt: String,
    workers: Vec<String>,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    /// `None` means "the backend declined, route through stage 3".
    /// Caching declines is just as useful as caching picks — the
    /// LLM call is what's expensive.
    worker_id: Option<String>,
    cached_at: Instant,
}

/// The stage-2 [`Dispatcher`] adapter. Holds the configured
/// [`LlmBackend`] and the per-process classification cache.
pub struct LlmDispatcher {
    backend: Box<dyn LlmBackend>,
    cache: Mutex<HashMap<CacheKey, CacheEntry>>,
}

impl std::fmt::Debug for LlmDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Manual impl because `dyn LlmBackend` isn't `Debug`. A
        // summary is enough — used by test `expect`/`expect_err`
        // failure messages and ad-hoc dbg!() during development.
        f.debug_struct("LlmDispatcher")
            .field("backend", &self.backend.name())
            .field(
                "cache_entries",
                &self.cache.lock().map(|m| m.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl LlmDispatcher {
    pub fn new(backend: Box<dyn LlmBackend>) -> Self {
        Self {
            backend,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Diagnostic accessor — used by `jarvis dispatcher status` (when
    /// we add that surface) and by the pipeline's startup log line
    /// to record which backend ended up wired in.
    pub fn backend_name(&self) -> &str {
        self.backend.name()
    }

    fn build_key(prompt: &str, workers: &[WorkerInfo]) -> CacheKey {
        let mut ids: Vec<String> = workers.iter().map(|w| w.id.clone()).collect();
        ids.sort();
        CacheKey {
            prompt: prompt.to_string(),
            workers: ids,
        }
    }

    /// Look up the cache. Returns the cached worker_id (which itself
    /// may be `None` for cached declines) if the entry is fresh, or
    /// `None` if the cache missed or the entry is stale.
    fn cache_get(&self, key: &CacheKey) -> Option<Option<String>> {
        let map = self.cache.lock().ok()?;
        let entry = map.get(key)?;
        if entry.cached_at.elapsed() < CACHE_TTL {
            Some(entry.worker_id.clone())
        } else {
            None
        }
    }

    fn cache_put(&self, key: CacheKey, worker_id: Option<String>) {
        let Ok(mut map) = self.cache.lock() else {
            return;
        };
        if map.len() >= CACHE_MAX_ENTRIES {
            // Evict the oldest 25% by `cached_at`. Cheap because
            // CACHE_MAX_ENTRIES is small and this runs rarely.
            let mut by_age: Vec<(CacheKey, Instant)> =
                map.iter().map(|(k, v)| (k.clone(), v.cached_at)).collect();
            by_age.sort_by_key(|(_, t)| *t);
            let drop_count = CACHE_MAX_ENTRIES / 4;
            for (k, _) in by_age.into_iter().take(drop_count) {
                map.remove(&k);
            }
        }
        map.insert(
            key,
            CacheEntry {
                worker_id,
                cached_at: Instant::now(),
            },
        );
    }

    /// Build a `DispatchDecision` from a validated worker id. The
    /// session-id lookup mirrors `BuiltinIntentDispatcher`'s pattern
    /// so stateful workers (like `claude`) get their prior session
    /// resumed when the LLM picks them.
    fn decision_for(worker_id: &str, prompt: &str, session: &Session) -> DispatchDecision {
        let session_id = session
            .active_worker_session(worker_id)
            .and_then(|opt| opt.clone());
        DispatchDecision {
            worker_id: worker_id.to_string(),
            resolved_prompt: prompt.to_string(),
            session_id,
        }
    }
}

impl Dispatcher for LlmDispatcher {
    fn dispatch(
        &self,
        prompt: &str,
        session: &Session,
        registry: &WorkerRegistry,
    ) -> Result<Option<DispatchDecision>> {
        let workers = WorkerInfo::from_registry(registry);
        let key = Self::build_key(prompt, &workers);

        // Cache check first — cheap, in-memory, no syscalls.
        if let Some(cached) = self.cache_get(&key) {
            tracing::debug!(
                backend = self.backend.name(),
                cached_worker = ?cached,
                "llm dispatcher cache hit"
            );
            return Ok(cached
                .as_deref()
                .filter(|id| registry.get(id).is_some())
                .map(|id| Self::decision_for(id, prompt, session)));
        }

        // Cache miss: ask the backend. Errors *never* propagate —
        // the cascade adapter's whole point is to be invisible
        // when it doesn't have a confident answer.
        let raw = match self.backend.classify(prompt, &workers) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    error = %e,
                    backend = self.backend.name(),
                    "llm classifier failed; falling through to stage 3"
                );
                // Don't cache backend errors — a transient network
                // hiccup shouldn't lock us out for 60s.
                return Ok(None);
            }
        };

        // Validate the returned id (if any) against the live
        // registry. Hallucinated ids become declines.
        let validated = raw
            .as_ref()
            .filter(|id| registry.get(id).is_some())
            .cloned();
        if raw.is_some() && validated.is_none() {
            warn!(
                backend = self.backend.name(),
                returned = ?raw,
                "llm classifier returned unknown worker id; falling through"
            );
        }

        // Cache the validated outcome (including `None` declines).
        self.cache_put(key, validated.clone());

        Ok(validated.map(|id| Self::decision_for(&id, prompt, session)))
    }
}

/// Translate a raw `[dispatcher.fallback]` TOML value into a fully-
/// built [`LlmDispatcher`].
///
/// Soft-fails: an unrecognised `backend` field, a missing required
/// field, or a type mismatch returns `Err`. The caller (pipeline)
/// logs the error at WARN and proceeds without stage 2, matching
/// the spec's "malformed config disables stage 2, daemon still
/// starts" requirement.
pub fn build_llm_stage(raw: &toml::Value) -> Result<LlmDispatcher> {
    let table = raw
        .as_table()
        .ok_or_else(|| anyhow::anyhow!("[dispatcher.fallback] must be a table, got {raw}"))?;

    let backend = table
        .get("backend")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("[dispatcher.fallback].backend is required (string)"))?;

    let llm: Box<dyn LlmBackend> = match backend {
        "openai_compat" => Box::new(build_openai_compat(table)?),
        "oz" => Box::new(build_oz_cli(table)?),
        "opencode" => Box::new(build_opencode_cli(table)?),
        other => {
            return Err(anyhow::anyhow!(
                "[dispatcher.fallback].backend = {other:?} is unknown; \
                 expected \"openai_compat\", \"oz\", or \"opencode\""
            ));
        }
    };

    Ok(LlmDispatcher::new(llm))
}

fn read_string(table: &toml::Table, field: &str) -> Result<String> {
    table
        .get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("[dispatcher.fallback].{field} is required (string)"))
}

fn read_optional_string(table: &toml::Table, field: &str) -> Result<Option<String>> {
    match table.get(field) {
        None => Ok(None),
        Some(v) => v
            .as_str()
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| anyhow::anyhow!("[dispatcher.fallback].{field} must be a string")),
    }
}

fn read_optional_u64(table: &toml::Table, field: &str) -> Result<Option<u64>> {
    match table.get(field) {
        None => Ok(None),
        Some(v) => v
            .as_integer()
            .filter(|i| *i >= 0)
            .map(|i| Some(i as u64))
            .ok_or_else(|| {
                anyhow::anyhow!("[dispatcher.fallback].{field} must be a non-negative integer")
            }),
    }
}

fn read_string_map(table: &toml::Table, field: &str) -> Result<HashMap<String, String>> {
    let Some(v) = table.get(field) else {
        return Ok(HashMap::new());
    };
    let inner = v.as_table().ok_or_else(|| {
        anyhow::anyhow!("[dispatcher.fallback].{field} must be a table of string→string")
    })?;
    let mut out = HashMap::with_capacity(inner.len());
    for (k, val) in inner {
        let s = val.as_str().ok_or_else(|| {
            anyhow::anyhow!("[dispatcher.fallback].{field}.{k} must be a string, got {val}")
        })?;
        out.insert(k.clone(), s.to_string());
    }
    Ok(out)
}

fn build_openai_compat(table: &toml::Table) -> Result<OpenAiCompatBackend> {
    let endpoint = read_string(table, "endpoint")?;
    let model = read_string(table, "model")?;
    let api_key = read_optional_string(table, "api_key")?;
    let headers = read_string_map(table, "headers")?;
    let timeout = read_optional_u64(table, "timeout_secs")?.map(Duration::from_secs);

    let mut backend = OpenAiCompatBackend::new(endpoint, model);
    if let Some(k) = api_key {
        backend = backend.with_api_key(k);
    }
    if !headers.is_empty() {
        backend = backend.with_headers(headers);
    }
    if let Some(t) = timeout {
        backend = backend.with_timeout(t);
    }
    Ok(backend)
}

fn build_oz_cli(table: &toml::Table) -> Result<OzCliBackend> {
    let model = read_string(table, "model")?;
    let binary = read_optional_string(table, "binary")?;
    let timeout = read_optional_u64(table, "timeout_secs")?.map(Duration::from_secs);

    let mut backend = OzCliBackend::new(model);
    if let Some(b) = binary {
        backend = backend.with_binary(b);
    }
    if let Some(t) = timeout {
        backend = backend.with_timeout(t);
    }
    Ok(backend)
}

/// Mirror of `build_oz_cli` for the opencode backend (spec 0016).
/// Same surface: required `model` (provider/model), optional
/// `binary` override, optional `timeout_secs`. The fast-default
/// path is wizard-driven (`opencode/qwen3.6-plus-free`); here we
/// just plumb whatever the user wrote in TOML.
fn build_opencode_cli(table: &toml::Table) -> Result<OpencodeCliBackend> {
    let model = read_string(table, "model")?;
    let binary = read_optional_string(table, "binary")?;
    let timeout = read_optional_u64(table, "timeout_secs")?.map(Duration::from_secs);

    let mut backend = OpencodeCliBackend::new(model);
    if let Some(b) = binary {
        backend = backend.with_binary(b);
    }
    if let Some(t) = timeout {
        backend = backend.with_timeout(t);
    }
    Ok(backend)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::dispatcher::llm::testing::MockLlmBackend;
    use crate::workers::{WorkerHandle, WorkerInvocation, WorkerResponse};

    struct FakeWorker {
        id: &'static str,
        stateful: bool,
    }

    impl WorkerHandle for FakeWorker {
        fn id(&self) -> &str {
            self.id
        }
        fn stateful(&self) -> bool {
            self.stateful
        }
        fn invoke(&self, _: &WorkerInvocation<'_>) -> Result<WorkerResponse> {
            unreachable!("trait test should not invoke")
        }
    }

    /// A backend that counts invocations so we can verify the cache
    /// actually elided the call. Returns the configured reply
    /// regardless of input — purpose-built for cache tests.
    struct CountingBackend {
        reply: Option<String>,
        calls: Arc<AtomicUsize>,
    }

    impl CountingBackend {
        fn new(reply: Option<&str>) -> (Self, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    reply: reply.map(|s| s.to_string()),
                    calls: Arc::clone(&calls),
                },
                calls,
            )
        }
    }

    impl LlmBackend for CountingBackend {
        fn name(&self) -> &str {
            "counting"
        }
        fn classify(&self, _: &str, _: &[WorkerInfo]) -> Result<Option<String>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.reply.clone())
        }
    }

    fn registry_with(ids: &[(&'static str, bool)]) -> WorkerRegistry {
        let mut r = WorkerRegistry::default();
        for (id, stateful) in ids {
            r.register_builtin(Arc::new(FakeWorker {
                id,
                stateful: *stateful,
            }));
        }
        r
    }

    /// Happy path: backend picks a worker that exists in the
    /// registry → adapter returns a DispatchDecision with the id +
    /// prompt verbatim.
    #[test]
    fn dispatch_returns_decision_when_backend_picks_known_worker() {
        let backend = Box::new(MockLlmBackend::picking("time"));
        let dispatcher = LlmDispatcher::new(backend);
        let registry = registry_with(&[("time", false), ("claude", true)]);
        let session = Session::new();

        let out = dispatcher
            .dispatch("qué hora es", &session, &registry)
            .unwrap()
            .expect("should pick a worker");
        assert_eq!(out.worker_id, "time");
        assert_eq!(out.resolved_prompt, "qué hora es");
        assert_eq!(out.session_id, None);
    }

    /// Stateful worker: the adapter pulls the session id from the
    /// session's `active_workers` map (same shape as
    /// BuiltinIntentDispatcher). Lets the LLM hand off to a stateful
    /// `claude` and have its prior session resumed.
    #[test]
    fn dispatch_carries_prior_session_id_for_stateful_worker() {
        let backend = Box::new(MockLlmBackend::picking("claude"));
        let dispatcher = LlmDispatcher::new(backend);
        let registry = registry_with(&[("time", false), ("claude", true)]);
        let mut session = Session::new();
        session.set_active_worker_session("claude", Some("sess-abc".into()));

        let out = dispatcher
            .dispatch("explícame X", &session, &registry)
            .unwrap()
            .expect("should pick claude");
        assert_eq!(out.worker_id, "claude");
        assert_eq!(out.session_id.as_deref(), Some("sess-abc"));
    }

    /// Backend declines (`Ok(None)`) → cascade falls through.
    #[test]
    fn dispatch_returns_none_when_backend_declines() {
        let backend = Box::new(MockLlmBackend::declining());
        let dispatcher = LlmDispatcher::new(backend);
        let registry = registry_with(&[("time", false)]);
        let session = Session::new();

        let out = dispatcher
            .dispatch("anything", &session, &registry)
            .unwrap();
        assert!(out.is_none());
    }

    /// Backend hallucinated an id that isn't in the registry → the
    /// adapter treats it as a decline. The cascade falls through to
    /// stage 3 instead of invoking a non-existent worker.
    #[test]
    fn dispatch_filters_unknown_worker_ids() {
        let backend = Box::new(MockLlmBackend::picking("ghost-worker"));
        let dispatcher = LlmDispatcher::new(backend);
        let registry = registry_with(&[("time", false), ("claude", true)]);
        let session = Session::new();

        let out = dispatcher
            .dispatch("anything", &session, &registry)
            .unwrap();
        assert!(out.is_none(), "unknown id must not produce a decision");
    }

    /// Backend errors (`Err`) → swallowed at WARN, `Ok(None)` to
    /// the cascade. Critical invariant: a transient classifier
    /// failure must never kill the user's turn.
    #[test]
    fn dispatch_swallows_backend_errors() {
        let backend = Box::new(MockLlmBackend::failing("classifier offline"));
        let dispatcher = LlmDispatcher::new(backend);
        let registry = registry_with(&[("time", false)]);
        let session = Session::new();

        let out = dispatcher
            .dispatch("anything", &session, &registry)
            .unwrap();
        assert!(out.is_none());
    }

    /// Cache hit: the same prompt + worker set produces only one
    /// backend call.
    #[test]
    fn cache_elides_repeated_call_for_same_prompt_and_workers() {
        let (backend, calls) = CountingBackend::new(Some("time"));
        let dispatcher = LlmDispatcher::new(Box::new(backend));
        let registry = registry_with(&[("time", false), ("claude", true)]);
        let session = Session::new();

        let _ = dispatcher
            .dispatch("qué hora es", &session, &registry)
            .unwrap();
        let _ = dispatcher
            .dispatch("qué hora es", &session, &registry)
            .unwrap();
        let _ = dispatcher
            .dispatch("qué hora es", &session, &registry)
            .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "second + third calls should be cache hits"
        );
    }

    /// Cache hits also work for cached declines — a `none` reply
    /// shouldn't re-call the backend for the same key within TTL.
    #[test]
    fn cache_caches_declines_too() {
        let (backend, calls) = CountingBackend::new(None);
        let dispatcher = LlmDispatcher::new(Box::new(backend));
        let registry = registry_with(&[("time", false)]);
        let session = Session::new();

        let _ = dispatcher
            .dispatch("explica X", &session, &registry)
            .unwrap();
        let _ = dispatcher
            .dispatch("explica X", &session, &registry)
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// Different prompts produce distinct cache entries; both
    /// trigger backend calls.
    #[test]
    fn cache_keys_distinguish_prompts() {
        let (backend, calls) = CountingBackend::new(Some("time"));
        let dispatcher = LlmDispatcher::new(Box::new(backend));
        let registry = registry_with(&[("time", false)]);
        let session = Session::new();

        let _ = dispatcher
            .dispatch("prompt A", &session, &registry)
            .unwrap();
        let _ = dispatcher
            .dispatch("prompt B", &session, &registry)
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// The worker-id set is part of the cache key: adding a worker
    /// to the registry between calls produces a cache miss because
    /// the model would now see a different option list.
    #[test]
    fn cache_keys_distinguish_worker_sets() {
        let (backend, calls) = CountingBackend::new(Some("time"));
        let dispatcher = LlmDispatcher::new(Box::new(backend));
        let session = Session::new();

        let reg_a = registry_with(&[("time", false)]);
        let reg_b = registry_with(&[("time", false), ("claude", true)]);

        let _ = dispatcher
            .dispatch("qué hora es", &session, &reg_a)
            .unwrap();
        let _ = dispatcher
            .dispatch("qué hora es", &session, &reg_b)
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// Worker id insertion order doesn't affect the cache key —
    /// sorting normalises it. Catches the regression where a
    /// registry rebuild changes order and we'd otherwise re-query
    /// the backend for an identical question.
    #[test]
    fn cache_keys_normalise_worker_order() {
        let (backend, calls) = CountingBackend::new(Some("time"));
        let dispatcher = LlmDispatcher::new(Box::new(backend));
        let session = Session::new();

        let reg_ab = registry_with(&[("time", false), ("claude", true)]);
        let reg_ba = registry_with(&[("claude", true), ("time", false)]);

        let _ = dispatcher
            .dispatch("qué hora es", &session, &reg_ab)
            .unwrap();
        let _ = dispatcher
            .dispatch("qué hora es", &session, &reg_ba)
            .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "different insertion order, same key set → one call"
        );
    }

    /// Backend errors are *not* cached — a transient hiccup
    /// shouldn't lock us out for 60s. Two consecutive errors
    /// should produce two backend calls.
    #[test]
    fn cache_does_not_persist_backend_errors() {
        struct FlakyBackend {
            calls: Arc<AtomicUsize>,
        }
        impl LlmBackend for FlakyBackend {
            fn name(&self) -> &str {
                "flaky"
            }
            fn classify(&self, _: &str, _: &[WorkerInfo]) -> Result<Option<String>> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("transient"))
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let dispatcher = LlmDispatcher::new(Box::new(FlakyBackend {
            calls: Arc::clone(&calls),
        }));
        let registry = registry_with(&[("time", false)]);
        let session = Session::new();

        let _ = dispatcher.dispatch("hola", &session, &registry).unwrap();
        let _ = dispatcher.dispatch("hola", &session, &registry).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// `backend_name` exposes the wrapped backend's identifier for
    /// tracing log fields and future diagnostic surfaces.
    #[test]
    fn backend_name_passes_through() {
        let dispatcher = LlmDispatcher::new(Box::new(MockLlmBackend::declining()));
        assert_eq!(dispatcher.backend_name(), "mock");
    }

    /// `build_llm_stage` accepts a well-formed openai_compat config
    /// and produces a working dispatcher with the right backend name.
    #[test]
    fn build_llm_stage_openai_compat_minimal() {
        let raw = toml::toml! {
            backend = "openai_compat"
            endpoint = "http://localhost:11434/v1/chat/completions"
            model = "llama-3.1-8b"
        };
        let dispatcher = build_llm_stage(&toml::Value::Table(raw)).expect("should build");
        assert_eq!(dispatcher.backend_name(), "openai_compat");
    }

    /// `build_llm_stage` accepts an openai_compat config with the
    /// full optional surface: api_key, headers, timeout_secs.
    #[test]
    fn build_llm_stage_openai_compat_full() {
        let raw = toml::toml! {
            backend = "openai_compat"
            endpoint = "https://api.groq.com/openai/v1/chat/completions"
            model = "llama-3.1-70b-versatile"
            api_key = "gsk_xxx"
            timeout_secs = 8
            [headers]
            "X-Custom-Header" = "yes"
        };
        let dispatcher = build_llm_stage(&toml::Value::Table(raw)).expect("should build");
        assert_eq!(dispatcher.backend_name(), "openai_compat");
    }

    /// `build_llm_stage` accepts a minimal oz config.
    #[test]
    fn build_llm_stage_oz_minimal() {
        let raw = toml::toml! {
            backend = "oz"
            model = "claude-3.7-sonnet"
        };
        let dispatcher = build_llm_stage(&toml::Value::Table(raw)).expect("should build");
        assert_eq!(dispatcher.backend_name(), "oz");
    }

    /// `build_llm_stage` accepts an oz config with binary + timeout
    /// overrides.
    #[test]
    fn build_llm_stage_oz_full() {
        let raw = toml::toml! {
            backend = "oz"
            model = "claude-3.7-sonnet"
            binary = "/opt/oz/bin/oz"
            timeout_secs = 10
        };
        let dispatcher = build_llm_stage(&toml::Value::Table(raw)).expect("should build");
        assert_eq!(dispatcher.backend_name(), "oz");
    }

    /// `build_llm_stage` accepts a minimal opencode config
    /// (spec 0016). Required field: `model` in the
    /// `provider/model` shape opencode itself accepts.
    #[test]
    fn build_llm_stage_opencode_minimal() {
        let raw = toml::toml! {
            backend = "opencode"
            model = "opencode/qwen3.6-plus-free"
        };
        let dispatcher = build_llm_stage(&toml::Value::Table(raw)).expect("should build");
        assert_eq!(dispatcher.backend_name(), "opencode");
    }

    /// `build_llm_stage` accepts an opencode config with binary +
    /// timeout overrides — same option surface as oz.
    #[test]
    fn build_llm_stage_opencode_full() {
        let raw = toml::toml! {
            backend = "opencode"
            model = "opencode/big-pickle"
            binary = "/usr/local/bin/opencode"
            timeout_secs = 20
        };
        let dispatcher = build_llm_stage(&toml::Value::Table(raw)).expect("should build");
        assert_eq!(dispatcher.backend_name(), "opencode");
    }

    /// `build_llm_stage` errors on a missing `backend` field with a
    /// message naming the section.
    #[test]
    fn build_llm_stage_errors_on_missing_backend() {
        let raw = toml::toml! {
            model = "x"
        };
        let err = build_llm_stage(&toml::Value::Table(raw)).expect_err("should fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("backend"), "got: {msg}");
        assert!(msg.contains("[dispatcher.fallback]"), "got: {msg}");
    }

    /// `build_llm_stage` errors on an unknown `backend` value with
    /// a useful "expected X or Y" message.
    #[test]
    fn build_llm_stage_errors_on_unknown_backend() {
        let raw = toml::toml! {
            backend = "magic-router"
            model = "x"
        };
        let err = build_llm_stage(&toml::Value::Table(raw)).expect_err("should fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("magic-router"), "got: {msg}");
        assert!(
            msg.contains("openai_compat") && msg.contains("oz") && msg.contains("opencode"),
            "got: {msg}"
        );
    }

    /// Openai_compat config without the required `endpoint` field
    /// fails with a specific error naming the field.
    #[test]
    fn build_llm_stage_errors_on_missing_endpoint() {
        let raw = toml::toml! {
            backend = "openai_compat"
            model = "x"
        };
        let err = build_llm_stage(&toml::Value::Table(raw)).expect_err("should fail");
        assert!(format!("{err:#}").contains("endpoint"));
    }

    /// Oz config without the required `model` field fails with a
    /// specific error naming the field.
    #[test]
    fn build_llm_stage_errors_on_missing_model() {
        let raw = toml::toml! {
            backend = "oz"
        };
        let err = build_llm_stage(&toml::Value::Table(raw)).expect_err("should fail");
        assert!(format!("{err:#}").contains("model"));
    }

    /// Non-table top-level value (string, list, etc) is rejected
    /// with a clear message rather than panicking.
    #[test]
    fn build_llm_stage_errors_on_non_table_root() {
        let raw = toml::Value::String("oops".into());
        let err = build_llm_stage(&raw).expect_err("should fail");
        assert!(format!("{err:#}").contains("table"));
    }
}

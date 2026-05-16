//! Built-in handler for voice-driven application launching — spec
//! 0015.
//!
//! Matches Spanish + English launch triggers (`abre Firefox`,
//! `launch VS Code`, …), resolves the tail through a built-in
//! alias table + an optional user-supplied alias map, gates the
//! resolved binary against a hard-coded refusal list, and spawns
//! it through the platform-appropriate launcher.
//!
//! Linux uses `xdg-open` for registered `.desktop` entries and
//! falls back to direct `Command::new` for raw binaries on PATH.
//! macOS uses `open -a <AppName>`. Other unix targets (BSD,
//! Wayland-only setups) inherit the Linux path until someone
//! contributes a specialised backend.
//!
//! The whole point is that "abre Firefox" should be a
//! sub-millisecond Rust path that confirms in Spanish ("Listo,
//! abrí Firefox.") rather than cold-spawning Claude to interpret
//! the request — same shape as `time`, `calc`, `task-list`, etc.
//!
//! Failure modes are friendly: unknown apps say "No encuentro X
//! instalado"; refused names say "No puedo lanzar X por
//! seguridad" — not a stacktrace, not a generic dispatcher
//! fallthrough.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Result;

use crate::dispatcher::IntentMatcher;
use crate::session::Session;
use crate::workers::{WorkerHandle, WorkerInvocation, WorkerResponse};

/// Trigger phrases — matched as case-insensitive prefix against
/// the normalised transcript. Each trigger must end in a space so
/// "abrelas" or "openhouse" don't accidentally match.
const LAUNCH_TRIGGERS: &[&str] = &[
    "abre ", "abrir ", "lanza ", "lanzar ", "inicia ", "iniciar ", "open ", "launch ", "start ",
];

/// Friendly-name → binary alias table. Lowercase keys; matched
/// against the normalised app tail. User aliases in
/// `[apps.aliases]` override these.
///
/// Curated for the ~20 most-common desktop apps a voice user
/// might reach for. Adding entries is a one-line change; the
/// user-extensible config is the right place for niche apps
/// (signal-desktop, obsidian-flatpak, etc.).
const BUILTIN_ALIASES: &[(&str, &str)] = &[
    // Browsers
    ("firefox", "firefox"),
    ("chrome", "google-chrome-stable"),
    ("chromium", "chromium"),
    ("brave", "brave"),
    // Editors / IDEs
    ("code", "code"),
    ("vscode", "code"),
    ("vs code", "code"),
    ("visual studio code", "code"),
    ("vim", "gvim"),
    ("neovim", "nvim"),
    ("zed", "zed"),
    // Comms
    ("slack", "slack"),
    ("discord", "discord"),
    ("telegram", "telegram-desktop"),
    ("signal", "signal-desktop"),
    ("zoom", "zoom"),
    // Media
    ("spotify", "spotify"),
    ("vlc", "vlc"),
    // Terminals
    ("terminal", "kitty"),
    ("kitty", "kitty"),
    ("alacritty", "alacritty"),
    ("warp", "warp-terminal"),
    // Files / utilities
    ("files", "nautilus"),
    ("nautilus", "nautilus"),
    ("calculator", "gnome-calculator"),
    ("calculadora", "gnome-calculator"),
    // Office
    ("obsidian", "obsidian"),
    ("notion", "notion-app"),
];

/// Hard-coded refusal list — names that must never be voice-
/// launched even if the user (accidentally or otherwise) puts
/// them in their alias map. Policy resolved in spec 0015 and
/// documented there.
///
/// Match is case-insensitive on the *resolved* binary name (after
/// the alias lookup) so a user alias of `"safe-name" = "rm"`
/// still gets refused.
const REFUSAL_LIST: &[&str] = &[
    // Destructive filesystem ops
    "rm",
    "dd",
    "mkfs",
    "fdisk",
    "parted",
    "wipefs",
    "shred",
    // Power state
    "shutdown",
    "reboot",
    "poweroff",
    "halt",
    "suspend",
    "hibernate",
    // Privilege escalation
    "sudo",
    "su",
    "doas",
    "pkexec",
    // Process control
    "kill",
    "killall",
    "pkill",
    // System service control
    "systemctl",
    "service",
    "init",
];

pub struct AppLauncherHandler {
    /// User aliases from `[apps.aliases]`, override built-ins.
    /// Constructed empty when no user config is wired in.
    user_aliases: HashMap<String, String>,
    /// Launcher backend chosen at construction by `cfg!`. Stored
    /// as an enum (not a trait object) so the handler stays
    /// `Clone`-able and the test suite can swap in a fake backend
    /// without `Box<dyn>` gymnastics.
    backend: Backend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    /// Linux: try `xdg-open <name>` first (so `.desktop`-registered
    /// apps work by their friendly name), fall back to direct
    /// `Command::new(<binary>).spawn()` for raw binaries on PATH.
    LinuxXdg,
    /// macOS: `open -a <AppName>`. Handles both bundle names and
    /// raw binaries through a single entry point.
    MacOpen,
    /// Test backend: records calls instead of spawning. Reachable
    /// only via `AppLauncherHandler::with_backend` in `cfg(test)`.
    #[cfg(test)]
    Test,
}

impl AppLauncherHandler {
    /// Construct with the platform's default launcher and no
    /// user-supplied aliases. Production startup uses
    /// [`AppLauncherHandler::with_user_aliases`] to plug in
    /// `cfg.apps.aliases`.
    pub fn new() -> Self {
        Self {
            user_aliases: HashMap::new(),
            backend: default_backend(),
        }
    }

    /// Construct with a user alias map. Keys are normalised
    /// (lowercased, accent-folded) so `"VS Code"` and `"vs code"`
    /// collide cleanly.
    pub fn with_user_aliases(aliases: HashMap<String, String>) -> Self {
        Self {
            user_aliases: aliases
                .into_iter()
                .map(|(k, v)| (normalise(&k), v))
                .collect(),
            backend: default_backend(),
        }
    }

    /// Resolve an app name (post-trigger tail) into the binary we
    /// should hand to the launcher. User aliases beat built-ins;
    /// no match → fall back to the raw input (caller will then
    /// pass it to `xdg-open`, which handles unregistered names
    /// gracefully). Returns the lowercased / accent-folded form.
    fn resolve(&self, app: &str) -> String {
        let n = normalise(app);
        if let Some(v) = self.user_aliases.get(&n) {
            return v.clone();
        }
        for (key, bin) in BUILTIN_ALIASES {
            if n == *key {
                return bin.to_string();
            }
        }
        n
    }
}

impl Default for AppLauncherHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl IntentMatcher for AppLauncherHandler {
    fn worker_id(&self) -> &str {
        "app-launcher"
    }

    fn recognize(&self, prompt: &str, _session: &Session) -> Option<String> {
        // Cut at first clause boundary so "abre Firefox, no está sonando"
        // resolves to "firefox" instead of dragging the whole follow-up
        // into the launch argument.
        let head = clip_to_clause(prompt);
        let n = normalise(head);
        for t in LAUNCH_TRIGGERS {
            if let Some(tail) = n.strip_prefix(t) {
                let tail = tail.trim();
                if !tail.is_empty() && looks_like_app_name(tail) {
                    return Some(tail.to_string());
                }
            }
        }
        None
    }
}

impl WorkerHandle for AppLauncherHandler {
    fn id(&self) -> &str {
        "app-launcher"
    }

    fn description(&self) -> Option<&str> {
        Some("Launches a desktop application by name (Firefox, VS Code, Spotify, …).")
    }

    fn dispatch_hint(&self) -> Option<&str> {
        Some(
            "Use when the user wants to open or launch a desktop \
             application (e.g. \"abre Firefox\", \"launch Spotify\").",
        )
    }

    fn invoke(&self, ctx: &WorkerInvocation<'_>) -> Result<WorkerResponse> {
        // Stage-1 hands us the resolved tail (already trigger-stripped).
        // Stage-2 (LLM dispatcher) hands us the raw user turn — opencode
        // sometimes misroutes complaints like "Firefox no está sonando"
        // here. Trim at the first clause boundary, drop the trigger
        // prefix if present, and bail with a friendly message when the
        // residue doesn't look like a bare app name.
        let raw = ctx.prompt.trim();
        let head = clip_to_clause(raw);
        let stripped = strip_trigger(head);
        let had_trigger = stripped.len() < head.len();
        let tail = stripped
            .trim()
            .trim_end_matches(['.', '?', '!', '¿', '¡', ',']);
        // If we never matched a trigger AND clip_to_clause discarded
        // real words after the boundary, the prompt was a multi-clause
        // sentence misrouted to us — refuse rather than try to launch
        // the first noun we see (e.g. "Jarvis, Firefox no está sonando").
        // A bare trailing period on "Firefox." doesn't count.
        let discarded_has_words = raw[head.len()..].chars().any(|c| c.is_alphanumeric());
        if !had_trigger && discarded_has_words {
            return Ok(WorkerResponse {
                text: "No entendí qué app abrir.".to_string(),
                captured_session_id: None,
            });
        }
        if !looks_like_app_name(tail) {
            return Ok(WorkerResponse {
                text: "No entendí qué app abrir.".to_string(),
                captured_session_id: None,
            });
        }
        let resolved = self.resolve(tail);

        if is_refused(&resolved) {
            return Ok(WorkerResponse {
                text: format!("No puedo lanzar {tail} por seguridad."),
                captured_session_id: None,
            });
        }

        match launch(&self.backend, &resolved) {
            Ok(()) => Ok(WorkerResponse {
                text: format!("Listo, abrí {tail}."),
                captured_session_id: None,
            }),
            Err(LaunchError::NotFound) => Ok(WorkerResponse {
                text: format!("No encuentro {tail} instalado."),
                captured_session_id: None,
            }),
            Err(LaunchError::Other(msg)) => Ok(WorkerResponse {
                text: format!("No pude lanzar {tail}: {msg}"),
                captured_session_id: None,
            }),
        }
    }
}

/// Refusal check — case-insensitive exact match on the resolved
/// binary name. Tested standalone so the policy decisions stay
/// honest as we add or remove entries.
pub(crate) fn is_refused(resolved: &str) -> bool {
    let lower = resolved.to_ascii_lowercase();
    // Match the *first whitespace-delimited token* of the resolved
    // binary so a user alias like `"x" = "rm -rf"` is still
    // caught (the binary is `rm`).
    let head = lower.split_whitespace().next().unwrap_or("");
    REFUSAL_LIST.iter().any(|r| *r == head)
}

#[derive(Debug)]
enum LaunchError {
    NotFound,
    Other(String),
}

/// Cap on how long we wait after spawn before deciding the child
/// survived. Binary-not-found and immediate display-socket failures
/// exit in <10ms; 150ms is comfortably above the noise and below
/// human-perceptible TTS lag.
const SPAWN_VERIFY_DELAY: Duration = Duration::from_millis(150);

/// Cut `prompt` at the first sentence/clause-boundary character so
/// follow-on clauses don't get dragged into the launch argument.
fn clip_to_clause(prompt: &str) -> &str {
    let mut end = prompt.len();
    for (i, c) in prompt.char_indices() {
        if matches!(c, '.' | ',' | '!' | '?' | ';' | '\n' | ':') {
            end = i;
            break;
        }
    }
    &prompt[..end]
}

/// If `s` starts with a known launch trigger (e.g. "abre "), return
/// the tail; otherwise return `s` unchanged. Used by `invoke` to
/// normalise both stage-1 ("firefox") and stage-2 ("abre firefox")
/// prompt shapes into a bare app name.
fn strip_trigger(s: &str) -> &str {
    let n_lower = s.trim_start().to_ascii_lowercase();
    for t in LAUNCH_TRIGGERS {
        if let Some(stripped) = n_lower.strip_prefix(t) {
            let strip_len = s.len() - stripped.len();
            return &s[strip_len..];
        }
    }
    s
}

/// A real app name is short and unpunctuated. Anything longer than
/// 4 words or 60 chars is almost certainly a misrouted complaint
/// from stage-2, not "open Firefox".
fn looks_like_app_name(s: &str) -> bool {
    if s.is_empty() || s.len() > 60 {
        return false;
    }
    let words = s.split_whitespace().count();
    (1..=4).contains(&words)
}

/// Spawn the resolved binary through the chosen backend. The
/// child is dropped immediately (fire-and-forget) — the launched
/// app inherits the daemon's environment, which is usually what
/// the user wants (their `DISPLAY` / `WAYLAND_DISPLAY` / `XDG_*`).
fn launch(backend: &Backend, resolved: &str) -> Result<(), LaunchError> {
    match backend {
        Backend::LinuxXdg => spawn_linux(resolved),
        Backend::MacOpen => spawn_macos(resolved),
        #[cfg(test)]
        Backend::Test => {
            // Test backend: record the call and succeed unless the
            // resolved name is "__missing__" (which simulates a
            // not-found binary so we can test that failure path).
            if resolved == "__missing__" {
                Err(LaunchError::NotFound)
            } else {
                Ok(())
            }
        }
    }
}

fn spawn_linux(resolved: &str) -> Result<(), LaunchError> {
    // Try xdg-open first — handles `.desktop`-registered apps by
    // their friendly name. If it spawns but exits non-zero within
    // SPAWN_VERIFY_DELAY (e.g. exit 3 = "no handler for argument"),
    // fall back to direct exec so we can give a real error rather
    // than a confident "Listo, abrí <garbage>".
    if let Ok(mut child) = Command::new("xdg-open")
        .arg(resolved)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        std::thread::sleep(SPAWN_VERIFY_DELAY);
        match child.try_wait() {
            Ok(Some(status)) if !status.success() => {
                // xdg-open declined — fall through to direct exec.
            }
            _ => return Ok(()),
        }
    }

    match Command::new(resolved)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::sleep(SPAWN_VERIFY_DELAY);
            match child.try_wait() {
                Ok(Some(status)) if !status.success() => Err(LaunchError::Other(format!(
                    "salió con código {}",
                    status.code().unwrap_or(-1)
                ))),
                _ => Ok(()),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(LaunchError::NotFound),
        Err(e) => Err(LaunchError::Other(e.to_string())),
    }
}

fn spawn_macos(resolved: &str) -> Result<(), LaunchError> {
    match Command::new("open")
        .args(["-a", resolved])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::sleep(SPAWN_VERIFY_DELAY);
            match child.try_wait() {
                Ok(Some(status)) if !status.success() => Err(LaunchError::Other(format!(
                    "salió con código {}",
                    status.code().unwrap_or(-1)
                ))),
                _ => Ok(()),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(LaunchError::NotFound),
        Err(e) => Err(LaunchError::Other(e.to_string())),
    }
}

fn default_backend() -> Backend {
    if cfg!(target_os = "macos") {
        Backend::MacOpen
    } else {
        // BSDs / Wayland-only setups inherit the Linux path — they
        // ship xdg-open more often than not, and the direct-exec
        // fallback covers the rest.
        Backend::LinuxXdg
    }
}

/// Same accent-folding + lowercasing helper the time/date handlers
/// use. Pulled inline because the handler set doesn't have a
/// shared utilities module yet (each handler has its own copy);
/// when a third handler needs it we'll lift it to a common spot.
fn normalise(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            c => c.to_ascii_lowercase(),
        })
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
impl AppLauncherHandler {
    /// Test-only constructor that swaps in the recording backend.
    /// Used by tests that exercise the resolve → refuse → launch
    /// pipeline without actually spawning processes.
    fn with_test_backend(user_aliases: HashMap<String, String>) -> Self {
        Self {
            user_aliases: user_aliases
                .into_iter()
                .map(|(k, v)| (normalise(&k), v))
                .collect(),
            backend: Backend::Test,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler() -> AppLauncherHandler {
        AppLauncherHandler::with_test_backend(HashMap::new())
    }

    /// Every documented launch trigger (ES + EN) recognises and
    /// returns the tail. Locks the trigger list so adding /
    /// removing one is a deliberate code change.
    #[test]
    fn launch_triggers_recognise_and_extract_tail() {
        let h = handler();
        let s = Session::new();
        for (prompt, expected_tail) in [
            ("abre Firefox", "firefox"),
            ("abrir Spotify", "spotify"),
            ("lanza VS Code", "vs code"),
            ("lanzar Slack", "slack"),
            ("inicia Discord", "discord"),
            ("iniciar Telegram", "telegram"),
            ("open Firefox", "firefox"),
            ("launch Spotify", "spotify"),
            ("start Zoom", "zoom"),
        ] {
            let m = h.recognize(prompt, &s);
            assert_eq!(
                m.as_deref(),
                Some(expected_tail),
                "trigger {prompt:?} should yield {expected_tail:?}"
            );
        }
    }

    /// Triggers with empty tail decline so the cascade can route
    /// the bare verb elsewhere (or fall through). "abre" alone
    /// isn't a launch command.
    #[test]
    fn triggers_with_empty_tail_decline() {
        let h = handler();
        let s = Session::new();
        for prompt in ["abre", "abre ", "open", "launch  ", "start"] {
            assert!(
                h.recognize(prompt, &s).is_none(),
                "{prompt:?} should decline (no app tail)"
            );
        }
    }

    /// Non-launch phrases decline.
    #[test]
    fn non_launch_phrases_decline() {
        let h = handler();
        let s = Session::new();
        for prompt in [
            "qué hora es",
            "cuánto es dos más dos",
            "explícame opencode",
            "muéstrame las tareas",
        ] {
            assert!(
                h.recognize(prompt, &s).is_none(),
                "{prompt:?} should decline (not a launch trigger)"
            );
        }
    }

    /// `resolve` maps built-in aliases to their binary, lowercases
    /// the input, and ignores accents.
    #[test]
    fn resolve_uses_builtin_aliases() {
        let h = handler();
        assert_eq!(h.resolve("Firefox"), "firefox");
        assert_eq!(h.resolve("VS Code"), "code");
        assert_eq!(h.resolve("VSCODE"), "code");
        assert_eq!(h.resolve("Spotify"), "spotify");
        assert_eq!(h.resolve("calculadora"), "gnome-calculator");
    }

    /// User aliases beat built-ins for the same friendly name.
    /// Lets a user with a custom Firefox path override the default.
    #[test]
    fn resolve_user_aliases_override_builtins() {
        let mut user = HashMap::new();
        user.insert(
            "firefox".to_string(),
            "/opt/firefox-nightly/firefox".to_string(),
        );
        user.insert("vscode".to_string(), "code-insiders".to_string());
        let h = AppLauncherHandler::with_test_backend(user);
        assert_eq!(h.resolve("firefox"), "/opt/firefox-nightly/firefox");
        assert_eq!(h.resolve("VSCODE"), "code-insiders");
        // Built-ins still work for non-overridden names.
        assert_eq!(h.resolve("Spotify"), "spotify");
    }

    /// Unknown names fall through to the raw (normalised) input —
    /// the launcher then tries them as-is, which `xdg-open` handles
    /// gracefully for `.desktop`-registered names.
    #[test]
    fn resolve_unknown_falls_back_to_normalised_input() {
        let h = handler();
        assert_eq!(h.resolve("signal-desktop"), "signaldesktop");
        // Note: the alphanumeric filter in normalise() drops the
        // hyphen, which is why users with hyphenated binaries
        // either need to add an alias or use xdg-open's
        // friendly-name handling.
        assert_eq!(h.resolve("some-unknown-app"), "someunknownapp");
    }

    /// `is_refused` catches every documented refusal token,
    /// case-insensitively, in either bare or "with-args" form.
    #[test]
    fn is_refused_catches_destructive_commands() {
        for r in [
            "rm",
            "Rm",
            "RM",
            "rm -rf /",
            "sudo",
            "sudo rm",
            "shutdown",
            "Shutdown",
            "reboot",
            "poweroff",
            "killall",
            "systemctl",
            "systemctl reboot",
            "doas",
        ] {
            assert!(is_refused(r), "should refuse: {r:?}");
        }
    }

    /// Legitimate user-app names don't trip the refusal list.
    #[test]
    fn is_refused_allows_normal_apps() {
        for ok in [
            "firefox",
            "code",
            "spotify",
            "obsidian",
            "/usr/bin/firefox",
            "warp-terminal",
            "remmina", // starts with `r` but not refused
        ] {
            assert!(!is_refused(ok), "should allow: {ok:?}");
        }
    }

    /// `invoke` on a known alias produces the success TTS.
    #[test]
    fn invoke_known_app_returns_success_text() {
        let h = handler();
        let resp = h
            .invoke(&WorkerInvocation {
                prompt: "Firefox",
                session_id: None,
                cwd: None,
            })
            .expect("invoke succeeds");
        assert!(
            resp.text.starts_with("Listo, abrí"),
            "expected success TTS, got: {:?}",
            resp.text
        );
        assert!(resp.text.contains("Firefox"));
    }

    /// `invoke` on a missing binary produces the not-found TTS
    /// (via the test backend's `__missing__` sentinel).
    #[test]
    fn invoke_missing_app_returns_not_found_text() {
        let mut user = HashMap::new();
        user.insert("ghost".to_string(), "__missing__".to_string());
        let h = AppLauncherHandler::with_test_backend(user);
        let resp = h
            .invoke(&WorkerInvocation {
                prompt: "ghost",
                session_id: None,
                cwd: None,
            })
            .expect("invoke succeeds");
        assert!(
            resp.text.starts_with("No encuentro"),
            "expected not-found TTS, got: {:?}",
            resp.text
        );
    }

    /// `invoke` on a refused name produces the security TTS and
    /// never reaches the launcher.
    #[test]
    fn invoke_refused_name_returns_security_text() {
        let mut user = HashMap::new();
        user.insert("safe sounding name".to_string(), "rm".to_string());
        let h = AppLauncherHandler::with_test_backend(user);
        let resp = h
            .invoke(&WorkerInvocation {
                prompt: "safe sounding name",
                session_id: None,
                cwd: None,
            })
            .expect("invoke succeeds");
        assert!(
            resp.text.contains("por seguridad"),
            "expected security TTS, got: {:?}",
            resp.text
        );
    }

    /// Trailing punctuation in the prompt doesn't break alias
    /// resolution.
    #[test]
    fn invoke_strips_trailing_punctuation() {
        let h = handler();
        let resp = h
            .invoke(&WorkerInvocation {
                prompt: "Firefox.",
                session_id: None,
                cwd: None,
            })
            .expect("invoke succeeds");
        assert!(resp.text.contains("Firefox"));
    }

    /// `default_backend()` returns the right enum per platform.
    /// We can't really test both branches here without conditional
    /// compilation gymnastics; the assertion locks the current
    /// platform's pick.
    #[test]
    fn default_backend_matches_target_os() {
        let b = default_backend();
        if cfg!(target_os = "macos") {
            assert_eq!(b, Backend::MacOpen);
        } else {
            assert_eq!(b, Backend::LinuxXdg);
        }
    }

    /// IDs match across traits — the registry-lookup invariant.
    #[test]
    fn ids_match_across_traits() {
        let h = handler();
        assert_eq!(IntentMatcher::worker_id(&h), WorkerHandle::id(&h));
    }

    /// Clause-boundary cut: a comma or period after the app name
    /// must not drag follow-on text into the launch argument. Bug
    /// observed 2026-05-16 — STT delivered "abre Firefox no está
    /// sonando. No está abierto." which previously resolved to a
    /// sentence-shaped binary name.
    #[test]
    fn recognize_clips_at_clause_boundary() {
        let h = handler();
        let s = Session::new();
        for (prompt, expected) in [
            ("Abre Firefox, no está sonando", "firefox"),
            ("Abre Firefox. No está abierto", "firefox"),
            ("abre vs code; please", "vs code"),
            ("launch Firefox: now", "firefox"),
        ] {
            assert_eq!(
                h.recognize(prompt, &s).as_deref(),
                Some(expected),
                "{prompt:?} should yield {expected:?}"
            );
        }
    }

    /// Recognise declines when the post-trigger tail looks
    /// sentence-shaped (>4 words). Prevents the matcher from
    /// claiming "open the door and tell me what time it is" as a
    /// launch request.
    #[test]
    fn recognize_declines_sentence_shaped_tail() {
        let h = handler();
        let s = Session::new();
        for prompt in [
            "abre Firefox y luego cierra Chrome y limpia la pantalla porfa",
            "open the door and tell me what time it is please",
        ] {
            assert!(
                h.recognize(prompt, &s).is_none(),
                "{prompt:?} should decline (too long to be an app name)"
            );
        }
    }

    /// Invoke refuses sentence-shaped prompts — the second line of
    /// defense against stage-2 (LLM) dispatcher misrouting a
    /// complaint like "Firefox no está sonando, no está abierto" to
    /// app-launcher.
    #[test]
    fn invoke_refuses_misrouted_sentence_prompt() {
        let h = handler();
        for prompt in [
            "Jarvis, Firefox no está sonando. No está abierto.",
            "el navegador no está respondiendo desde hace un rato la verdad",
            "qué tal el clima en madrid hoy por la tarde",
        ] {
            let resp = h
                .invoke(&WorkerInvocation {
                    prompt,
                    session_id: None,
                    cwd: None,
                })
                .expect("invoke succeeds");
            assert!(
                resp.text.starts_with("No entendí"),
                "expected refusal for {prompt:?}, got: {:?}",
                resp.text
            );
        }
    }

    /// Invoke accepts a stage-2 prompt that still carries the
    /// launch trigger ("abre firefox") and strips it before
    /// resolving. The earlier stage-1 path delivers a bare tail
    /// ("firefox") which also still works — both shapes resolve.
    #[test]
    fn invoke_strips_trigger_from_stage2_prompt() {
        let h = handler();
        for prompt in ["abre Firefox", "Open Firefox", "Firefox"] {
            let resp = h
                .invoke(&WorkerInvocation {
                    prompt,
                    session_id: None,
                    cwd: None,
                })
                .expect("invoke succeeds");
            assert!(
                resp.text.starts_with("Listo, abrí")
                    && resp.text.to_lowercase().contains("firefox"),
                "expected success for {prompt:?}, got: {:?}",
                resp.text
            );
        }
    }

    /// `clip_to_clause` cuts at the first sentence/clause boundary
    /// and otherwise returns the input unchanged.
    #[test]
    fn clip_to_clause_cuts_at_boundary() {
        assert_eq!(clip_to_clause("abre firefox, ya"), "abre firefox");
        assert_eq!(clip_to_clause("abre firefox. ya"), "abre firefox");
        assert_eq!(clip_to_clause("abre firefox"), "abre firefox");
        assert_eq!(clip_to_clause(""), "");
    }

    /// `looks_like_app_name` accepts 1–4-word inputs ≤60 chars and
    /// rejects everything else.
    #[test]
    fn looks_like_app_name_bounds() {
        assert!(looks_like_app_name("firefox"));
        assert!(looks_like_app_name("vs code"));
        assert!(looks_like_app_name("visual studio code"));
        assert!(!looks_like_app_name(""));
        assert!(!looks_like_app_name(
            "this is way too many words for any sensible app name"
        ));
        assert!(!looks_like_app_name(&"a".repeat(61)));
    }
}

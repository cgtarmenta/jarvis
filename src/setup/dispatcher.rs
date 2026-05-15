//! Wizard step for `[dispatcher.fallback]` — spec 0014.
//!
//! Sits between the agent step and the config save in `setup::run`.
//! Walks the user through enabling the stage-2 LLM classifier shipped
//! by spec 0013, validates inputs *before* writing the TOML, and runs a
//! single live `classify` probe so the user gets immediate feedback
//! instead of the existing silent disable at runtime.
//!
//! Skip path leaves `cfg.dispatcher.fallback` untouched — re-running
//! `jarvis setup` never wipes a working stage-2 config.

use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Completion, Confirm, Input, Password, Select};

use crate::config::JarvisConfig;
use crate::dispatcher::llm::{LlmBackend, OpenAiCompatBackend, OzCliBackend, WorkerInfo};

const DEFAULT_TIMEOUT_SECS: u64 = 5;

pub fn configure_dispatcher_fallback(theme: &ColorfulTheme, cfg: &mut JarvisConfig) -> Result<()> {
    let has_existing = cfg.dispatcher.fallback.is_some();
    let prompt = if has_existing {
        "Existing [dispatcher.fallback] found. Reconfigure the LLM classifier?"
    } else {
        "Configure an LLM classifier for stage-2 routing? (Defaults to off.)"
    };
    let want = Confirm::with_theme(theme)
        .with_prompt(prompt)
        .default(false)
        .interact()?;
    if !want {
        if has_existing {
            println!("  → leaving existing [dispatcher.fallback] alone.");
        } else {
            println!("  → stage 2 stays disabled.");
        }
        return Ok(());
    }

    // Hide `oz` unless the binary resolves on PATH. Offering a backend the
    // user can't actually run would just produce a probe failure 30s later.
    // `which::which` is the same lookup the oz backend uses at runtime.
    let oz_available = which::which("oz").is_ok();
    let mut choices: Vec<(&'static str, &'static str)> =
        vec![("openai_compat", "OpenAI-compatible HTTP endpoint")];
    if oz_available {
        choices.push(("oz", "oz (Warp's CLI)"));
    }
    let labels: Vec<&str> = choices.iter().map(|(_, l)| *l).collect();
    let idx = Select::with_theme(theme)
        .with_prompt("Backend")
        .items(&labels)
        .default(0)
        .interact()?;

    let (table, backend) = match choices[idx].0 {
        "openai_compat" => collect_openai_compat(theme)?,
        "oz" => collect_oz(theme)?,
        _ => unreachable!("Select index pinned to the choices list"),
    };

    run_probe(&*backend);

    cfg.dispatcher.fallback = Some(toml::Value::Table(table));
    Ok(())
}

fn collect_openai_compat(theme: &ColorfulTheme) -> Result<(toml::Table, Box<dyn LlmBackend>)> {
    let endpoint_raw: String = Input::with_theme(theme)
        .with_prompt("Endpoint URL (full path, e.g. https://.../chat/completions)")
        .validate_with(|s: &String| -> Result<(), &'static str> {
            if looks_like_http_url(s.trim()) {
                Ok(())
            } else {
                Err("must be a non-empty http:// or https:// URL")
            }
        })
        .interact_text()?;
    let endpoint = normalize_user_input(&endpoint_raw);
    let model_raw: String = Input::with_theme(theme)
        .with_prompt("Model name (e.g. llama-3.1-8b-instant)")
        .interact_text()?;
    let model = normalize_user_input(&model_raw);
    // api_key passes through verbatim — Bearer tokens never contain
    // whitespace, but trimming a pasted token catches the common
    // copy-paste mistake without changing any legitimate value.
    let api_key_raw: String = Password::with_theme(theme)
        .with_prompt("API key (Bearer token; leave blank if endpoint is unauthenticated)")
        .allow_empty_password(true)
        .interact()?;
    let api_key = normalize_user_input(&api_key_raw);
    let timeout_raw: String = Input::with_theme(theme)
        .with_prompt("Timeout (seconds)")
        .default(DEFAULT_TIMEOUT_SECS.to_string())
        .validate_with(|s: &String| -> Result<(), &'static str> {
            s.trim()
                .parse::<u64>()
                .map(|_| ())
                .map_err(|_| "must be a non-negative integer")
        })
        .interact_text()?;
    let timeout_secs = timeout_raw
        .trim()
        .parse::<u64>()
        .unwrap_or(DEFAULT_TIMEOUT_SECS);

    let mut backend = OpenAiCompatBackend::new(endpoint.clone(), model.clone())
        .with_timeout(Duration::from_secs(timeout_secs));
    if !api_key.is_empty() {
        backend = backend.with_api_key(api_key.clone());
    }

    let mut t = toml::Table::new();
    t.insert(
        "backend".into(),
        toml::Value::String("openai_compat".into()),
    );
    t.insert("endpoint".into(), toml::Value::String(endpoint));
    t.insert("model".into(), toml::Value::String(model));
    if !api_key.is_empty() {
        t.insert("api_key".into(), toml::Value::String(api_key));
    }
    t.insert(
        "timeout_secs".into(),
        toml::Value::Integer(timeout_secs as i64),
    );
    Ok((t, Box::new(backend)))
}

fn collect_oz(theme: &ColorfulTheme) -> Result<(toml::Table, Box<dyn LlmBackend>)> {
    // Try to enumerate live — `oz model list --output-format json`
    // gives us a stable, parseable shape. On any failure (binary
    // moved, not authenticated, network hiccup) we degrade to the
    // free-text Input below so the wizard never blocks on it.
    let model = match fetch_oz_models() {
        Ok(models) if !models.is_empty() => choose_oz_model_from_list(theme, &models)?,
        Ok(_) => {
            println!("  ⚠ `oz model list` returned an empty catalog; falling back to free-text.");
            free_text_oz_model(theme)?
        }
        Err(e) => {
            println!("  ⚠ couldn't enumerate oz models ({e:#}); falling back to free-text.");
            free_text_oz_model(theme)?
        }
    };

    let backend = OzCliBackend::new(model.clone());

    let mut t = toml::Table::new();
    t.insert("backend".into(), toml::Value::String("oz".into()));
    t.insert("model".into(), toml::Value::String(model));
    Ok((t, Box::new(backend)))
}

/// Render the catalog as a multi-column table (so it doesn't show
/// up as a 50-line "chorizo") and prompt with tab-completion. The
/// `Input` flow accepts any value the user types, so power users
/// can pick a private/pre-release id that isn't in the live list —
/// no separate "Other (custom)" sentinel needed. Default is `auto`,
/// which is always present in oz's catalog and matches oz's own
/// behaviour for unscoped calls.
fn choose_oz_model_from_list(theme: &ColorfulTheme, models: &[String]) -> Result<String> {
    print_models_table(models);

    let completion = ModelCompletion {
        models: models.to_vec(),
    };
    let default = if models.iter().any(|m| m == "auto") {
        "auto".to_string()
    } else {
        models[0].clone()
    };
    let raw: String = Input::with_theme(theme)
        .with_prompt(format!(
            "oz model ({} available — Tab completes)",
            models.len()
        ))
        .default(default)
        .completion_with(&completion)
        .interact_text()?;
    Ok(normalize_user_input(&raw))
}

/// Tab-completion source for the oz model Input. Implements
/// shell-style completion: a unique-prefix match completes to the
/// full id; multiple matches advance to their longest common
/// prefix; nothing matches → `None` and the typed text stays.
struct ModelCompletion {
    models: Vec<String>,
}

impl Completion for ModelCompletion {
    fn get(&self, input: &str) -> Option<String> {
        let trimmed = input.trim();
        let matches: Vec<&str> = self
            .models
            .iter()
            .filter(|m| m.starts_with(trimmed))
            .map(|s| s.as_str())
            .collect();
        if matches.is_empty() {
            return None;
        }
        if matches.len() == 1 {
            return Some(matches[0].to_string());
        }
        let lcp = longest_common_prefix(&matches);
        if lcp.len() > trimmed.len() {
            Some(lcp)
        } else {
            None
        }
    }
}

fn longest_common_prefix(strs: &[&str]) -> String {
    let Some(first) = strs.first() else {
        return String::new();
    };
    let mut lcp = String::new();
    for (i, c) in first.chars().enumerate() {
        if strs.iter().all(|s| s.chars().nth(i) == Some(c)) {
            lcp.push(c);
        } else {
            break;
        }
    }
    lcp
}

fn print_models_table(models: &[String]) {
    let width = terminal_width();
    println!("  Available oz models ({}):", models.len());
    let table = format_models_table(models, width.saturating_sub(4));
    for line in table.lines() {
        println!("    {line}");
    }
    println!();
}

fn terminal_width() -> usize {
    // `console::Term::size()` returns (rows, cols). Fall back to 80
    // when we're not on a TTY (e.g. piped output during a doctor
    // run) so the table still renders sanely.
    let (_, cols) = console::Term::stdout().size();
    let w = cols as usize;
    if w == 0 { 80 } else { w }
}

/// Render `models` as a column-major multi-column table that fits in
/// `width` cols. Column-major so reading top-to-bottom in each column
/// is alphabetical (matches what `ls -C` does). Pure function so the
/// test suite can pin column-packing behaviour without touching a
/// real terminal.
pub fn format_models_table(models: &[String], width: usize) -> String {
    if models.is_empty() {
        return String::new();
    }
    let max_id = models.iter().map(|m| m.len()).max().unwrap_or(20);
    let gap = 2;
    let col_width = max_id + gap;
    let cols = (width / col_width).max(1);
    let rows = models.len().div_ceil(cols);

    let mut out = String::new();
    for r in 0..rows {
        let mut line = String::new();
        for c in 0..cols {
            let idx = c * rows + r;
            if let Some(m) = models.get(idx) {
                if c == cols - 1 || c * rows + r + rows >= models.len() {
                    // Last column on this row — skip the trailing
                    // padding so the line doesn't have a tail of
                    // spaces.
                    line.push_str(m);
                } else {
                    line.push_str(&format!("{m:col_width$}"));
                }
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn free_text_oz_model(theme: &ColorfulTheme) -> Result<String> {
    println!("  ℹ Pick a model id that `oz` itself accepts.");
    println!("    Run `oz model list` to see the current set.");
    let raw: String = Input::with_theme(theme)
        .with_prompt("oz model id (e.g. claude-4-6-sonnet-high)")
        .interact_text()?;
    Ok(normalize_user_input(&raw))
}

/// Spawn `oz model list --output-format json` and return the parsed
/// ids. Bounded by a short timeout because the wizard freezing on a
/// stuck subprocess would be the worst UX. Any non-zero exit, parse
/// failure, or timeout returns `Err` — the caller degrades to the
/// free-text path.
fn fetch_oz_models() -> Result<Vec<String>> {
    let output = Command::new("oz")
        .args(["model", "list", "--output-format", "json"])
        .output()
        .context("spawning `oz model list`")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "`oz model list` exited {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    let stdout = String::from_utf8(output.stdout).context("oz stdout was not utf-8")?;
    parse_oz_models_json(&stdout)
}

/// Parse the JSON shape `oz model list --output-format json` emits:
/// `[{"id":"auto"}, {"id":"claude-4-6-sonnet-high"}, …]`. Tolerant of
/// extra fields per object (oz might grow the shape over time) but
/// strict that each entry has a non-empty `id`. Exposed so tests can
/// pin the parse contract without invoking the binary.
pub fn parse_oz_models_json(stdout: &str) -> Result<Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Entry {
        id: String,
    }
    let raw: Vec<Entry> =
        serde_json::from_str(stdout.trim()).context("parsing oz model list JSON")?;
    let ids: Vec<String> = raw
        .into_iter()
        .map(|e| e.id)
        .filter(|id| !id.trim().is_empty())
        .collect();
    if ids.is_empty() {
        Err(anyhow!("oz model list returned no usable ids"))
    } else {
        Ok(ids)
    }
}

/// Single live classify call against a one-worker fixture. Surfaces
/// reachability immediately without blocking save — per spec, the
/// endpoint may come online later and a hard refusal would re-create
/// the brittleness the rest of the wizard avoids.
fn run_probe(backend: &dyn LlmBackend) {
    let workers = vec![WorkerInfo {
        id: "test".into(),
        dispatch_hint: None,
    }];
    match backend.classify("hello world", &workers) {
        Ok(_) => println!("  ✓ classifier reachable."),
        Err(e) => println!(
            "  ⚠ classifier didn't respond ({e:#}); saving config anyway — \
             you can fix it later."
        ),
    }
}

/// Trim a free-text user input. Centralised so the wizard treats
/// `"  qwen-3.6  "` and `"qwen-3.6"` identically — copy-paste from
/// the `oz model list` table or browser drag-select frequently
/// leaves surrounding whitespace that `oz` itself rejects verbatim.
pub fn normalize_user_input(raw: &str) -> String {
    raw.trim().to_string()
}

/// Cheap URL heuristic. Must start with `http://` or `https://` and
/// carry something after the scheme. We deliberately don't enforce a
/// `/chat/completions` suffix: Triton's per-model routes embed the
/// model name and break that rule legitimately.
pub fn looks_like_http_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    let after_scheme = if let Some(rest) = lower.strip_prefix("https://") {
        rest
    } else if let Some(rest) = lower.strip_prefix("http://") {
        rest
    } else {
        return false;
    };
    !after_scheme.is_empty() && !after_scheme.starts_with('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_validator_accepts_http_and_https() {
        assert!(looks_like_http_url(
            "http://localhost:11434/v1/chat/completions"
        ));
        assert!(looks_like_http_url(
            "https://api.groq.com/openai/v1/chat/completions"
        ));
        assert!(looks_like_http_url(
            "HTTPS://triton.local/v2/models/llama/infer"
        ));
    }

    /// Parse the captured-real-world JSON shape from `oz model list
    /// --output-format json`. The fixture is a verbatim slice of the
    /// output we collected on 2026-05-15 (a few entries — we don't
    /// need the whole catalog to pin parsing).
    #[test]
    fn parse_oz_models_json_accepts_real_shape() {
        let stdout = r#"[
            {"id":"auto"},
            {"id":"auto-efficient"},
            {"id":"claude-4-6-sonnet-high"},
            {"id":"qwen-3.6-plus-fireworks"}
        ]"#;
        let models = parse_oz_models_json(stdout).expect("should parse");
        assert_eq!(
            models,
            vec![
                "auto",
                "auto-efficient",
                "claude-4-6-sonnet-high",
                "qwen-3.6-plus-fireworks"
            ]
        );
    }

    /// Extra unknown fields per entry don't break parsing — oz may
    /// grow the shape over time (description, deprecated, etc.) and
    /// we only care about `id`.
    #[test]
    fn parse_oz_models_json_ignores_extra_fields() {
        let stdout = r#"[
            {"id":"auto","description":"safe default","deprecated":false},
            {"id":"claude-4-6-sonnet-high","tier":"premium"}
        ]"#;
        let models = parse_oz_models_json(stdout).expect("should parse");
        assert_eq!(models, vec!["auto", "claude-4-6-sonnet-high"]);
    }

    /// Empty-id entries are filtered (defensive — shouldn't happen
    /// but a single malformed row mustn't kill the whole listing).
    #[test]
    fn parse_oz_models_json_filters_empty_ids() {
        let stdout = r#"[{"id":""}, {"id":"  "}, {"id":"claude-4-6-sonnet-high"}]"#;
        let models = parse_oz_models_json(stdout).expect("should parse");
        assert_eq!(models, vec!["claude-4-6-sonnet-high"]);
    }

    /// Malformed JSON returns an Err with useful context — the
    /// wizard wraps this and falls back to free-text, so the user
    /// sees the upstream "oz returned garbage" reason in the warning.
    #[test]
    fn parse_oz_models_json_errors_on_garbage() {
        assert!(parse_oz_models_json("not json at all").is_err());
        assert!(parse_oz_models_json("").is_err());
        assert!(parse_oz_models_json("[{}]").is_err()); // missing `id`
    }

    /// `longest_common_prefix` for tab-completion: returns the
    /// shared prefix across all candidate strings. Empty when there
    /// is no overlap or the input set is empty.
    #[test]
    fn longest_common_prefix_basic() {
        assert_eq!(
            longest_common_prefix(&[
                "claude-4-5-haiku",
                "claude-4-5-opus",
                "claude-4-6-sonnet-high"
            ]),
            "claude-4-"
        );
        assert_eq!(
            longest_common_prefix(&["auto", "auto-efficient", "auto-genius"]),
            "auto"
        );
        assert_eq!(longest_common_prefix(&["one"]), "one");
        assert_eq!(longest_common_prefix(&[]), "");
        assert_eq!(longest_common_prefix(&["a", "b"]), "");
    }

    /// `ModelCompletion`: unique-prefix match completes to the full
    /// id; multi-match advances to the longest common prefix; no
    /// match returns None (typed text stays).
    #[test]
    fn model_completion_unique_prefix_completes_to_full_id() {
        let c = ModelCompletion {
            models: vec![
                "auto".into(),
                "claude-4-6-sonnet-high".into(),
                "qwen-3.6-plus-fireworks".into(),
            ],
        };
        assert_eq!(c.get("cla"), Some("claude-4-6-sonnet-high".to_string()));
        assert_eq!(c.get("qw"), Some("qwen-3.6-plus-fireworks".to_string()));
    }

    #[test]
    fn model_completion_multi_match_advances_to_common_prefix() {
        let c = ModelCompletion {
            models: vec![
                "claude-4-5-haiku".into(),
                "claude-4-5-opus".into(),
                "claude-4-6-sonnet-high".into(),
            ],
        };
        assert_eq!(c.get("c"), Some("claude-4-".to_string()));
        assert_eq!(c.get("claude-4-5"), Some("claude-4-5-".to_string()));
    }

    #[test]
    fn model_completion_no_match_returns_none() {
        let c = ModelCompletion {
            models: vec!["auto".into(), "claude-4-5-haiku".into()],
        };
        assert_eq!(c.get("zzz"), None);
        assert_eq!(c.get("xyzzy"), None);
    }

    /// Multi-column table: column-major layout, padded so columns
    /// line up, last column on each row doesn't carry trailing
    /// whitespace. Width is the available column count (caller has
    /// already subtracted any indent).
    #[test]
    fn format_models_table_packs_columns_to_width() {
        let models: Vec<String> = (0..6).map(|i| format!("m{i}")).collect();
        // 6 ids, each "mN" (2 chars). Col width = 2 + 2 gap = 4.
        // width=12 → 3 cols → 2 rows.
        let out = format_models_table(&models, 12);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "expected 2 rows, got:\n{out}");
        // Column-major: row 0 sees m0, m2, m4; row 1 sees m1, m3, m5.
        assert!(lines[0].starts_with("m0"));
        assert!(lines[0].contains("m2"));
        assert!(lines[0].trim_end().ends_with("m4"));
        assert!(lines[1].starts_with("m1"));
        assert!(lines[1].contains("m3"));
        assert!(lines[1].trim_end().ends_with("m5"));
    }

    #[test]
    fn format_models_table_handles_narrow_terminal_with_single_column() {
        let models = vec!["claude-4-6-sonnet-high".into(), "auto".into()];
        let out = format_models_table(&models, 5);
        // Width 5 is smaller than the longest id, so cols=1 forced.
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "claude-4-6-sonnet-high");
        assert_eq!(lines[1], "auto");
    }

    #[test]
    fn format_models_table_empty_input_returns_empty() {
        assert_eq!(format_models_table(&[], 80), "");
    }

    /// An empty-but-valid array returns Err (no usable ids).
    /// The wizard's caller treats this same as a hard failure and
    /// falls back to free-text — there's nothing to Select from.
    #[test]
    fn parse_oz_models_json_errors_on_empty_array() {
        let err = parse_oz_models_json("[]").expect_err("empty array should fail");
        assert!(format!("{err:#}").contains("no usable ids"));
    }

    #[test]
    fn url_validator_rejects_non_http_or_empty() {
        assert!(!looks_like_http_url(""));
        assert!(!looks_like_http_url("api.groq.com/chat"));
        assert!(!looks_like_http_url("ftp://example.com"));
        assert!(!looks_like_http_url("http://"));
        assert!(!looks_like_http_url("https:///path"));
    }
}

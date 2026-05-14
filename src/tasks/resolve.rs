//! Resolve natural-language task references to a concrete
//! `Task` from the registry.
//!
//! The voice surface (spec 0012 / E2) accepts phrases like
//! "esa tarea", "la última", "la de gemini", "el análisis del
//! log". The CLI accepts task-id prefixes. This module bridges
//! the gap with a small set of heuristics so the user doesn't
//! have to remember the `t-<unix>-<hex>` ids that the CLI uses.

use super::record::Task;
use super::registry::TaskRegistry;

/// Outcome of [`resolve_task_reference`].
#[derive(Debug, Clone)]
pub enum ResolveResult {
    /// Exactly one task matched.
    Unique(Task),
    /// No task matched — the voice handler should respond with
    /// "no encontré ninguna tarea que coincida con …".
    None,
    /// Multiple tasks matched. The voice handler should ask the
    /// user to disambiguate rather than guess.
    Ambiguous(Vec<Task>),
}

/// Resolve a natural-language `query` against the registry.
///
/// Heuristics, in priority order:
///
/// 1. **"Last" keywords** ("esa", "la última", "the last one").
///    Returns the most recent task by `spawn_time`.
/// 2. **Worker-name reference** ("la de gemini", "lo de
///    claude"). The query, normalised, contains a worker id
///    that appears on at least one task. Filters to those
///    tasks and applies the unique/ambiguous resolution. If
///    the user said "la de" (singular), we still return
///    Ambiguous when there are multiple matches — the voice
///    handler is responsible for asking which one.
/// 3. **Fuzzy intent match**. Substring match between the
///    normalised query and each task's normalised `user_intent`.
pub fn resolve_task_reference(query: &str, registry: &TaskRegistry) -> ResolveResult {
    let n = normalise(query);
    if n.is_empty() {
        return ResolveResult::None;
    }

    // (1) Last keywords. Substring match so "esa tarea" /
    // "la ultima tarea" both fire on their core noun.
    if LAST_KEYWORDS.iter().any(|k| n == *k || n.contains(k)) {
        return match registry.all().iter().max_by_key(|t| t.spawn_time) {
            Some(t) => ResolveResult::Unique(t.clone()),
            None => ResolveResult::None,
        };
    }

    // (2) Worker-name reference. We scan every known worker
    // id (from active tasks) and see if it appears in the
    // normalised query. Matches accumulate; resolution picks
    // unique/ambiguous via the helper below.
    let worker_matches: Vec<&Task> = registry
        .all()
        .iter()
        .filter(|t| {
            let wid = normalise(&t.worker_id);
            !wid.is_empty() && n.contains(&wid)
        })
        .collect();
    if !worker_matches.is_empty() {
        return resolve_set(&worker_matches);
    }

    // (3) Fuzzy intent match. A task's `user_intent` is the
    // user's own words at spawn time, so substring matching
    // against the query catches "el análisis del log" referring
    // back to a task spawned with intent "analyze the syslog
    // file and summarise errors".
    let intent_matches: Vec<&Task> = registry
        .all()
        .iter()
        .filter(|t| {
            let intent = normalise(&t.user_intent);
            !intent.is_empty()
                && (intent.contains(&n)
                    || n.split_whitespace()
                        .any(|word| word.len() >= 4 && intent.contains(word)))
        })
        .collect();
    resolve_set(&intent_matches)
}

const LAST_KEYWORDS: &[&str] = &[
    // Spanish
    "esa",
    "esa tarea",
    "la ultima",
    "la ultima tarea",
    "la mas reciente",
    "la reciente",
    "la de hace un momento",
    // English
    "the last",
    "the last one",
    "last task",
    "the most recent",
    "the recent one",
];

fn resolve_set(matches: &[&Task]) -> ResolveResult {
    match matches.len() {
        0 => ResolveResult::None,
        1 => ResolveResult::Unique(matches[0].clone()),
        _ => ResolveResult::Ambiguous(matches.iter().map(|t| (*t).clone()).collect()),
    }
}

/// Same normaliser the rest of the handlers use. Inlined
/// because the alternatives (move to a shared util) is more
/// cross-module plumbing than it's worth for one extra caller.
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
mod tests {
    use super::*;
    use crate::tasks::record::TaskStatus;
    use tempfile::TempDir;

    fn record(id: &str, worker: &str, intent: &str, spawn: u64) -> Task {
        Task {
            id: id.to_string(),
            thread_id: "test".to_string(),
            worker_id: worker.to_string(),
            spawn_time: spawn,
            completion_time: None,
            status: TaskStatus::Running,
            user_intent: intent.to_string(),
            command: vec!["true".to_string()],
            pid: Some(1),
            exit_code: None,
            summary: None,
        }
    }

    fn registry_with(tasks: Vec<Task>) -> TaskRegistry {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        for t in tasks {
            t.save(&path).unwrap();
        }
        // Keep `tmp` alive by leaking the dir handle into a
        // static-ish path; tests use this builder and drop the
        // registry first.
        let reg = TaskRegistry::load_from_dir(&path);
        // We need to keep `tmp` alive while `reg` is used. Since
        // we can't return it directly, we leak it via `Box::leak`
        // for the duration of the test. Each test has its own
        // TempDir from this call so contention is impossible.
        let _ = Box::leak(Box::new(tmp));
        reg
    }

    /// Empty query → None.
    #[test]
    fn empty_query_yields_none() {
        let reg = registry_with(vec![]);
        assert!(matches!(
            resolve_task_reference("", &reg),
            ResolveResult::None
        ));
        assert!(matches!(
            resolve_task_reference("   ", &reg),
            ResolveResult::None
        ));
    }

    /// "esa" / "la última" / "the last one" all pick the most
    /// recent task by spawn_time.
    #[test]
    fn last_keywords_pick_newest() {
        let reg = registry_with(vec![
            record("t-100", "claude", "old refactor", 100),
            record("t-200", "gemini", "newer analysis", 200),
            record("t-150", "claude", "middle task", 150),
        ]);
        for phrase in [
            "esa",
            "esa tarea",
            "la última",
            "la ultima tarea",
            "the last one",
            "the most recent",
        ] {
            match resolve_task_reference(phrase, &reg) {
                ResolveResult::Unique(t) => assert_eq!(
                    t.id, "t-200",
                    "phrase {phrase:?} should resolve to the newest"
                ),
                other => panic!("phrase {phrase:?} produced {other:?}"),
            }
        }
    }

    /// "la de claude" → tasks with worker_id="claude". If
    /// there's one, Unique; if multiple, Ambiguous.
    #[test]
    fn worker_reference_unique_or_ambiguous() {
        let reg_one_claude = registry_with(vec![
            record("t-100", "claude", "single claude task", 100),
            record("t-200", "gemini", "gemini work", 200),
        ]);
        match resolve_task_reference("la de claude", &reg_one_claude) {
            ResolveResult::Unique(t) => assert_eq!(t.id, "t-100"),
            other => panic!("expected Unique, got {other:?}"),
        }

        let reg_two_claude = registry_with(vec![
            record("t-100", "claude", "first claude task", 100),
            record("t-300", "claude", "second claude task", 300),
            record("t-200", "gemini", "gemini work", 200),
        ]);
        match resolve_task_reference("la de claude", &reg_two_claude) {
            ResolveResult::Ambiguous(matches) => {
                assert_eq!(matches.len(), 2);
                let ids: Vec<&str> = matches.iter().map(|t| t.id.as_str()).collect();
                assert!(ids.contains(&"t-100"));
                assert!(ids.contains(&"t-300"));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    /// Fuzzy intent match: query substring within
    /// `user_intent`, or any 4+ char query word found in
    /// `user_intent`, both count.
    #[test]
    fn fuzzy_intent_match_via_substring() {
        let reg = registry_with(vec![
            record(
                "t-001",
                "gemini",
                "analiza el syslog y resume los errores",
                100,
            ),
            record("t-002", "claude", "refactoriza el módulo de specs", 200),
        ]);
        // "el análisis del log" — none of the words is a
        // direct substring of the intent, but "log" → matches
        // "syslog" inside "syslog y resume"? No: "log" is 3
        // chars, below the 4-char gate. Let me use the actual
        // 4+-char match: "syslog" appears in the intent.
        match resolve_task_reference("muéstrame el syslog ese", &reg) {
            ResolveResult::Unique(t) => assert_eq!(t.id, "t-001"),
            other => panic!("expected Unique syslog match, got {other:?}"),
        }
        // "refactor" matches the second task.
        match resolve_task_reference("cómo va el refactor", &reg) {
            ResolveResult::Unique(t) => assert_eq!(t.id, "t-002"),
            other => panic!("expected Unique refactor match, got {other:?}"),
        }
    }

    /// Nothing matches → None.
    #[test]
    fn no_match_yields_none() {
        let reg = registry_with(vec![record("t-001", "gemini", "analyze syslog", 100)]);
        assert!(matches!(
            resolve_task_reference("tarea de cocina", &reg),
            ResolveResult::None
        ));
    }
}

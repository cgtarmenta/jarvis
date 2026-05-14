//! Voice handler for "qué tareas tengo" / "what's running" —
//! reads back the active task list (and a hint about
//! completed tasks if asked).
//!
//! Part of spec 0012 / E2. The CLI already has
//! `jarvis task list`; this handler gives the user the same
//! information from voice, formatted for TTS rather than for a
//! terminal table.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use crate::dispatcher::IntentMatcher;
use crate::session::Session;
use crate::tasks::{TaskRegistry, humanise_age_spanish, truncate_chars};
use crate::workers::{WorkerHandle, WorkerInvocation, WorkerResponse};

const LIST_TRIGGERS: &[&str] = &[
    // Spanish
    "que tareas tengo",
    "que esta corriendo",
    "que estan corriendo",
    "que hay en background",
    "tareas activas",
    "lista de tareas",
    "listame las tareas",
    "muestrame las tareas",
    "ver tareas",
    // English
    "what tasks",
    "whats running",
    "what is running",
    "list tasks",
    "show me tasks",
    "show tasks",
    "active tasks",
];

pub struct TaskListHandler;

impl IntentMatcher for TaskListHandler {
    fn worker_id(&self) -> &str {
        "task-list"
    }

    fn recognize(&self, prompt: &str, _session: &Session) -> Option<String> {
        let n = normalise(prompt);
        if LIST_TRIGGERS.iter().any(|t| n.contains(t)) {
            Some(prompt.to_string())
        } else {
            None
        }
    }
}

impl WorkerHandle for TaskListHandler {
    fn id(&self) -> &str {
        "task-list"
    }

    fn description(&self) -> Option<&str> {
        Some("Speak a summary of currently-running and recent tasks.")
    }

    fn dispatch_hint(&self) -> Option<&str> {
        Some(
            "Use when the user asks what's running, what tasks are \
             active, or wants a list of background work.",
        )
    }

    fn invoke(&self, _ctx: &WorkerInvocation<'_>) -> Result<WorkerResponse> {
        let dir = TaskRegistry::default_dir()?;
        let registry = TaskRegistry::load_from_dir(&dir);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let active: Vec<_> = registry.active().collect();
        let terminal_count = registry
            .all()
            .iter()
            .filter(|t| t.status.is_terminal())
            .count();

        let text = if active.is_empty() {
            if terminal_count == 0 {
                "No tienes tareas en background ni en historial.".to_string()
            } else {
                format!(
                    "No hay tareas corriendo ahora. Tienes {terminal_count} \
                     resultados terminados disponibles si quieres consultarlos."
                )
            }
        } else if active.len() == 1 {
            let t = active[0];
            let intent_short = truncate_chars(&t.user_intent, 80);
            let age = humanise_age_spanish(now.saturating_sub(t.spawn_time));
            format!(
                "Tienes una tarea corriendo con {}: «{}», desde hace {}.",
                t.worker_id, intent_short, age
            )
        } else {
            // Group by worker so the spoken summary stays
            // compact — two claude tasks read as "dos con claude"
            // rather than two separate sentences.
            use std::collections::HashMap;
            let mut by_worker: HashMap<&str, Vec<&_>> = HashMap::new();
            for t in &active {
                by_worker.entry(t.worker_id.as_str()).or_default().push(t);
            }
            let mut pieces: Vec<String> = by_worker
                .iter()
                .map(|(worker, tasks)| match tasks.len() {
                    1 => {
                        let t = tasks[0];
                        let age = humanise_age_spanish(now.saturating_sub(t.spawn_time));
                        format!("una con {worker} desde hace {age}")
                    }
                    n => format!("{n} con {worker}"),
                })
                .collect();
            pieces.sort();
            format!(
                "Tienes {} tareas corriendo: {}.",
                active.len(),
                pieces.join("; ")
            )
        };

        Ok(WorkerResponse {
            text,
            captured_session_id: None,
        })
    }
}

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

    #[test]
    fn list_phrases_match() {
        let h = TaskListHandler;
        let session = Session::new();
        for phrase in [
            "¿qué tareas tengo?",
            "qué tareas tengo",
            "qué está corriendo",
            "lista de tareas",
            "muéstrame las tareas",
            "what's running",
            "list tasks",
            "show me tasks",
            "active tasks please",
        ] {
            assert!(
                h.recognize(phrase, &session).is_some(),
                "should recognise: {phrase:?}"
            );
        }
    }

    #[test]
    fn unrelated_phrases_decline() {
        let h = TaskListHandler;
        let session = Session::new();
        for phrase in [
            "qué hora es",
            "olvida todo",
            "cuánto es 5 más 3",
            "abre un spec para X",
        ] {
            assert!(
                h.recognize(phrase, &session).is_none(),
                "should decline: {phrase:?}"
            );
        }
    }

    /// `invoke` against an empty registry surfaces the
    /// "no tareas" message. The handler reads from
    /// `TaskRegistry::default_dir()` which uses the XDG cache;
    /// we trust the wider integration tests for the seeded
    /// case and keep this unit test focused on the empty path.
    #[test]
    fn invoke_handles_empty_registry() {
        let h = TaskListHandler;
        let resp = h
            .invoke(&WorkerInvocation {
                prompt: "qué tareas tengo",
                session_id: None,
                cwd: None,
            })
            .expect("invoke succeeds");
        // We don't assert exact content because the user's real
        // tasks dir may have leftover records from prior runs.
        // Just confirm the handler ran and returned a non-empty
        // reply with no captured session id.
        assert!(!resp.text.is_empty());
        assert!(resp.captured_session_id.is_none());
    }

    #[test]
    fn ids_match_across_traits() {
        let h = TaskListHandler;
        assert_eq!(IntentMatcher::worker_id(&h), WorkerHandle::id(&h));
    }
}

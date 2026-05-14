//! Voice handler for "limpia las tareas viejas" / "clean up
//! old tasks". Runs the same `clean_old_tasks` primitive the
//! CLI does, but with a fixed default cutoff (7 days) instead
//! of the user-supplied `--older-than` flag.
//!
//! Part of spec 0012 / E2.

use std::time::Duration;

use anyhow::Result;

use crate::dispatcher::IntentMatcher;
use crate::session::Session;
use crate::tasks::{TaskRegistry, clean_old_tasks};
use crate::workers::{WorkerHandle, WorkerInvocation, WorkerResponse};

const CLEAN_TRIGGERS: &[&str] = &[
    // Spanish
    "limpia las tareas",
    "limpia tareas viejas",
    "limpia las viejas",
    "borra las tareas terminadas",
    "borra las viejas",
    "purga las tareas",
    "tira las tareas viejas",
    // English
    "clean up tasks",
    "clean up old tasks",
    "clean tasks",
    "purge tasks",
    "remove old tasks",
    "delete old tasks",
];

/// 7 days. Voice users don't supply a duration; the CLI's
/// `--older-than` flag is for power users who want different
/// cutoffs.
const VOICE_CUTOFF: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub struct TaskCleanHandler;

impl IntentMatcher for TaskCleanHandler {
    fn worker_id(&self) -> &str {
        "task-clean"
    }

    fn recognize(&self, prompt: &str, _session: &Session) -> Option<String> {
        let n = normalise(prompt);
        if CLEAN_TRIGGERS.iter().any(|t| n.contains(t)) {
            Some(prompt.to_string())
        } else {
            None
        }
    }
}

impl WorkerHandle for TaskCleanHandler {
    fn id(&self) -> &str {
        "task-clean"
    }

    fn description(&self) -> Option<&str> {
        Some("Prune terminal-status tasks older than seven days.")
    }

    fn dispatch_hint(&self) -> Option<&str> {
        Some(
            "Use when the user asks to clean up, purge, or delete old completed/failed/cancelled \
             tasks. Always uses a 7-day cutoff (use the CLI `jarvis task clean --older-than` for \
             custom durations).",
        )
    }

    fn invoke(&self, _ctx: &WorkerInvocation<'_>) -> Result<WorkerResponse> {
        let dir = TaskRegistry::default_dir()?;
        let removed = clean_old_tasks(&dir, VOICE_CUTOFF)?;
        let text = match removed {
            0 => "No había tareas viejas que purgar.".to_string(),
            1 => "Listo, purgué una tarea de hace más de una semana.".to_string(),
            n => format!("Listo, purgué {n} tareas de hace más de una semana."),
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
    fn clean_phrases_match() {
        let h = TaskCleanHandler;
        let session = Session::new();
        for phrase in [
            "limpia las tareas viejas",
            "limpia las viejas",
            "borra las tareas terminadas",
            "purga las tareas",
            "clean up old tasks",
            "purge tasks",
            "delete old tasks",
        ] {
            assert!(
                h.recognize(phrase, &session).is_some(),
                "should recognise: {phrase:?}"
            );
        }
    }

    #[test]
    fn unrelated_phrases_decline() {
        let h = TaskCleanHandler;
        let session = Session::new();
        for phrase in [
            "qué tareas tengo",
            "cancela esa tarea",
            "muéstrame el resultado",
            "qué hora es",
        ] {
            assert!(
                h.recognize(phrase, &session).is_none(),
                "should decline: {phrase:?}"
            );
        }
    }

    #[test]
    fn ids_match_across_traits() {
        let h = TaskCleanHandler;
        assert_eq!(IntentMatcher::worker_id(&h), WorkerHandle::id(&h));
    }
}

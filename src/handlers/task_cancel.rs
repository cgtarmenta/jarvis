//! Voice handler for "cancela esa tarea" / "cancel the gemini
//! task". Resolves a natural-language reference and runs the
//! same shared cancel primitive as the CLI.
//!
//! Part of spec 0012 / E2.

use anyhow::Result;

use crate::dispatcher::IntentMatcher;
use crate::session::Session;
use crate::tasks::{ResolveResult, TaskRegistry, cancel_task, resolve_task_reference};
use crate::workers::{WorkerHandle, WorkerInvocation, WorkerResponse};

const CANCEL_TRIGGERS: &[&str] = &[
    // Spanish
    "cancela",
    "cancela la tarea",
    "cancela esa",
    "cancela la ultima",
    "cancela la de",
    "para la tarea",
    "para esa",
    "deten la tarea",
    "deten esa",
    // English
    "cancel that",
    "cancel the",
    "cancel my",
    "stop the task",
    "stop that",
    "kill the task",
    "kill that",
];

pub struct TaskCancelHandler;

impl IntentMatcher for TaskCancelHandler {
    fn worker_id(&self) -> &str {
        "task-cancel"
    }

    fn recognize(&self, prompt: &str, _session: &Session) -> Option<String> {
        let n = normalise(prompt);
        if CANCEL_TRIGGERS.iter().any(|t| n.contains(t)) {
            Some(prompt.to_string())
        } else {
            None
        }
    }
}

impl WorkerHandle for TaskCancelHandler {
    fn id(&self) -> &str {
        "task-cancel"
    }

    fn description(&self) -> Option<&str> {
        Some("Cancel a previously-spawned task by name or recency.")
    }

    fn dispatch_hint(&self) -> Option<&str> {
        Some(
            "Use when the user asks to stop or cancel a background task — \
             'cancela la tarea de gemini', 'para esa', 'cancel the analysis'.",
        )
    }

    fn invoke(&self, ctx: &WorkerInvocation<'_>) -> Result<WorkerResponse> {
        let dir = TaskRegistry::default_dir()?;
        let registry = TaskRegistry::load_from_dir(&dir);
        let text = match resolve_task_reference(ctx.prompt, &registry) {
            ResolveResult::None => {
                "No encontré ninguna tarea corriendo que coincida con eso.".to_string()
            }
            ResolveResult::Ambiguous(matches) => {
                let workers: Vec<&str> = matches
                    .iter()
                    .map(|t| t.worker_id.as_str())
                    .collect();
                let mut unique = workers.clone();
                unique.sort();
                unique.dedup();
                if unique.len() <= 1 {
                    format!(
                        "Hay {} tareas que coinciden. Sé más específico para \
                         que no cancele la equivocada.",
                        matches.len()
                    )
                } else {
                    format!(
                        "Hay varias: ¿cuál cancelo, la de {}?",
                        unique.join(" o la de ")
                    )
                }
            }
            ResolveResult::Unique(task) => match cancel_task(&task, &dir) {
                Ok(cancelled) => format!(
                    "Listo, cancelé la tarea de {}.",
                    cancelled.worker_id
                ),
                Err(e) => {
                    // `cancel_task` already produces a
                    // human-readable error ("not running", "no
                    // pid", "signal failed"). Pass it through.
                    format!("No pude cancelarla: {e}")
                }
            },
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
    fn cancel_phrases_match() {
        let h = TaskCancelHandler;
        let session = Session::new();
        for phrase in [
            "cancela esa tarea",
            "cancela la última",
            "cancela la de gemini",
            "para la tarea de claude",
            "detén esa tarea",
            "cancel that task",
            "cancel the analysis",
            "stop the task",
            "kill that",
        ] {
            assert!(
                h.recognize(phrase, &session).is_some(),
                "should recognise: {phrase:?}"
            );
        }
    }

    #[test]
    fn unrelated_phrases_decline() {
        let h = TaskCancelHandler;
        let session = Session::new();
        for phrase in [
            "qué tareas tengo",
            "muéstrame el resultado",
            "qué hora es",
            "limpia las tareas viejas",
        ] {
            assert!(
                h.recognize(phrase, &session).is_none(),
                "should decline: {phrase:?}"
            );
        }
    }

    #[test]
    fn ids_match_across_traits() {
        let h = TaskCancelHandler;
        assert_eq!(IntentMatcher::worker_id(&h), WorkerHandle::id(&h));
    }
}

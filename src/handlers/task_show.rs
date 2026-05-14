//! Voice handler for "muéstrame el resultado de X" /
//! "qué dijo X" — resolves a natural-language reference to a
//! task and reads back its summary (or its running-state if
//! still alive).
//!
//! Part of spec 0012 / E2.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use crate::dispatcher::IntentMatcher;
use crate::session::Session;
use crate::tasks::{
    ResolveResult, TaskRegistry, TaskStatus, humanise_age_spanish, resolve_task_reference,
};
use crate::workers::{WorkerHandle, WorkerInvocation, WorkerResponse};

const SHOW_TRIGGERS: &[&str] = &[
    // Spanish
    "muestrame el resultado",
    "muestrame la tarea",
    "muestrame eso",
    "que dijo",
    "que salio de",
    "como fue",
    "como va",
    "dime el resultado",
    "dame el resultado",
    "lee el resultado",
    "leelo",
    "resultado de",
    // English
    "show me the result",
    "show me what",
    "what did",
    "how did",
    "what's the result",
    "whats the result",
    "read the result",
];

pub struct TaskShowHandler;

impl IntentMatcher for TaskShowHandler {
    fn worker_id(&self) -> &str {
        "task-show"
    }

    fn recognize(&self, prompt: &str, _session: &Session) -> Option<String> {
        let n = normalise(prompt);
        if SHOW_TRIGGERS.iter().any(|t| n.contains(t)) {
            Some(prompt.to_string())
        } else {
            None
        }
    }
}

impl WorkerHandle for TaskShowHandler {
    fn id(&self) -> &str {
        "task-show"
    }

    fn description(&self) -> Option<&str> {
        Some("Read back the result of a previously-spawned task by name or recency.")
    }

    fn dispatch_hint(&self) -> Option<&str> {
        Some(
            "Use when the user asks to hear the result of a task they spawned earlier, \
             e.g. 'muéstrame el resultado del análisis' or 'qué dijo gemini'.",
        )
    }

    fn invoke(&self, ctx: &WorkerInvocation<'_>) -> Result<WorkerResponse> {
        let dir = TaskRegistry::default_dir()?;
        let registry = TaskRegistry::load_from_dir(&dir);
        let text = match resolve_task_reference(ctx.prompt, &registry) {
            ResolveResult::None => {
                "No encontré ninguna tarea que coincida con eso. Si ya pasó \
                 hace tiempo, puede que se haya purgado del registro."
                    .to_string()
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
                        "Tengo {} tareas que coinciden con eso. Sé más específico, \
                         por ejemplo «la más reciente» o «la primera».",
                        matches.len()
                    )
                } else {
                    format!(
                        "Tengo varias que coinciden: ¿quieres la de {}?",
                        unique.join(" o la de ")
                    )
                }
            }
            ResolveResult::Unique(task) => describe_task(&task),
        };
        Ok(WorkerResponse {
            text,
            captured_session_id: None,
        })
    }
}

fn describe_task(task: &crate::tasks::Task) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match task.status {
        TaskStatus::Running => {
            let age = humanise_age_spanish(now.saturating_sub(task.spawn_time));
            format!(
                "{} todavía está corriendo. Lleva {} desde que la lanzaste; \
                 te aviso por notificación cuando termine.",
                task.worker_id, age
            )
        }
        TaskStatus::Completed => match &task.summary {
            Some(s) if !s.is_empty() => format!(
                "{} terminó. Esto fue lo que dijo: {}",
                task.worker_id, s
            ),
            _ => format!(
                "{} terminó sin producir resultado visible.",
                task.worker_id
            ),
        },
        TaskStatus::Failed => {
            let code = task
                .exit_code
                .map(|c| format!(" con código {c}"))
                .unwrap_or_default();
            match &task.summary {
                Some(s) if !s.is_empty() => format!(
                    "{} falló{code}. Esto fue lo último que produjo: {s}",
                    task.worker_id
                ),
                _ => format!("{} falló{code} sin producir output.", task.worker_id),
            }
        }
        TaskStatus::Cancelled => format!("{} fue cancelada antes de terminar.", task.worker_id),
        TaskStatus::Orphaned => format!(
            "{} quedó huérfana cuando el daemon se reinició. \
             No tengo el resultado completo.",
            task.worker_id
        ),
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
    fn show_phrases_match() {
        let h = TaskShowHandler;
        let session = Session::new();
        for phrase in [
            "muéstrame el resultado del análisis",
            "qué dijo gemini",
            "cómo fue el refactor",
            "dime el resultado",
            "lee el resultado de la última tarea",
            "show me the result",
            "what did claude say",
        ] {
            assert!(
                h.recognize(phrase, &session).is_some(),
                "should recognise: {phrase:?}"
            );
        }
    }

    #[test]
    fn unrelated_phrases_decline() {
        let h = TaskShowHandler;
        let session = Session::new();
        for phrase in [
            "qué hora es",
            "olvida todo",
            "abre un spec",
            "qué tareas tengo",
        ] {
            assert!(
                h.recognize(phrase, &session).is_none(),
                "should decline: {phrase:?}"
            );
        }
    }

    /// `describe_task` produces a non-empty Spanish sentence for
    /// each terminal status and the running case. Locking the
    /// shape so a refactor doesn't drop a branch silently.
    #[test]
    fn describe_handles_every_status() {
        use crate::tasks::Task;
        fn make(status: TaskStatus, summary: Option<&str>, exit: Option<i32>) -> Task {
            Task {
                id: "t-test".to_string(),
                thread_id: "t".to_string(),
                worker_id: "gemini".to_string(),
                spawn_time: 0,
                completion_time: Some(0),
                status,
                user_intent: "test".to_string(),
                command: vec![],
                pid: None,
                exit_code: exit,
                summary: summary.map(|s| s.to_string()),
            }
        }
        for status in [
            TaskStatus::Running,
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
            TaskStatus::Orphaned,
        ] {
            let text = describe_task(&make(status, Some("some output"), Some(0)));
            assert!(!text.is_empty(), "status {status:?} produced empty text");
            assert!(text.contains("gemini"));
        }
    }

    #[test]
    fn ids_match_across_traits() {
        let h = TaskShowHandler;
        assert_eq!(IntentMatcher::worker_id(&h), WorkerHandle::id(&h));
    }
}

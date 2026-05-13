//! Spec-driven development support.
//!
//! Three layers live here:
//!
//! 1. [`spec`] — pure data: the `Spec` struct, its frontmatter parser
//!    (hand-rolled, no YAML dep), and slug/filename helpers.
//! 2. [`store`] — filesystem operations: locate `specs/`, list, create
//!    inbox specs, promote / ship / reject, find by id or slug.
//! 3. [`intent`] — recognise voice phrases like "open a spec for X"
//!    so the pipeline can route them away from the agent.
//!
//! The CLI (`jarvis spec ...`) wires everything together in `cli.rs`.
//! Pipeline integration lands as a separate change so we can ship the
//! deterministic side first and iterate on voice UX afterwards.

pub mod intent;
pub mod spec;
pub mod store;

pub use intent::{Intent, recognize};
pub use spec::{Spec, Status};

/// Execute a recognised intent against the filesystem and return a short
/// human-readable summary suitable for TTS. The pipeline calls this
/// before falling through to the agent.
///
/// **No error paths surface to the user as failures** — when the
/// operation can't proceed (no specs/ dir, missing spec, etc.) we
/// return a polite explanation that still tells them what happened.
/// That's deliberate: a voice UI shouting Rust errors feels broken.
pub fn execute(intent: Intent) -> String {
    let specs_dir = match store::find_specs_dir_from_cwd() {
        Ok(p) => p,
        Err(e) => return format!("No encuentro un directorio `specs/`: {e}"),
    };

    match intent {
        Intent::NewSpec { title } => match store::create_inbox(&specs_dir, &title) {
            Ok(s) => format!(
                "Listo, abrí un spec en inbox con el título «{}».",
                s.frontmatter.title
            ),
            Err(e) => format!("No pude crear el spec: {e}"),
        },
        Intent::ListSpecs => match store::list_all(&specs_dir) {
            Ok(all) => summarise(&all),
            Err(e) => format!("No pude leer los specs: {e}"),
        },
        Intent::ShowSpec { query } => match store::find(&specs_dir, &query) {
            Ok(Some(s)) => describe(&s),
            Ok(None) => format!("No encontré ningún spec que coincida con «{query}»."),
            Err(e) => format!("No pude buscar el spec: {e}"),
        },
        Intent::PromoteSpec { query } => match store::find(&specs_dir, &query) {
            Ok(Some(s)) => match store::promote(&specs_dir, &s) {
                Ok(promoted) => format!(
                    "Promovido a active con el id {:04}.",
                    promoted.frontmatter.id.unwrap_or(0)
                ),
                Err(e) => format!("No pude promover el spec: {e}"),
            },
            Ok(None) => format!("No encontré ningún spec con «{query}»."),
            Err(e) => format!("No pude buscar el spec: {e}"),
        },
        Intent::ShipSpec { query } => match store::find(&specs_dir, &query) {
            Ok(Some(s)) => match store::ship(&specs_dir, &s) {
                Ok(shipped) => format!(
                    "Spec {:04} enviado a shipped.",
                    shipped.frontmatter.id.unwrap_or(0)
                ),
                Err(e) => format!("No pude marcar como hecho: {e}"),
            },
            Ok(None) => format!("No encontré ningún spec con «{query}»."),
            Err(e) => format!("No pude buscar el spec: {e}"),
        },
        Intent::RejectSpec { query, reason } => match store::find(&specs_dir, &query) {
            Ok(Some(s)) => {
                let r = if reason.trim().is_empty() {
                    "rechazado por voz".to_string()
                } else {
                    reason
                };
                match store::reject(&specs_dir, &s, &r) {
                    Ok(rejected) => format!(
                        "Rechazado: {}",
                        rejected
                            .path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("?")
                    ),
                    Err(e) => format!("No pude rechazar el spec: {e}"),
                }
            }
            Ok(None) => format!("No encontré ningún spec con «{query}»."),
            Err(e) => format!("No pude buscar el spec: {e}"),
        },
    }
}

fn summarise(all: &[Spec]) -> String {
    let mut inbox = 0usize;
    let mut active = 0usize;
    let mut shipped = 0usize;
    let mut rejected = 0usize;
    for s in all {
        match s.frontmatter.status {
            Some(Status::Inbox) => inbox += 1,
            Some(Status::Active) => active += 1,
            Some(Status::Shipped) => shipped += 1,
            Some(Status::Rejected) => rejected += 1,
            _ => {}
        }
    }
    format!("{inbox} en inbox, {active} activos, {shipped} enviados, {rejected} rechazados.")
}

fn describe(s: &Spec) -> String {
    let id = s
        .frontmatter
        .id
        .map(|n| format!("{n:04}"))
        .unwrap_or_else(|| "inbox".to_string());
    let title = &s.frontmatter.title;
    let open_count = s
        .body
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("- [ ]") || t.starts_with("* [ ]")
        })
        .count();
    let done_count = s
        .body
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("- [x]") || t.starts_with("* [x]")
        })
        .count();
    format!("Spec {id}: {title}. {done_count} criterios cumplidos, {open_count} pendientes.")
}

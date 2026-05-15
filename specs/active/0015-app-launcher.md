---
id: 0015
title: App launcher — voice-driven application launching across OSes
status: active
owner: tadeo
created: 2026-05-14
shipped:
verifying:
related:
  - shipped/0010-orchestrator-a-dispatcher-trait-and-buil.md
---
# App launcher — voice-driven application launching across OSes

## Why

Today if the user says "abre Firefox" or "lanza VS Code", the prompt falls
through every built-in handler in the dispatcher cascade and lands on
Claude. Claude *can* launch apps via tool use, but it's slow (cold-spawn
+ tool reasoning) and indirect — the user wanted Firefox open, not a
conversation about it.

A dedicated built-in handler turns "abre Firefox" into a sub-millisecond
Rust path that calls the OS's native launcher and confirms with a short
TTS. Same shape as `time`, `calc`, `task-list`, etc. — `IntentMatcher`
+ `WorkerHandle` pair that self-registers through
`handlers::register_builtins`. As Jarvis grows to target macOS and BSD
(see project_scope), having one Rust path with OS-aware backends beats
asking the agent to figure out platform differences on every launch.

## What

- [ ] New built-in handler `AppLauncherHandler` matching Spanish + English
      launch triggers: "abre <app>", "lanza <app>", "inicia <app>",
      "open <app>", "launch <app>", "start <app>". The `<app>` tail is
      the part after the trigger; alias-mapping and normalisation
      happen in the handler.
- [ ] OS-aware backend selection. Linux uses `xdg-open` for registered
      `.desktop` entries, falls back to direct execution for raw binary
      names. macOS uses `open -a <AppName>`. Future BSD / Wayland-only
      setups can plug their own variant via the same internal trait.
- [ ] Built-in alias table covering ~20 common desktop apps: "vscode" →
      `code`, "spotify" → `spotify`, "firefox" → `firefox`, etc.
      Aliases match case-insensitive after ASCII-folding normalisation.
- [ ] User-extensible aliases via a new TOML section `[apps.aliases]` so
      the user can teach Jarvis their own vocabulary
      (`signal-desktop = "signal"`, `obsidian-flatpak = "obsidian"`,
      etc.). User aliases override built-ins.
- [ ] Successful launch produces a brief Spanish TTS confirmation:
      "Listo, abrí Firefox." Failure (binary not found, app not
      installed) responds with a useful message rather than the
      generic dispatcher fallthrough.
- [ ] Refusal list: short hard-coded set of names that must never be
      voice-launched (e.g. `rm`, `shutdown`, `reboot`, anything that
      isn't a "user-app" by reasonable interpretation). Policy is
      spec'd in this file before code-time.
- [ ] Tests cover positive matches across ES + EN triggers, alias
      resolution, the unrecognised-app failure path, the refusal path,
      and the OS-detection branch.

## How

- Module `src/handlers/app_launcher.rs`. Same pattern as
  `time_of_day.rs` and `task_list.rs`.
- OS detection at handler construction: `cfg!(target_os = ...)` picks
  the launcher backend. The backend trait is internal to the module
  (no need to expose it broadly).
- The alias table is a compile-time `&[(&str, &str)]` array; user
  aliases load from config at startup and override built-ins.
- Spawn mechanics: `Command::new(<resolved-binary>).spawn()`, drop the
  `Child` immediately (fire-and-forget). The launched app inherits the
  daemon's environment, which is usually what the user wants (their
  `DISPLAY` / `WAYLAND_DISPLAY`).
- Voice errors should be friendly Spanish: "No encuentro Firefox
  instalado" instead of "binary not found".

Out of scope:

- Closing apps by voice — bigger surface (which window? the user's
  last-focused? all instances?). Future spec.
- Window management ("minimize Firefox", "switch workspace"). Window
  managers vary too much for v1.
- Launch arguments ("abre Firefox con un perfil distinto"). v2.

## Journal

- 2026-05-14: opened. User asked for it during the spec 0012 ship
  review. Independent of orchestrator-B but shares the same
  `IntentMatcher` infrastructure spec 0010 established.

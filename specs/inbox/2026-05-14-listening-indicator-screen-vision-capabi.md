---
id:
title: Listening indicator + screen-vision capabilities (think)
status: inbox
owner: unassigned
created: 2026-05-14
shipped:
verifying:
related:
---
# Listening indicator + screen-vision capabilities (think)

> **Vision-level entry, not a finalised spec.** Two related ideas
> captured at the end of a session for future refinement. Treat the
> "What" bullets as brainstorm starting points, not commitments.
> Promote and split into concrete specs when ready to implement.

## Why

Two pain points the user named, both deferred for after the
orchestrator stack lands:

1. **Listening indicator.** Today the daemon is invisible. The user
   can't tell at a glance whether Jarvis is recording, transcribing,
   processing a turn through the agent, or idle. The wake word fires
   silently; the follow-up window is opaque. An always-on visual
   cue — tray icon, status-bar widget, dock badge — would transform
   the UX from "did it hear me?" to "I can see it's listening."

2. **Screen vision.** Voice + Claude can do a lot, but they can't
   answer "what's on this screen right now?". Modern LLMs accept
   images; Jarvis could capture a screenshot (or a specific window)
   and pass it as context to the agent. Use cases: "describe what
   I'm looking at", "summarise this article on screen", "what does
   this error message say?", "translate the dialog box".

The two are independent but both feel orthogonal to the existing
pipeline and deserve their own design discussion.

## What

*Vision-level starting points, to be refined.*

### Listening indicator

- [ ] Always-on visual surface showing the daemon's current state.
      Candidates: tray icon (libappindicator / libayatana-appindicator,
      cross-DE on Linux + native on macOS), status-bar widget for
      waybar / polybar / i3blocks reading via UNIX socket, or a small
      floating native overlay (gtk / egui / wgpu).
- [ ] State surface: idle / listening (recording) / thinking (agent
      invoked) / speaking (TTS playing). Maybe also "task running" if
      there are async tasks from spec 0011.
- [ ] Configurable position + style so it fits the user's desktop
      theme.
- [ ] Cross-DE story: GNOME, KDE, Hyprland, Sway, i3, macOS, ideally
      with one binary.

### Screen vision

- [ ] Orchestrator capability to capture the current screen (or a
      specific window) as an image and feed it into the agent's
      invocation. Voice triggers: "qué hay en pantalla", "describe
      esta ventana", "qué dice este error", etc.
- [ ] Screenshot backends: grim (Wayland), scrot / maim (X11),
      screencapture (macOS). Auto-detect.
- [ ] Window selection: focused window by default; explicit "describe
      this window" picker via wmctrl / hyprctl / yabai when supported.
- [ ] Image handoff to the agent: Claude Code CLI supports image
      inputs via `--image <path>`. Other agents may need a different
      protocol; the manifest can declare image support.
- [ ] Privacy / opt-in: never capture without an explicit voice
      trigger. Document this prominently — users give mic permission;
      screen is a separate sensitivity level.

## How

Both features are large enough that the actual designs should happen
in dedicated specs when promoted. Open questions to noodle on:

- For the indicator, do we ship one mechanism (tray icon via
  libappindicator) and call it good, or a manifest-shaped contract
  where users plug their own bar widget via a small protocol (daemon
  writes state to a UDS, the bar widget reads it)? The latter mirrors
  how the worker registry works and avoids forcing a GUI dependency
  on users who already have a status bar.
- For screen vision, the trickiest piece is privacy signalling. A
  bright "📷 captured" notification each time would protect the user
  but add friction. Whatever shape lands needs to consider this.

## Journal

- 2026-05-14: opened as a vision note. User listed both features as
  "cosas para pensar" after the orchestrator ships. Captured here so
  the ideas don't fall out of memory; refine and promote individually
  when ready.

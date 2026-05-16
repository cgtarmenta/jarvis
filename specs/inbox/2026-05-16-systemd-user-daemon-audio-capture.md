---
id:
title: systemd-user daemon doesn't capture mic input
status: inbox
owner: unassigned
created: 2026-05-16
shipped:
verifying:
related:
  - active/0015-app-launcher.md
  - shipped/0002-pluggable-wake-backends.md
---

# systemd-user daemon doesn't capture mic input

## Why

The packaged path for running Jarvis is `systemctl --user start jarvis`
— a long-lived service that comes up with the desktop session and is
the only sensible way to ship the assistant to anyone who isn't us. On
2026-05-16, while verifying spec 0015, we discovered that the daemon
launched as a systemd-user unit (transient, via `systemd-run --user`)
sees the microphone at the noise floor (`peak_rms=0.0001`) and never
crosses the VAD trigger (`0.0200`). The exact same binary, started
from a shell in the same desktop session, captures fine
(`peak_rms=0.09+`).

The functional symptom is "wake word never fires" — the daemon
silently ignores the user forever. There is no log error, no failed
device-open warning; the audio backend opens *something* (RMS samples
are arriving) but it's evidently not the mic the user is speaking into.

Until this is resolved, the user is forced to run `jarvis daemon` from
a shell to get a working assistant, which defeats the whole point of
shipping a systemd unit. It also blocks any future end-to-end
verification of voice-driven specs (0015 today, future
intent-handling specs tomorrow) because the systemd path is the
realistic deployment shape.

## What

Vision-level entry — the underlying mechanism isn't fully diagnosed
yet, so the acceptance criteria here are deliberately empty until we
understand *why* the daemon picks a wrong/silent source under
systemd-user. First step is a diagnosis pass, not a fix.

Observations we already have (use these as the starting point — don't
re-discover them):

- Daemon inherits `XDG_RUNTIME_DIR=/run/user/1000` and
  `DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus` from the
  systemd-user environment, so the PipeWire socket at
  `$XDG_RUNTIME_DIR/pipewire-0` is reachable in principle.
- `PIPEWIRE_RUNTIME_DIR`, `PULSE_SERVER`, and similar audio-specific
  env vars are **not** set on the daemon process — neither is
  `PULSE_RUNTIME_PATH`. The same env in the shell-launched daemon
  (which works) also does not have these set, so it's not obviously
  an env-passthrough issue.
- No warning or error is logged. The daemon reports `peak_rms` every
  10s, so it *is* reading frames; it's just reading silence.
- Session is Hyprland + Wayland; PipeWire is the audio server.
- The shipped systemd unit (`systemd/jarvis.service`) declares
  `After=pipewire.service pulseaudio.service` but no `Requires=` /
  `BindsTo=` — that's fine for ordering but doesn't change source
  selection.

Hypotheses worth checking, in rough priority order:

- [ ] The recording backend (whatever `[record].backend = "auto"`
      resolves to in `src/recording/`) picks a different default
      source under systemd-user than under shell. Plausible culprits:
      cpal's host selection logic, the absence of `XDG_SESSION_TYPE`
      from the daemon env (we did not check this on 2026-05-16),
      ALSA fallback when PipeWire's PulseAudio shim isn't reachable.
- [ ] PipeWire's *node permission* model gates capture by app id /
      desktop file presence; a systemd-user unit with no
      `.desktop`-derived identity might land in a portal-mediated
      "needs permission" state that returns silence instead of
      failing loudly. Worth checking
      `pw-cli list-objects | grep -i jarvis` while the daemon is
      running.
- [ ] The wrong source is auto-selected because the systemd-user
      environment doesn't get the user's PulseAudio /
      PipeWire-pulse default source preference (which sometimes
      lives in `~/.config/pulse/default-source` or is set by the
      DE's volume applet at session start).

## How

Diagnosis first; no code changes proposed until we know which of the
hypotheses is correct. A reasonable diagnostic pass:

1. Reproduce: `systemd-run --user --unit=jarvis-diag --service-type=simple
   -E RUST_LOG=debug /path/to/target/debug/jarvis daemon`, then
   `journalctl --user -u jarvis-diag -f` and watch which recording
   backend is selected (cpal device name, ALSA pcm, etc.) — the
   shell-launched daemon will pick something different.
2. Inspect PipeWire's view: `pw-cli list-objects | grep -A5 -iE
   "jarvis|node\.name"` to see which node the daemon attached to,
   versus the shell-launched one.
3. Check whether passing `XDG_SESSION_TYPE` and friends fixes it —
   `systemd-run --user -E XDG_SESSION_TYPE=wayland …`.
4. If we land on a "needs explicit source name" fix, the natural
   landing spot is `[record]` in `config.toml`: today it's
   `backend = "auto"` with no `device` field, and PipeWire is happy
   to take a node name through cpal if we expose one.

Out of scope (probably):

- Installing the production systemd unit. The repo's
  `systemd/jarvis.service` is for AUR packaging; the development
  story should keep working with `systemd-run --user` against a
  debug or release binary that lives in the worktree.
- Wayland portal integration in general — only what's necessary to
  reach the mic.

## Journal

- 2026-05-16: opened. Discovered during the manual verification
  pass for spec 0015 — the freshly-fixed handler couldn't be
  exercised end-to-end because the daemon never heard the user
  through the systemd-user unit. Shell-launched daemon works in
  the same session, which let us trust the unit tests and ship
  0015's A+B fixes despite the gap. Diagnosis of the audio
  capture mismatch is its own spec because the surface (cpal
  / PipeWire / portal / systemd env) is unrelated to anything
  in the dispatcher / handler layer.

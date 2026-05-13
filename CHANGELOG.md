# Changelog

All notable changes to Jarvis will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Conversation sessions: persistent JSON-backed history at
  `~/.cache/jarvis/sessions/current.json` so the agent keeps context
  across wake events. New `[session]` config block, `jarvis session
  {show,reset,path}` subcommand, and voice reset phrases ("olvida",
  "new conversation", …).
- Spec-driven development scaffolding under [`specs/`](specs/) with
  `inbox/`, `active/`, `shipped/`, `rejected/` directories, a
  canonical README + template, and three seed specs retro-documenting
  shipped features.

### Added

- Warp `oz` agent (`name = "warp"`): wraps `oz agent run --prompt …`, with
  optional `model`, `profile`, `cwd`, and `api_key` overrides. Auto-detects
  the binary among `oz`, `oz-preview`, and the deprecated `warp-cli`.
  Doctor reports the binary and `WARP_API_KEY` presence.
- `jarvis setup` interactive wizard: detects `$LANG`, proposes a Whisper
  model (with download), filters Piper voices by language and downloads
  the chosen one on first use, walks the user through agent selection +
  API key, and writes the final config to disk. Defaults to **en-GB** for
  English and **es-ES** for Spanish rather than the US variants.
- Doctor now suggests `jarvis setup` when the Whisper model file is
  missing.

## [0.1.0] - 2026-05-13

### Added

- Rust implementation of the orchestrator (single ~4 MB binary, no in-process
  ML, no Python interpreter dependency).
- `jarvis listen` one-shot turn for hotkey-driven use, plus `daemon`, `dev`,
  `doctor`, `config`, `edit-config`, `test-tts`, `test-stt`, `test-agent`
  subcommands.
- Subprocess-based pipeline: recorder (`arecord` / `parecord` / `pw-record`
  / `ffmpeg`), STT (`whisper-cli` from whisper.cpp + arbitrary `command`),
  TTS (`piper` / `espeak-ng` / `command` / `none`).
- AI agent backends: Claude Code, OpenAI, Gemini, and a generic `shell`
  agent for Ollama, Warp, and custom plugins.
- TOML config at `~/.config/jarvis/config.toml` (XDG on Linux/BSD, the
  macOS-native path on Apple platforms via the `directories` crate).
- AUR `PKGBUILD`, `systemd --user` unit, governance docs (`CONTRIBUTING`,
  `CODE_OF_CONDUCT`, `SECURITY`), GitHub Actions for CI and releases.
- `wakeword` Cargo feature placeholder for the future hands-free path.

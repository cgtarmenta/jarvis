# Contributing to Jarvis

Thanks for your interest. This document covers the dev workflow, expectations
for pull requests, and how releases are cut.

## Code of conduct

By participating in this project you agree to abide by the
[Code of Conduct](CODE_OF_CONDUCT.md).

## Project layout

```
src/                Rust source
  main.rs           thin entry point
  lib.rs            module declarations
  cli.rs            clap-based CLI surface
  config.rs         TOML + XDG path handling
  recorder.rs       audio capture via arecord/parecord/ffmpeg
  stt.rs            speech-to-text wrappers (whisper-cli / command)
  tts.rs            text-to-speech wrappers (piper / espeak / command)
  agents/           Claude / OpenAI / Gemini / shell agents
  pipeline.rs       one-shot record → STT → agent → TTS turn
  daemon.rs         signal handling + wake-word loop
  wake.rs           wake-word stub (feature-gated)
tests/              integration tests
config/             bundled example TOML
systemd/            user service unit
packaging/          AUR PKGBUILD
scripts/dev         developer task runner
.github/workflows/  CI + release automation
```

## Quick start

You need Rust 1.85+ (edition 2024). On Arch:

```sh
sudo pacman -S rustup && rustup default stable
```

On macOS:

```sh
brew install rustup-init && rustup-init -y
```

Then:

```sh
git clone https://github.com/tadeoarmenta/jarvis.git
cd jarvis
./scripts/dev install
./scripts/dev test
./scripts/dev doctor
./scripts/dev run -- listen
```

## Style and quality gates

- **Formatting:** `cargo fmt --all` (enforced in CI via
  `cargo fmt --all -- --check`).
- **Lints:** `cargo clippy --all-targets --all-features -- -D warnings` —
  warnings are errors.
- **Tests:** `cargo test --all-features`. Integration tests under `tests/`
  must be deterministic and not touch the user's real config dir.
- **Comments:** explain the *why*, not the *what*. The reader has the code.

## Branching and commits

- Branch off `main`. Use short prefixes: `feat/...`, `fix/...`, `docs/...`.
- Imperative-mood commit messages: "Add foo backend", not "Added a foo
  backend".
- Reference issues with `Closes #42` / `Refs #42`.

## Pull requests

1. Open against `main`.
2. CI must be green (fmt + clippy + tests on Linux & macOS, plus a release
   build sanity check).
3. Fill in the PR template — especially the **Test plan** section.
4. At least one maintainer approval required.

## Adding a new agent

1. Create `src/agents/<name>.rs` with a struct implementing the `Agent`
   trait.
2. Add `mod <name>;` + `pub use <name>::<NameAgent>;` to
   `src/agents/mod.rs` and wire it up in `agents::build`.
3. Document the `[agent]` block in `config/config.example.toml` and update
   the README.
4. Add at least one integration test under `tests/`.

For users who don't want to recompile, the `shell` agent (`name = "shell"`,
`command = ["..."]`) already handles any binary or script that reads the
prompt on stdin and writes the reply on stdout.

## Adding a new STT or TTS backend

1. Implement the `Stt` or `Tts` trait in the relevant module.
2. Add the backend name to the `build` match in `src/stt.rs` /
   `src/tts.rs`.
3. Document the config keys in `config/config.example.toml`.

## Release process

Releases are tag-driven. CI builds Linux x86_64/aarch64 and macOS
x86_64/aarch64 tarballs and uploads them to a GitHub release.

- **Pre-release:** push a tag like `v0.3.0-rc.1`. Release is flagged as
  prerelease.
- **Stable:** push a tag like `v0.3.0`. Marked as the latest release.

To cut a release:

```sh
# 1. bump version
sed -i 's/^version = ".*"/version = "0.3.0"/' Cargo.toml
cargo update -p jarvis-voice
# 2. commit + tag + push
git commit -am "Release v0.3.0"
git tag v0.3.0
git push --follow-tags
```

The release workflow verifies that the tag's numeric portion matches
`Cargo.toml`'s `version` before publishing.

## Reporting issues

Use the GitHub issue templates. For security vulnerabilities, follow the
disclosure process in [SECURITY.md](SECURITY.md) — **do not** open a public
issue.

# Jarvis

A tiny, cross-platform voice-assistant orchestrator. Press a hotkey (or say a
wake word), speak to your computer, and any AI agent CLI you already use —
Claude Code, ChatGPT, Gemini, Ollama, or a script you wrote yourself — gets the
transcript and answers back through your speakers.

Jarvis itself runs no AI models. It is a small Rust binary (~4 MB) that wires
together existing tools:

```
hotkey / wake word
        │
        ▼
   arecord / parecord / ffmpeg          (any recorder on PATH)
        │  WAV
        ▼
   whisper-cli (whisper.cpp)            (or any command you configure)
        │  text
        ▼
   claude / openai / gemini / shell     (an AI agent CLI you already use)
        │  reply
        ▼
   piper-tts / espeak-ng / command      (or none — print-only mode)
        │
        ▼
       speakers
```

## Why

- **Tiny footprint.** A single ~4 MB static binary; no Python, no model files
  bundled, no virtualenvs. Runs all day at idle without you noticing.
- **Cross-Unix from day one.** Linux, macOS, BSD — Jarvis only needs commands
  that already exist on each platform (`arecord` / `parecord` / `ffmpeg` /
  `afplay` / Homebrew binaries).
- **Bring your own brain.** Any CLI agent works. Claude Code, OpenAI, Gemini,
  Ollama, Warp, or a script Jarvis wrote when you said "make me a plugin that
  controls my smart bulbs".
- **One config file.** Editable TOML at `~/.config/jarvis/config.toml`,
  designed so a UI can wrap it later.

## Install

### Arch / CachyOS (AUR)

```sh
yay -S jarvis-voice          # or: paru -S jarvis-voice
```

Optional dependencies (you can mix and match):

```sh
yay -S whisper.cpp piper-tts claude-code
# pacman -S alsa-utils pipewire-pulse ffmpeg espeak-ng    # any one works
```

### macOS (Homebrew)

```sh
brew install rustup-init && rustup-init -y
brew install whisper-cpp piper espeak-ng ffmpeg
cargo install --git https://github.com/tadeoarmenta/jarvis
```

### From source

```sh
git clone https://github.com/tadeoarmenta/jarvis && cd jarvis
./scripts/dev install
./scripts/dev doctor
```

## Usage

```sh
jarvis setup                # interactive first-time wizard (recommended)
jarvis doctor               # check what's installed
jarvis test-agent "hi"      # ping the configured agent (no audio)
jarvis test-tts             # speak a phrase
jarvis test-stt --seconds 4 # record + transcribe
jarvis listen               # one full turn: record → STT → agent → speak
```

### First-time setup

`jarvis setup` walks you through:

1. **Language** — auto-detected from `$LANG`. English defaults to `en-GB`
   and Spanish to `es-ES` (override at any step).
2. **Whisper model** — pick from a curated list (tiny → large-v3), shown
   with size and approximate RAM. Downloaded to
   `~/.local/share/jarvis/whisper/`.
3. **Piper voice** — fetched live from the rhasspy/piper-voices catalog
   and filtered by your language. Downloaded on first use.
4. **AI agent** — Claude / OpenAI / Gemini / Warp / shell, with prompts
   for API keys when relevant.

Re-run `jarvis setup` any time to change those choices.

Bind `jarvis listen` to a global hotkey in your WM:

```ini
# Hyprland
bind = SUPER, J, exec, jarvis listen

# sway
bindsym $mod+j exec jarvis listen

# GNOME / KDE: bind via Settings → Keyboard → Custom Shortcuts
# macOS: Shortcuts.app → "Run Shell Script" → "/usr/local/bin/jarvis listen"
```

For hands-free wake-word mode (`hey jarvis`), set `[wake] enabled = true` in
the config and build with `--features wakeword`. (Wake-word backend is a
feature-gated stub today; see issue tracker for status.)

## Configure

Config lives at `~/.config/jarvis/config.toml` (Linux/BSD) or
`~/Library/Application Support/jarvis/config.toml` (macOS). The bundled
example is dropped on first run; open it with `jarvis edit-config`. Highlights:

```toml
log_level       = "INFO"
speak_responses = true

[record]
backend = "auto"             # parecord / pw-record / arecord / ffmpeg / command

[stt]
backend  = "whisper-cli"     # or "command"
binary   = "whisper-cli"
model    = "/usr/share/whisper.cpp/models/ggml-base.en.bin"
language = "en"

[tts]
backend = "piper"            # piper / espeak / command / none
voice   = "en_US-lessac-medium"

[agent]
name = "claude"              # claude / openai / gemini / shell
```

### Switching agents

```toml
# OpenAI
[agent]
name  = "openai"
model = "gpt-4o-mini"

# Gemini
[agent]
name  = "gemini"
model = "gemini-1.5-flash"

# Warp (oz CLI)
[agent]
name    = "warp"
model   = "claude-3.7-sonnet"   # any model your Warp account exposes
# Expects WARP_API_KEY in env.

# Ollama / any CLI
[agent]
name    = "shell"
command = ["ollama", "run", "llama3"]
```

API keys come from the environment: `OPENAI_API_KEY`, `GEMINI_API_KEY`,
`ANTHROPIC_API_KEY`, `WARP_API_KEY`. The systemd unit passes them through automatically.

## Service

```sh
systemctl --user enable --now jarvis     # wake-word mode (requires [wake].enabled)
systemctl --user status   jarvis
journalctl --user -u jarvis -f
```

Most people don't need the service: bind `jarvis listen` to a hotkey and stop
there.

## Development

```sh
./scripts/dev install   # cargo fetch + build
./scripts/dev test      # fmt --check + clippy + cargo test
./scripts/dev run -- listen
./scripts/dev build     # release build (reports binary size)
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the contributor workflow and
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) for community norms.

## Notify on long-running tasks

Jarvis exposes its TTS pipeline as `jarvis say` so any long-running
task can announce completion by voice without you having to look at
the screen:

```sh
cargo build --release && jarvis say "build finished"
deploy.sh && jarvis say "deploy ok" || jarvis say "deploy failed"
echo "long summary text" | jarvis say -
jarvis say --voice en_GB-alan-medium "tests passed"
```

> **First install Jarvis globally.** Hooks and scripts run from a fresh
> shell that doesn't know about your `target/release/` checkout. From
> the repo root:
>
> ```sh
> cargo install --path . --force        # puts it in ~/.cargo/bin/jarvis
> # or:
> ./scripts/dev install-bin             # same thing, via the helper
> ```

### Hook into Claude Code

Edit `~/.claude/settings.json` to fire `jarvis say` on every
[`Stop` event](https://docs.claude.com/en/docs/claude-code/hooks).
**Always pass `--detach`** — without it the hook blocks for the
duration of the spoken phrase and Claude Code shows "thinking"
until the audio finishes:

```json
{
  "hooks": {
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "jarvis say --detach \"Listo, tarea completada.\"" }
        ]
      }
    ]
  }
}
```

For a dynamic one-sentence summary instead of a fixed phrase, parse
the hook's JSON-on-stdin (Claude Code passes `session_id` there, not
as an env var) and pipe the answer through. Requires `jq`. Three
guards combine here:

1. **`[ -n "$JARVIS_VOICE_TURN" ] && exit 0`** — Jarvis's voice
   pipeline (`jarvis listen` / `jarvis daemon`) sets this env var
   when it invokes `claude --print --resume …`. The hook detects
   it and exits early, so a voice turn doesn't end with Jarvis
   speaking the assistant's reply *and then* speaking a summary
   on top of it. Interactive Claude Code in a terminal doesn't set
   the var, so the summary still fires there.
2. **`claude --bare`** on the inner invocation — `--bare` skips
   hooks, which prevents the inner `claude --print` from firing
   the Stop hook itself, which would call `claude` again, which
   would fire the hook again, until you `kill -9` something.
3. **`jarvis say --detach`** — so the audio playback doesn't
   block the hook (otherwise Claude Code shows "thinking" until
   the spoken phrase finishes).

```json
{
  "type": "command",
  "command": "[ -n \"$JARVIS_VOICE_TURN\" ] && exit 0; jq -r '.session_id' | xargs -I{} claude --bare --print --resume {} 'In one short sentence, what did you just finish?' | jarvis say --detach -"
}
```

If you don't want a `jq` dependency, a fixed phrase is a perfectly
fine first step — and it has no recursion risk to worry about
(though you still want the `JARVIS_VOICE_TURN` guard if you also
use `jarvis listen`, otherwise voice turns end with the fixed
phrase too).

## Specs

We use lightweight spec-driven development: each non-trivial change
gets a short markdown spec in [`specs/`](specs/) capturing intent
before implementation. Browse `specs/shipped/` to see how the project
got here, `specs/active/` for what's being built right now, and
`specs/inbox/` for rough ideas. The format and lifecycle are
documented in [`specs/README.md`](specs/README.md).

## License

MIT — see [LICENSE](LICENSE).

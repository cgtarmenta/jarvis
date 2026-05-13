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
jarvis doctor               # check what's installed
jarvis test-agent "hi"      # ping the configured agent (no audio)
jarvis test-tts             # speak a phrase
jarvis test-stt --seconds 4 # record + transcribe
jarvis listen               # one full turn: record → STT → agent → speak
```

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

# Ollama / any CLI
[agent]
name    = "shell"
command = ["ollama", "run", "llama3"]
```

API keys come from the environment: `OPENAI_API_KEY`, `GEMINI_API_KEY`,
`ANTHROPIC_API_KEY`. The systemd unit passes them through automatically.

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

## License

MIT — see [LICENSE](LICENSE).

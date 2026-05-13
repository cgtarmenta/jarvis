# Security Policy

## Supported versions

Jarvis is in early development; the latest tagged release is the only
supported version. Older versions receive security fixes only at the
maintainer's discretion.

## Reporting a vulnerability

**Please do not open a public GitHub issue for security problems.**

Email **tadeo.armenta@researchwarrant.com** with:

- A description of the vulnerability and its impact
- Steps to reproduce or proof-of-concept code
- Affected versions / commits, if known
- Any suggested mitigation

You should expect an initial reply within 5 business days. We will coordinate
a fix and disclosure timeline with you, aiming to publish a patched release
within 30 days for typical issues.

## Scope

In scope:

- The Jarvis daemon and CLI (`src/jarvis/`)
- The systemd user unit
- The AUR PKGBUILD
- Default configuration shipped with the project

Out of scope:

- Vulnerabilities in third-party agents (Claude Code, OpenAI, Gemini,
  Ollama, etc.) — report those to their respective maintainers.
- Issues caused by user-supplied custom plugins.

## Hall of fame

Reporters who follow this process responsibly will be credited in the release
notes (with their consent) once a fix has shipped.

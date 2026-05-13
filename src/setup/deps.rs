//! Detect missing CLI tools and (optionally) install them.
//!
//! The wizard calls into here right before any step that needs an external
//! binary (`whisper-cli`, `claude`, `oz`, `piper`…). If the binary is on
//! `PATH` we no-op; otherwise we try to translate the request into the
//! right invocation of the user's package manager (`pacman`, `brew`,
//! `apt-get`, `dnf`) and offer to run it.
//!
//! Auto-install needs `sudo` for system package managers. We *show* the
//! exact command before running it so the user knows what they're agreeing
//! to — no opaque installer magic.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::Result;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Select};

/// One installable CLI tool.
pub struct Tool {
    /// Binary we look for on PATH.
    pub binary: &'static str,
    /// Friendly name for messages ("whisper.cpp", "Claude Code CLI").
    pub friendly_name: &'static str,
    /// Package name in `pacman` (official repos only). Use `aur` for AUR-only.
    pub pacman: Option<&'static str>,
    /// Package name in `brew`.
    pub brew: Option<&'static str>,
    /// Package name in `apt-get` (Debian/Ubuntu).
    pub apt: Option<&'static str>,
    /// Package name in `dnf` (Fedora).
    pub dnf: Option<&'static str>,
    /// Package name in an AUR helper (`yay`, `paru`). Pacman main-repo
    /// packages should set `pacman`; AUR-only packages set this.
    pub aur: Option<&'static str>,
    /// PyPI package name for `pipx install`. We use **pipx**, not raw `pip
    /// --user`, because modern distros (Arch, Debian 12+, Ubuntu 23.04+)
    /// follow PEP 668 and refuse `pip --user` outright. pipx creates an
    /// isolated venv per tool and exposes the binary under
    /// `~/.local/bin/<name>`. Set this only for tools that ship to PyPI;
    /// the field is also our signal that the tool is fundamentally Python
    /// (we surface that fact in install prompts).
    pub pipx: Option<&'static str>,
    /// Fallback documentation link.
    pub source_url: &'static str,
}

pub const WHISPER_CLI: Tool = Tool {
    binary: "whisper-cli",
    friendly_name: "whisper.cpp",
    // Not in the official Arch repos as of writing — only the AUR has it.
    pacman: None,
    brew: Some("whisper-cpp"),
    apt: None,
    dnf: None,
    aur: Some("whisper.cpp"),
    pipx: None,
    source_url: "https://github.com/ggerganov/whisper.cpp",
};

pub const PIPER_TTS: Tool = Tool {
    binary: "piper",
    friendly_name: "piper-tts",
    // The official Arch package literally named `piper` is a GTK app for
    // gaming mice. The TTS we want is `piper-tts` in the AUR — BUT it
    // conflicts with the gaming-mice piper for `/usr/bin/piper`. The pip
    // variant installs to `~/.local/bin/piper` and avoids the fight
    // entirely; that's our preferred path when there's a conflict.
    pacman: None,
    brew: Some("piper"),
    apt: None,
    dnf: None,
    aur: Some("piper-tts"),
    pipx: Some("piper-tts"),
    source_url: "https://github.com/OHF-Voice/piper1-gpl",
};

pub const CLAUDE_CODE: Tool = Tool {
    binary: "claude",
    friendly_name: "Claude Code CLI",
    // Ships in CachyOS's repo (pacman finds it) and in AUR for vanilla Arch.
    // The `pacman -Si` probe in `choose_install` falls through to the AUR
    // helper automatically if the active pacman config can't see it.
    pacman: Some("claude-code"),
    brew: None,
    apt: None,
    dnf: None,
    aur: Some("claude-code"),
    pipx: None,
    source_url: "https://docs.claude.com/en/docs/claude-code",
};

pub const WARP_OZ: Tool = Tool {
    binary: "oz",
    friendly_name: "Warp oz CLI",
    pacman: None,
    brew: Some("oz"),
    apt: None,
    dnf: None,
    aur: None,
    pipx: None,
    source_url: "https://docs.warp.dev/developers/cli",
};

/// Result of the dep-check step. Tells the caller whether to proceed
/// confidently or warn that downstream calls will fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepStatus {
    /// Already installed, or installed during the prompt. `binary_path` is
    /// `Some` when the wizard installed to a non-standard location (e.g.
    /// `~/.local/bin` via pip) and the caller should write it back to
    /// config so subsequent runs find the right binary.
    Installed { binary_path: Option<PathBuf> },
    /// User chose to skip; downstream steps will likely fail at runtime.
    Skipped,
}

#[derive(Debug, Clone, Copy)]
enum PkgManager {
    Pacman,
    Brew,
    Apt,
    Dnf,
}

#[derive(Debug, Clone, Copy)]
enum AurHelper {
    Yay,
    Paru,
    Trizen,
}

fn detect_pkg_manager() -> Option<PkgManager> {
    // pacman first because Arch users frequently also have `brew` (linuxbrew)
    // installed, and we want to prefer the native package manager.
    for (binary, mgr) in [
        ("pacman", PkgManager::Pacman),
        ("apt-get", PkgManager::Apt),
        ("dnf", PkgManager::Dnf),
        ("brew", PkgManager::Brew),
    ] {
        if which::which(binary).is_ok() {
            return Some(mgr);
        }
    }
    None
}

fn detect_aur_helper() -> Option<AurHelper> {
    for (binary, helper) in [
        ("yay", AurHelper::Yay),
        ("paru", AurHelper::Paru),
        ("trizen", AurHelper::Trizen),
    ] {
        if which::which(binary).is_ok() {
            return Some(helper);
        }
    }
    None
}

/// Build the exact argv to install `package` via `mgr`. The first element is
/// always the command name; we don't shell-quote because we `exec`.
fn install_cmd(mgr: PkgManager, package: &str) -> Vec<String> {
    match mgr {
        PkgManager::Pacman => vec![
            "sudo".into(),
            "pacman".into(),
            "-S".into(),
            "--needed".into(),
            package.into(),
        ],
        PkgManager::Apt => vec![
            "sudo".into(),
            "apt-get".into(),
            "install".into(),
            "-y".into(),
            package.into(),
        ],
        PkgManager::Dnf => vec![
            "sudo".into(),
            "dnf".into(),
            "install".into(),
            "-y".into(),
            package.into(),
        ],
        // brew never needs sudo.
        PkgManager::Brew => vec!["brew".into(), "install".into(), package.into()],
    }
}

/// Per-tool "is this *actually* the right binary?" probe.
///
/// Returning `Some(issue)` makes `ensure_installed` treat the binary as
/// missing and offer to install the correct package over it.
fn wrong_binary_issue(tool: &Tool) -> Option<String> {
    if tool.binary == "piper" {
        return crate::tts::piper_binary_issue(tool.binary);
    }
    None
}

fn aur_install_cmd(helper: AurHelper, package: &str) -> Vec<String> {
    let bin = match helper {
        AurHelper::Yay => "yay",
        AurHelper::Paru => "paru",
        AurHelper::Trizen => "trizen",
    };
    vec![bin.into(), "-S".into(), "--needed".into(), package.into()]
}

/// Probe whether `pacman` can find this package in any configured repo (core,
/// extra, cachyos-*, chaotic-aur, …). This lets us cleanly fall through to
/// an AUR helper for packages that don't live in the user's pacman repos.
fn pacman_can_find(pkg: &str) -> bool {
    Command::new("pacman")
        .args(["-Si", pkg])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// One actionable install strategy: an argv to run and a human label.
#[derive(Debug, Clone)]
struct InstallStrategy {
    cmd: Vec<String>,
    /// Optional command that must succeed *before* `cmd` is run. Used for
    /// "install pipx (system) first, then run pipx install (user-space)"
    /// combos so the user sees a single menu entry instead of two steps.
    prerequisite_cmd: Option<Vec<String>>,
    label: String,
    /// True when this strategy installs to the user's home (no sudo, no
    /// system-level conflicts). The wizard prefers these when the system
    /// install would force-remove a conflicting package the user might want
    /// to keep (e.g. AUR `piper-tts` evicting the gaming-mice `piper`).
    user_space: bool,
    /// Absolute path the wizard should write back to config after a
    /// successful install. None means "the binary will land on default
    /// PATH"; Some means "use this exact path to avoid PATH collisions".
    explicit_binary_path: Option<PathBuf>,
}

/// Build *all* viable install strategies for `tool`, in display order. We
/// surface multiple choices so the user can opt out of a system-level
/// install that would clobber something else (the gaming-mice piper is the
/// canonical example).
fn enumerate_strategies(tool: &Tool, has_conflict: bool) -> Vec<InstallStrategy> {
    let mut out = Vec::new();

    // 1. User-space pip first when available AND (the binary name collides
    //    with another package, OR the user explicitly asked us not to touch
    //    the system). Pip-user installs to `~/.local/bin/<binary>` so two
    //    binaries with the same name can coexist (we point our config at
    //    the absolute path).
    if let Some(pipx_pkg) = tool.pipx {
        let home_bin = user_local_bin().map(|p| p.join(tool.binary));
        if which::which("pipx").is_ok() {
            // Happy path — pipx is already installed.
            out.push(InstallStrategy {
                cmd: vec![
                    "pipx".into(),
                    "install".into(),
                    "--force".into(),
                    pipx_pkg.into(),
                ],
                prerequisite_cmd: None,
                label: format!(
                    "pipx — installs to ~/.local/bin/{} in an isolated venv \
                     (Python required, but doesn't touch system Python)",
                    tool.binary
                ),
                user_space: true,
                explicit_binary_path: home_bin,
            });
        } else if let Some(prereq) = pipx_prereq_strategy(pipx_pkg, &home_bin, tool.binary) {
            // pipx is missing but the system *can* install it. Offer a
            // combo strategy that installs pipx first, then the tool. We
            // run them as two child processes serially.
            out.push(prereq);
        }
    }

    // 2. Native package manager (pacman/apt/dnf/brew) — probe pacman first
    //    so we don't pretend a package is in repos when it really lives in
    //    AUR for this distro variant.
    if let (Some(mgr), Some(pkg)) = (detect_pkg_manager(), repo_package(tool)) {
        let usable = !matches!(mgr, PkgManager::Pacman) || pacman_can_find(pkg);
        if usable {
            out.push(InstallStrategy {
                cmd: install_cmd(mgr, pkg),
                prerequisite_cmd: None,
                label: format!("{} package: {}", pkg_label(mgr), pkg),
                user_space: matches!(mgr, PkgManager::Brew),
                explicit_binary_path: None,
            });
        }
    }

    // 3. AUR helper (yay / paru / trizen) — last resort because AUR
    //    builds-from-source are slower and more likely to surprise the
    //    user with conflict prompts.
    if let (Some(helper), Some(pkg)) = (detect_aur_helper(), tool.aur) {
        out.push(InstallStrategy {
            cmd: aur_install_cmd(helper, pkg),
            prerequisite_cmd: None,
            label: format!("AUR via {}: {}", aur_label(helper), pkg),
            user_space: false,
            explicit_binary_path: None,
        });
    }

    // If there's a binary conflict, push user_space strategies up front
    // (stable sort to preserve order otherwise).
    if has_conflict {
        out.sort_by_key(|s| !s.user_space);
    }
    out
}

// `detect_pip` was removed in favour of detecting `pipx` directly inline.
// Plain `pip --user` would fail on PEP 668 systems (Arch, Debian 12+,
// Ubuntu 23.04+). pipx is the modern equivalent and is what we offer.

/// Build a combo strategy that **installs pipx via the system package
/// manager first**, then `pipx install <pipx_pkg>`. Returns `None` if we
/// don't know how to install pipx on this OS.
///
/// The pipx package name varies across distros: `python-pipx` on Arch,
/// `pipx` on Debian/Ubuntu (recent) and Fedora and brew.
fn pipx_prereq_strategy(
    pipx_pkg: &'static str,
    home_bin: &Option<PathBuf>,
    binary_name: &str,
) -> Option<InstallStrategy> {
    let mgr = detect_pkg_manager()?;
    let pipx_pkg_name = match mgr {
        PkgManager::Pacman => "python-pipx",
        PkgManager::Apt | PkgManager::Dnf | PkgManager::Brew => "pipx",
    };
    if matches!(mgr, PkgManager::Pacman) && !pacman_can_find(pipx_pkg_name) {
        return None;
    }
    Some(InstallStrategy {
        cmd: vec![
            "pipx".into(),
            "install".into(),
            "--force".into(),
            pipx_pkg.into(),
        ],
        prerequisite_cmd: Some(install_cmd(mgr, pipx_pkg_name)),
        label: format!(
            "{} + pipx — installs pipx via the system, then pipx-installs \
             to ~/.local/bin/{} (Python required)",
            pkg_label(mgr),
            binary_name
        ),
        user_space: true,
        explicit_binary_path: home_bin.clone(),
    })
}

fn user_local_bin() -> Option<PathBuf> {
    directories::UserDirs::new().map(|d| d.home_dir().join(".local").join("bin"))
}

fn repo_package(tool: &Tool) -> Option<&'static str> {
    match detect_pkg_manager()? {
        PkgManager::Pacman => tool.pacman,
        PkgManager::Brew => tool.brew,
        PkgManager::Apt => tool.apt,
        PkgManager::Dnf => tool.dnf,
    }
}

fn pkg_label(mgr: PkgManager) -> &'static str {
    match mgr {
        PkgManager::Pacman => "pacman",
        PkgManager::Brew => "brew",
        PkgManager::Apt => "apt-get",
        PkgManager::Dnf => "dnf",
    }
}

fn aur_label(helper: AurHelper) -> &'static str {
    match helper {
        AurHelper::Yay => "yay",
        AurHelper::Paru => "paru",
        AurHelper::Trizen => "trizen",
    }
}

/// Make sure `tool.binary` is on PATH **and** is the right binary. Some
/// tool names collide with unrelated apps (piper-tts vs the gaming-mice
/// app named "piper"); we run a per-tool sanity probe before claiming
/// success.
pub fn ensure_installed(theme: &ColorfulTheme, tool: &Tool) -> Result<DepStatus> {
    let has_conflict = if which::which(tool.binary).is_ok() {
        if let Some(issue) = wrong_binary_issue(tool) {
            println!();
            println!("⚠ Found `{}` on PATH but {issue}.", tool.binary);
            true
        } else {
            return Ok(DepStatus::Installed { binary_path: None });
        }
    } else {
        println!();
        println!(
            "✗ {} is not installed ({} not on PATH).",
            tool.friendly_name, tool.binary
        );
        false
    };

    let strategies = enumerate_strategies(tool, has_conflict);
    if strategies.is_empty() {
        println!("  No automatic install path for this OS / package manager.");
        println!("  Install manually from: {}", tool.source_url);
        let _ = Confirm::with_theme(theme)
            .with_prompt(format!(
                "Continue setup without {}? (you can install it later)",
                tool.friendly_name
            ))
            .default(true)
            .interact()?;
        return Ok(DepStatus::Skipped);
    }

    prompt_install(theme, tool, &strategies)
}

fn prompt_install(
    theme: &ColorfulTheme,
    tool: &Tool,
    strategies: &[InstallStrategy],
) -> Result<DepStatus> {
    // Show all installable strategies followed by the always-available
    // "I'll do it myself" / "Skip" escape hatches. We render the argv up
    // front so the user knows exactly what's about to run.
    let mut menu: Vec<String> = strategies
        .iter()
        .map(|s| {
            let mut lines = format!("{}\n      $ {}", s.label, s.cmd.join(" "));
            if let Some(pre) = &s.prerequisite_cmd {
                lines = format!("{}\n      (first: $ {})", lines, pre.join(" "));
            }
            lines
        })
        .collect();
    menu.push("I'll run it myself in another terminal — wait for me".into());
    menu.push("Skip — I'll deal with it later".into());

    let idx = Select::with_theme(theme)
        .with_prompt(format!(
            "How do you want to install {}?",
            tool.friendly_name
        ))
        .items(&menu)
        .default(0)
        .interact()?;

    if idx < strategies.len() {
        run_install(tool, &strategies[idx])
    } else if idx == strategies.len() {
        // "I'll do it myself" — show all the command options so they pick.
        wait_for_install(theme, tool, strategies)
    } else {
        Ok(DepStatus::Skipped)
    }
}

fn run_install(tool: &Tool, strategy: &InstallStrategy) -> Result<DepStatus> {
    // Run the prerequisite first (e.g. install pipx via the system) — if
    // it fails the main step is skipped.
    if let Some(pre) = &strategy.prerequisite_cmd {
        println!("Running prerequisite: {}", pre.join(" "));
        let pre_status = Command::new(&pre[0]).args(&pre[1..]).status()?;
        if !pre_status.success() {
            println!("⚠ prerequisite failed with {pre_status} — skipping main step.");
            return Ok(DepStatus::Skipped);
        }
    }

    println!("Running: {}", strategy.cmd.join(" "));
    // Don't capture stdout/stderr — sudo password prompts, pacman progress
    // bars, and pip download bars all need the real TTY.
    let status = Command::new(&strategy.cmd[0])
        .args(&strategy.cmd[1..])
        .status()?;
    if !status.success() {
        println!("⚠ install command exited with {status}. You may need to retry.");
    }

    // Verify the binary is reachable post-install. For user-space installs
    // we prefer the explicit path because $PATH may not include
    // `~/.local/bin` in this shell yet, even though the binary is there.
    let resolved = if let Some(explicit) = &strategy.explicit_binary_path {
        if explicit.is_file() {
            Some(explicit.clone())
        } else {
            None
        }
    } else if which::which(tool.binary).is_ok() {
        which::which(tool.binary).ok()
    } else {
        None
    };

    if let Some(path) = resolved {
        println!("✓ {} installed at {}", tool.binary, path.display());
        // For user-space installs, write the absolute path back so the rest
        // of jarvis doesn't depend on PATH ordering. For system installs,
        // we return None and let the caller stick with the default name.
        let binary_path = if strategy.user_space {
            Some(path)
        } else {
            None
        };
        Ok(DepStatus::Installed { binary_path })
    } else {
        println!(
            "⚠ install completed but {} is still not where we expected. \
             Check the output above.",
            tool.binary
        );
        Ok(DepStatus::Skipped)
    }
}

fn wait_for_install(
    theme: &ColorfulTheme,
    tool: &Tool,
    strategies: &[InstallStrategy],
) -> Result<DepStatus> {
    println!();
    println!("Pick one and run it in another terminal:");
    for s in strategies {
        println!("    $ {}", s.cmd.join(" "));
    }
    println!();
    loop {
        let ready = Confirm::with_theme(theme)
            .with_prompt("Done? Check PATH now")
            .default(true)
            .interact()?;
        if !ready {
            return Ok(DepStatus::Skipped);
        }
        // After a manual install we don't know which strategy the user
        // picked, so we re-probe the standard PATH first, then any
        // explicit user-space paths we know about.
        if which::which(tool.binary).is_ok() && wrong_binary_issue(tool).is_none() {
            println!("✓ found {} on PATH.", tool.binary);
            return Ok(DepStatus::Installed { binary_path: None });
        }
        for s in strategies {
            if let Some(p) = &s.explicit_binary_path
                && p.is_file()
            {
                println!("✓ found {} at {}.", tool.binary, p.display());
                return Ok(DepStatus::Installed {
                    binary_path: if s.user_space { Some(p.clone()) } else { None },
                });
            }
        }
        println!(
            "⚠ {} still not found. Try again, or pick \"No\" to skip.",
            tool.binary
        );
    }
}

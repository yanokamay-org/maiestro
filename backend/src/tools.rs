//! Resolving the external CLIs mAIestro Code invokes **directly** — the agent CLIs
//! `claude`, `codex` and `agy` (Antigravity), `git`, and the VS Code `code` CLI — robustly, even when the app is launched from the
//! packaged bundle (`/Applications/mAIestro Code.app/…`).
//!
//! The problem: at login, macOS Launch Services starts the app with a **minimal
//! `$PATH`** (`/usr/bin:/bin:/usr/sbin:/sbin`) and no shell profile sourced. A
//! bare `Command::new("claude")` (or `git`, or `code`) then fails to resolve, or
//! resolves to the wrong binary (e.g. the `/usr/bin/git` Xcode stub instead of a
//! Homebrew git). This module centralizes resolution so every direct invocation
//! shares one, correct answer. Tools mAIestro Code launches *indirectly* — `open`,
//! `osascript`, `lsof` (system binaries always on the minimal PATH), and the
//! user-facing session, which inherits the full ambient env — are not affected
//! and don't go through here.
//!
//! Two layers:
//! 1. **Login-shell PATH, recovered once.** We run `$SHELL -l -c 'echo $PATH'`
//!    at startup ([`init`]) and cache it. This is the user's *real* PATH —
//!    Homebrew, asdf/nvm shims, etc. — that Launch Services stripped. Every
//!    directly-spawned child is then given this enriched PATH, so tools the
//!    resolved binary itself shells out to (git's helpers, claude's node) resolve
//!    too. This layer fixes all three tools — and any added later — with no config.
//! 2. **Per-tool explicit override.** `~/.maiestro/settings.json`'s `tool_paths`
//!    can pin an absolute path per tool. An override is **authoritative**: once
//!    set it is the *only* source, so a pinned path can never silently resolve to
//!    a different binary — a missing pin is a hard failure, not a fall-through.
//!
//! [`resolve_tool`] precedence: an explicit override wins outright (invoked
//! verbatim, so a broken pin fails loudly); only when **no** override is set do we
//! auto-resolve — `which` on the enriched PATH → known install locations → the
//! bare name (let the OS try, as a last resort preserving prior behavior).

use std::path::PathBuf;
use std::sync::OnceLock;

/// The user's login-shell PATH, resolved once and cached. `None` when the login
/// shell couldn't be run or produced nothing usable (we then fall back to the
/// process's own PATH plus the known-location probes).
static LOGIN_PATH: OnceLock<Option<String>> = OnceLock::new();

/// Resolve and cache the login-shell PATH eagerly. Called from `setup()` so the
/// (blocking) shell invocation happens once at startup rather than lazily on the
/// first spawn. Safe to call more than once — subsequent calls are no-ops.
pub fn init() {
    let _ = login_path();
}

fn login_path() -> &'static Option<String> {
    LOGIN_PATH.get_or_init(resolve_login_path)
}

/// How long to wait for the login-shell PATH probe before giving up. A shell
/// profile that prompts (or otherwise hangs) must not block startup with no tray
/// icon — bail and fall back to the process PATH + known locations instead.
const LOGIN_PATH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Run the user's login shell to capture the PATH it would set up. Mirrors how
/// `run_post_spawn_commands` invokes the shell (`$SHELL -l -c …`) so the two see
/// the same environment. Bounded by [`LOGIN_PATH_TIMEOUT`]: a hanging/prompting
/// profile is killed and treated as "couldn't resolve" rather than freezing the
/// app before the tray even appears (#101).
fn resolve_login_path() -> Option<String> {
    // Tests must never run the developer's login shell: sourcing their profile
    // can prompt, authenticate, or hang. Resolution then uses the process PATH
    // plus the known-location probes, which is all the tests need.
    if cfg!(test) {
        return None;
    }
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Instant;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut child = std::process::Command::new(&shell)
        .args(["-l", "-c", "echo $PATH"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    // Poll for exit rather than `output()` (which would block unbounded). `echo
    // $PATH` writes far less than a pipe buffer, so the child never blocks on the
    // unread stdout while we wait — safe to read it only after it exits.
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() >= LOGIN_PATH_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    tracing::warn!(shell = %shell, "login shell timed out while resolving PATH");
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(e) => {
                tracing::warn!(shell = %shell, error = %e, "failed to poll login shell while resolving PATH");
                return None;
            }
        }
    };
    if !status.success() {
        tracing::warn!(shell = %shell, "login shell exited non-zero while resolving PATH");
        return None;
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let path = out.trim().to_string();
    if path.is_empty() {
        None
    } else {
        tracing::debug!(path = %path, "resolved login-shell PATH");
        Some(path)
    }
}

/// The PATH to hand directly-spawned children: the login-shell PATH followed by
/// the process's own PATH entries not already present. Pure/order-preserving
/// merge in [`merge_paths`] so it's unit-testable.
pub fn enriched_path() -> String {
    let login = login_path().clone().unwrap_or_default();
    let current = std::env::var("PATH").unwrap_or_default();
    merge_paths(&login, &current)
}

/// Concatenate two `:`-separated PATH strings, `first` then `second`, dropping
/// empty and duplicate entries while preserving first-seen order.
fn merge_paths(first: &str, second: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut parts: Vec<&str> = Vec::new();
    for p in first.split(':').chain(second.split(':')) {
        if p.is_empty() {
            continue;
        }
        if seen.insert(p) {
            parts.push(p);
        }
    }
    parts.join(":")
}

/// First existing `bin` found in the given `:`-separated PATH.
fn which_on(path: &str, bin: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

use crate::paths::{expand_tilde, home};

/// Known install locations to probe when a tool isn't on the enriched PATH — the
/// PATH can still be minimal even after enrichment (e.g. the login shell itself
/// failed to resolve). Kept per-tool; unknown tools have no fallbacks.
fn fallbacks(name: &str) -> Vec<PathBuf> {
    match name {
        "claude" => vec![
            home().join(".claude/local/claude"),
            // `pip install --user` and Claude Code's native installer both land
            // here, and a fresh account's login shell often lacks it on PATH.
            home().join(".local/bin/claude"),
            PathBuf::from("/opt/homebrew/bin/claude"),
            PathBuf::from("/usr/local/bin/claude"),
        ],
        "codex" => vec![
            PathBuf::from("/opt/homebrew/bin/codex"),
            PathBuf::from("/usr/local/bin/codex"),
            // Codex's standalone installer links its binary here.
            home().join(".local/bin/codex"),
        ],
        "agy" => vec![
            // Antigravity's install script (`antigravity.google/cli/install.sh`)
            // puts the binary here.
            home().join(".local/bin/agy"),
            // The `antigravity-cli` Homebrew cask.
            PathBuf::from("/opt/homebrew/bin/agy"),
            PathBuf::from("/usr/local/bin/agy"),
        ],
        "code" => vec![
            PathBuf::from("/opt/homebrew/bin/code"),
            PathBuf::from("/usr/local/bin/code"),
            PathBuf::from("/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"),
        ],
        _ => Vec::new(),
    }
}

/// Resolve `name` to a concrete, existing binary, or `None` if nothing was found.
///
/// An explicit override is **authoritative**: once configured it is the *only*
/// source — we never fall back to PATH or known locations, so a pinned path can
/// never silently resolve to a *different* binary. A configured-but-missing
/// override therefore returns `None` (a hard "not found" the health check
/// surfaces), it does **not** fall through to auto-resolution. Only when no
/// override is set do we auto-resolve: `which` on the enriched PATH → known
/// locations.
pub fn find_tool(name: &str) -> Option<PathBuf> {
    // 1. Explicit override wins outright when set — existence decides Some/None,
    //    with no fallback either way.
    if let Some(over) = crate::app_settings::tool_path_override(name) {
        let p = expand_tilde(&over);
        return p.is_file().then_some(p);
    }
    // 2. No override: the enriched (login-shell) PATH.
    if let Some(p) = which_on(&enriched_path(), name) {
        return Some(p);
    }
    // 3. Then known install locations.
    fallbacks(name).into_iter().find(|p| p.is_file())
}

/// The configured `tool_paths` override for `name` when it's set but does *not*
/// point at a real file. Returns the offending path so the health check can name
/// it in a "configured path not found" failure; `None` when there's no override
/// or it exists. (With the authoritative-override rule in [`find_tool`], this is
/// exactly the case where `find_tool` returns `None` despite a pin being set.)
pub fn stale_override(name: &str) -> Option<String> {
    let over = crate::app_settings::tool_path_override(name)?;
    if expand_tilde(&over).is_file() {
        None
    } else {
        Some(over)
    }
}

/// The path to invoke `name` with. A configured override is invoked **verbatim**,
/// even if it doesn't exist, so a broken pin fails loudly (`No such file`) rather
/// than silently running a different binary off PATH. Without an override, falls
/// back to the bare name (letting the OS resolve it) when nothing concrete was
/// found, preserving prior behavior as a last resort.
pub fn resolve_tool(name: &str) -> PathBuf {
    if let Some(over) = crate::app_settings::tool_path_override(name) {
        return expand_tilde(&over);
    }
    find_tool(name).unwrap_or_else(|| PathBuf::from(name))
}

/// For the Settings UI: the resolved path and whether it points at a real file.
/// `(name, false)` means "not found" — nothing on the PATH or in known locations.
pub fn resolved_status(name: &str) -> (String, bool) {
    match find_tool(name) {
        Some(p) => (p.display().to_string(), true),
        None => (name.to_string(), false),
    }
}

/// A `std::process::Command` for a directly-invoked tool: the resolved binary
/// with the enriched PATH so any sub-tools it calls resolve too.
pub fn command(name: &str) -> std::process::Command {
    let mut c = std::process::Command::new(resolve_tool(name));
    c.env("PATH", enriched_path());
    c
}

/// The `tokio::process::Command` equivalent of [`command`], for async spawns.
pub fn tokio_command(name: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(resolve_tool(name));
    c.env("PATH", enriched_path());
    c
}

/// Spawn a fire-and-forget child (e.g. `open`, `code`) and reap it in a detached
/// thread. mAIestro Code is a weeks-running menu-bar process, so a child that's spawned
/// and never `wait()`ed leaves a zombie for the life of the app; each launched
/// URL/editor/Finder-reveal would accumulate one. The detached `wait` collects the
/// exit status without blocking the caller — the child's actual work (opening the
/// URL, launching VS Code) is unaffected. Errors spawning propagate; the reaping
/// itself is best-effort.
pub fn spawn_reaped(cmd: &mut std::process::Command) -> std::io::Result<()> {
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// A short, trimmed preview of some (possibly large) subprocess output for a
/// diagnostic error/log line: whitespace-trimmed, capped at 240 chars, with a
/// stand-in for empty output. Shared by every caller that surfaces `claude`/`git`
/// stdout/stderr (drafting, spawn, health) so the cap and placeholder are uniform.
pub fn snippet(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return "<empty>".into();
    }
    s.chars().take(240).collect()
}

/// Single-quote a string for safe inclusion in a POSIX shell command (VS Code
/// task `command`s and the Claude Code hook commands both run through a shell).
/// Wraps in single quotes and escapes any embedded single quote as `'\''`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The directly-invoked tools whose resolution the Settings UI surfaces.
/// Every agent is listed so any can be pinned; nothing *resolves* an agent's
/// binary for real work unless a repo actually uses that agent.
const TOOLS: &[&str] = &["claude", "codex", "agy", "git", "code"];

/// One tool's resolution result, for the Settings "Tool paths" status line.
#[derive(serde::Serialize)]
pub struct ResolvedTool {
    pub tool: String,
    /// The path we'd invoke — an absolute resolved path, or the bare name if not found.
    pub path: String,
    /// Whether `path` points at a real, existing file.
    pub exists: bool,
}

/// Report, per directly-invoked tool, the path resolution currently picks and
/// whether it exists. Drives the Settings ▸ Preferences ▸ Tool paths status line.
#[tauri::command]
pub fn tools_resolved() -> Vec<ResolvedTool> {
    crate::log_invoke_debug!("tools_resolved");
    TOOLS
        .iter()
        .map(|t| {
            let (path, exists) = resolved_status(t);
            ResolvedTool { tool: (*t).to_string(), path, exists }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_paths_dedups_and_preserves_order() {
        assert_eq!(merge_paths("/a:/b", "/b:/c"), "/a:/b:/c");
        assert_eq!(merge_paths("/a:/a", ""), "/a");
        assert_eq!(merge_paths("", "/x:/y"), "/x:/y");
        // Empty segments (leading/trailing/doubled colons) are dropped.
        assert_eq!(merge_paths("/a::/b:", ":/c"), "/a:/b:/c");
    }

    #[test]
    fn merge_paths_is_idempotent() {
        let once = merge_paths("/opt/homebrew/bin:/usr/bin", "/usr/bin:/bin");
        let twice = merge_paths(&once, &once);
        assert_eq!(once, twice);
    }

    #[test]
    fn which_on_finds_a_known_system_binary() {
        // `sh` exists in /bin on every macOS/Linux host the tests run on.
        let found = which_on("/nonexistent:/bin:/usr/bin", "sh");
        assert!(found.is_some(), "expected to find sh on PATH");
        assert!(found.unwrap().is_file());
    }

    #[test]
    fn which_on_misses_a_nonexistent_binary() {
        assert!(which_on("/bin:/usr/bin", "definitely-not-a-real-binary-xyz").is_none());
    }

    #[test]
    fn snippet_trims_caps_and_marks_empty() {
        assert_eq!(snippet("  hello  "), "hello");
        assert_eq!(snippet("   "), "<empty>");
        assert_eq!(snippet(""), "<empty>");
        assert_eq!(snippet(&"x".repeat(300)).chars().count(), 240);
    }

    #[test]
    fn shell_quote_wraps_and_escapes_single_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("a b"), "'a b'");
        // An embedded single quote is closed, escaped, and reopened.
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn claude_fallbacks_probe_the_user_local_bin() {
        // `~/.local/bin` is where `pip install --user` and the native installer
        // put `claude`; a fresh account's login shell may not have it on PATH.
        assert!(fallbacks("claude").contains(&home().join(".local/bin/claude")));
    }

    #[test]
    fn codex_fallbacks_probe_homebrew_and_the_user_local_bin() {
        let f = fallbacks("codex");
        assert!(f.contains(&PathBuf::from("/opt/homebrew/bin/codex")));
        assert!(f.contains(&PathBuf::from("/usr/local/bin/codex")));
        assert!(f.contains(&home().join(".local/bin/codex")));
    }

    #[test]
    fn agy_fallbacks_probe_the_install_script_and_homebrew() {
        let f = fallbacks("agy");
        assert!(f.contains(&home().join(".local/bin/agy")));
        assert!(f.contains(&PathBuf::from("/opt/homebrew/bin/agy")));
    }

    #[test]
    fn tools_list_includes_every_agent() {
        for agent in [crate::agent::Agent::Claude, crate::agent::Agent::Codex, crate::agent::Agent::Antigravity] {
            assert!(TOOLS.contains(&agent.tool()), "{agent}");
        }
    }

    #[test]
    fn resolve_tool_falls_back_to_bare_name_when_unresolvable() {
        // A tool with no fallbacks that won't be on any PATH resolves to itself.
        let p = resolve_tool("definitely-not-a-real-binary-xyz");
        assert_eq!(p, PathBuf::from("definitely-not-a-real-binary-xyz"));
    }
}

//! Resolving the external CLIs mAIestro Code invokes **directly** — the agent CLIs
//! `claude`, `codex`, `agy` (Antigravity) and `copilot` (GitHub Copilot), `git`, the VS Code `code` CLI, the `cmux` CLI and Windows Terminal's `wt` — robustly, even when the app is launched from the
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

use std::path::{Path, PathBuf};
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
    //
    // Windows has no login-shell gap to fill: a GUI app inherits the user and
    // machine PATH from the registry, not from a shell profile, and there is
    // no `$SHELL` to run.
    if cfg!(test) || cfg!(target_os = "windows") {
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

/// Concatenate two PATH strings (`:`-separated, `;` on Windows), `first` then
/// `second`, dropping empty and duplicate entries while preserving first-seen
/// order. If the merged list can't be re-joined (Windows rejects an entry
/// containing `"`), the process PATH `second` is returned unchanged.
fn merge_paths(first: &str, second: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let parts: Vec<PathBuf> = std::env::split_paths(first)
        .chain(std::env::split_paths(second))
        .filter(|p| !p.as_os_str().is_empty() && seen.insert(p.clone()))
        .collect();
    match std::env::join_paths(parts) {
        Ok(joined) => joined.to_string_lossy().into_owned(),
        Err(_) => second.to_string(),
    }
}

/// The files that running `path` could mean. On Windows a name without an
/// extension (`claude`, `code`) is only runnable as `name` + one of `%PATHEXT%`'s
/// extensions (`claude.exe`, `code.cmd`), tried in that order; a bare
/// extensionless file there (VS Code ships a `code` shell script for WSL next
/// to `code.cmd`) is not a Windows executable. Elsewhere it's just `path`.
fn exe_candidates(path: PathBuf) -> Vec<PathBuf> {
    if cfg!(target_os = "windows") && path.extension().is_none() {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        pathext
            .split(';')
            .filter(|ext| !ext.is_empty())
            .map(|ext| {
                let mut p = path.clone().into_os_string();
                p.push(ext.to_ascii_lowercase());
                PathBuf::from(p)
            })
            .collect()
    } else {
        vec![path]
    }
}

/// First existing `bin` found in the given PATH (see [`exe_candidates`] for
/// Windows extensions), skipping any candidate that is a stand-in rather than
/// the tool itself (see [`is_auto_resolvable`]).
fn which_on(path: &str, bin: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .flat_map(|d| exe_candidates(d.join(bin)))
        .find(|p| p.is_file() && is_auto_resolvable(p))
}

/// The path fragment of the `copilot` shim VS Code's Copilot Chat extension puts
/// on PATH (`…/globalStorage/github.copilot-chat/copilotCli/copilot`). With the
/// real CLI installed it passes calls through; without it, it asks "Install
/// GitHub Copilot CLI? [y/N]" and waits, which would hang a headless call.
const COPILOT_SHIM_MARKER: &str = "github.copilot-chat/copilotCli/";

/// Whether `p` is VS Code's Copilot Chat `copilot` shim rather than the real CLI.
pub fn is_copilot_shim(p: &std::path::Path) -> bool {
    p.to_string_lossy().replace('\\', "/").contains(COPILOT_SHIM_MARKER)
}

/// Whether auto-resolution may pick `p`. It never picks VS Code's `copilot`
/// shim, so with only the shim installed `copilot` counts as not found and we
/// never trigger its interactive installer. A `tool_paths` pin bypasses this:
/// it stays authoritative even when it points at the shim.
fn is_auto_resolvable(p: &std::path::Path) -> bool {
    !is_copilot_shim(p)
}

/// The VS Code Copilot Chat shim on the enriched PATH, if there is one. Lets the
/// health check say "only VS Code's shim was found" when `copilot` resolves to
/// nothing.
pub fn copilot_shim_on_path() -> Option<PathBuf> {
    copilot_shim_on(&enriched_path())
}

fn copilot_shim_on(path: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .flat_map(|d| exe_candidates(d.join("copilot")))
        .find(|p| p.is_file() && is_copilot_shim(p))
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
        ]
        .into_iter()
        // Its Windows installer, which only appends this to the user `Path`.
        .chain(windows_local_fallback(r"Programs\OpenAI\Codex\bin\codex"))
        .collect(),
        "agy" => vec![
            // Antigravity's install script (`antigravity.google/cli/install.sh`)
            // puts the binary here.
            home().join(".local/bin/agy"),
            // The `antigravity-cli` Homebrew cask.
            PathBuf::from("/opt/homebrew/bin/agy"),
            PathBuf::from("/usr/local/bin/agy"),
        ]
        .into_iter()
        // Its Windows install script, which only appends this to the user `Path`.
        .chain(windows_local_fallback(r"agy\bin\agy"))
        .collect(),
        "copilot" => vec![
            // `brew install copilot-cli`, and `npm install -g @github/copilot`
            // when npm's prefix is Homebrew (the npm entry point is a Node
            // script, run with the enriched PATH so it finds `node`).
            PathBuf::from("/opt/homebrew/bin/copilot"),
            PathBuf::from("/usr/local/bin/copilot"),
            // The `gh.io/copilot-install` script.
            home().join(".local/bin/copilot"),
            // A user-level npm prefix.
            home().join(".npm-global/bin/copilot"),
        ],
        "code" if cfg!(target_os = "windows") => windows_code_fallbacks(),
        "code" => vec![
            PathBuf::from("/opt/homebrew/bin/code"),
            PathBuf::from("/usr/local/bin/code"),
            PathBuf::from("/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"),
        ],
        // The CLI bundled inside cmux.app; cmux installs nothing on PATH itself.
        "cmux" => vec![
            PathBuf::from("/Applications/cmux.app/Contents/Resources/bin/cmux"),
            home().join("Applications/cmux.app/Contents/Resources/bin/cmux"),
        ],
        // Windows Terminal's app execution alias. It is a reparse point, but
        // `is_file()` is true for it and `Command` spawns it as is.
        "wt" => windows_local_fallback(r"Microsoft\WindowsApps\wt").into_iter().collect(),
        _ => Vec::new(),
    }
}

/// `rel` under `%LOCALAPPDATA%`, where Windows installers put a per-user CLI;
/// nothing elsewhere. Extensionless, so [`exe_candidates`] turns it into `.exe`.
fn windows_local_fallback(rel: &str) -> Option<PathBuf> {
    if !cfg!(target_os = "windows") {
        return None;
    }
    dirs::data_local_dir().map(|local| local.join(rel))
}

/// The `code` CLI inside VS Code's two default Windows installs — the per-user
/// installer (`%LOCALAPPDATA%\Programs`) and the system one (`%ProgramFiles%`).
/// The counterpart of the macOS app-bundle entry: it finds the CLI when the
/// installer's "Add to PATH" option was unticked. Extensionless, so
/// [`exe_candidates`] turns each into `code.cmd`.
fn windows_code_fallbacks() -> Vec<PathBuf> {
    let install = |root: PathBuf| root.join("Microsoft VS Code").join("bin").join("code");
    let mut out = Vec::new();
    if let Some(local) = dirs::data_local_dir() {
        out.push(install(local.join("Programs")));
    }
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        out.push(install(PathBuf::from(pf)));
    }
    out
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
    // cmux's own CLI lives in its app bundle; a `cmux` on PATH may be an
    // unrelated tool of the same name, so the bundle is probed first.
    if name == "cmux" {
        if let Some(p) = fallbacks(name).into_iter().find(|p| p.is_file()) {
            return Some(p);
        }
    }
    // 2. No override: the enriched (login-shell) PATH.
    if let Some(p) = which_on(&enriched_path(), name) {
        return Some(p);
    }
    // 3. Then known install locations. On Windows `~/.local/bin/claude` matches
    //    the native installer's `claude.exe`.
    fallbacks(name).into_iter().flat_map(exe_candidates).find(|p| p.is_file() && is_auto_resolvable(p))
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

/// Windows' `CREATE_NO_WINDOW` process-creation flag.
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Keep a console program from opening a console window. A release build is a
/// GUI-subsystem app on Windows (`windows_subsystem = "windows"`), so a console
/// child (`git`, `claude`, `code.cmd`, `cmd`) has no console to inherit and
/// Windows would open a new one for it — a window flashing up on every git call.
/// `tauri dev` never shows this: the debug build has its own console. A no-op
/// elsewhere.
pub fn hide_console(c: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// The `tokio::process::Command` equivalent of [`hide_console`].
pub fn hide_console_async(c: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(target_os = "windows")]
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

/// A `std::process::Command` for a directly-invoked tool: the resolved binary
/// with the enriched PATH so any sub-tools it calls resolve too, and no console
/// window on Windows ([`hide_console`]).
pub fn command(name: &str) -> std::process::Command {
    let mut c = std::process::Command::new(resolve_tool(name));
    c.env("PATH", enriched_path());
    hide_console(&mut c);
    c
}

/// The `tokio::process::Command` equivalent of [`command`], for async spawns.
pub fn tokio_command(name: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(resolve_tool(name));
    c.env("PATH", enriched_path());
    hide_console_async(&mut c);
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

/// The command that opens `target` (an http(s) URL, a file or a folder) with the
/// user's default handler, like a double-click: macOS `open`, and on Windows the
/// shell's own handler via `rundll32 url.dll,FileProtocolHandler`. Not `cmd /C
/// start`: cmd would re-parse the target, so an `&` in a URL splits the command,
/// and it flashes a console window from a GUI app. Run it with [`spawn_reaped`].
pub fn os_open(target: &std::ffi::OsStr) -> std::process::Command {
    #[cfg(target_os = "windows")]
    {
        let mut c = std::process::Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler").arg(target);
        c
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut c = std::process::Command::new("open");
        c.arg(target);
        c
    }
}

/// The command that shows `path` selected in its parent folder: Finder via
/// `open -R`, Explorer via `explorer /select,"<path>"`. Explorer only honors the
/// select when the quotes sit after the comma, which std's own argument quoting
/// can't produce, hence `raw_arg` (a Windows path can't contain `"`).
pub fn os_reveal(path: &Path) -> std::process::Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("explorer.exe");
        c.raw_arg(format!("/select,\"{}\"", path.display()));
        c
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut c = std::process::Command::new("open");
        c.arg("-R").arg(path);
        c
    }
}

/// Git for Windows' `bash.exe`, the shell Claude Code runs its commands (and so
/// the user's status line) with on Windows. Found as Claude Code finds it:
/// `CLAUDE_CODE_GIT_BASH_PATH` when set, else next to the resolved `git`
/// (`<Git>\cmd\git.exe` or `<Git>\mingw64\bin\git.exe` → `<Git>\bin\bash.exe`),
/// else the default install folder. Never a bare `bash` off PATH, which on
/// Windows can be WSL's `System32\bash.exe`. `None` when none exists.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn git_bash() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    let from_git = find_tool("git").and_then(|git| {
        git.ancestors().skip(1).take(3).map(|root| root.join(r"bin\bash.exe")).find(|b| b.is_file())
    });
    from_git.or_else(|| {
        let p = PathBuf::from(std::env::var_os("ProgramFiles")?).join(r"Git\bin\bash.exe");
        p.is_file().then_some(p)
    })
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
/// binary for real work unless a repo actually uses that agent. A terminal
/// host's own tool is listed only where that host exists ([`listed_here`]).
const TOOLS: &[&str] = &["claude", "codex", "agy", "copilot", "git", "code", "cmux", "wt"];

/// Whether `tool` belongs on this OS's Tool paths list: a terminal host's tool
/// only where that host is supported, everything else everywhere.
fn listed_here(tool: &str) -> bool {
    use crate::terminal_host::TerminalHost;
    match tool {
        "cmux" => TerminalHost::Cmux.supported_here(),
        "wt" => TerminalHost::WindowsTerminal.supported_here(),
        _ => true,
    }
}

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
        .filter(|t| listed_here(t))
        .map(|t| {
            let (path, exists) = resolved_status(t);
            ResolvedTool { tool: (*t).to_string(), path, exists }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Join entries with this platform's PATH separator (`:`, or `;` on
    /// Windows), where an empty entry makes a doubled/leading/trailing one.
    fn path_of(entries: &[&str]) -> String {
        let sep = if cfg!(target_os = "windows") { ";" } else { ":" };
        entries.join(sep)
    }

    #[test]
    fn merge_paths_dedups_and_preserves_order() {
        let p = path_of;
        assert_eq!(merge_paths(&p(&["/a", "/b"]), &p(&["/b", "/c"])), p(&["/a", "/b", "/c"]));
        assert_eq!(merge_paths(&p(&["/a", "/a"]), ""), "/a");
        assert_eq!(merge_paths("", &p(&["/x", "/y"])), p(&["/x", "/y"]));
        // Empty segments (leading/trailing/doubled separators) are dropped.
        assert_eq!(merge_paths(&p(&["/a", "", "/b", ""]), &p(&["", "/c"])), p(&["/a", "/b", "/c"]));
    }

    #[test]
    fn merge_paths_is_idempotent() {
        let once = merge_paths(&path_of(&["/opt/homebrew/bin", "/usr/bin"]), &path_of(&["/usr/bin", "/bin"]));
        let twice = merge_paths(&once, &once);
        assert_eq!(once, twice);
    }

    /// Windows paths contain `:` after the drive letter, so a `:` split would
    /// break every entry; each must survive the merge intact.
    #[cfg(target_os = "windows")]
    #[test]
    fn merge_paths_keeps_windows_drive_letters() {
        assert_eq!(
            merge_paths(r"C:\Users\me\.local\bin;C:\Windows", r"C:\Windows;D:\tools"),
            r"C:\Users\me\.local\bin;C:\Windows;D:\tools"
        );
    }

    #[cfg(unix)]
    #[test]
    fn which_on_finds_a_known_system_binary() {
        // `sh` exists in /bin on every macOS/Linux host the tests run on.
        let found = which_on("/nonexistent:/bin:/usr/bin", "sh");
        assert!(found.is_some(), "expected to find sh on PATH");
        assert!(found.unwrap().is_file());
    }

    /// A bare name resolves to its `%PATHEXT%` form: `cmd` → `cmd.exe`.
    #[cfg(target_os = "windows")]
    #[test]
    fn which_on_finds_a_known_system_binary() {
        let system32 = PathBuf::from(std::env::var("SystemRoot").unwrap()).join("System32");
        let path = format!(r"C:\nonexistent;{}", system32.display());
        let found = which_on(&path, "cmd").expect("expected to find cmd on PATH");
        assert_eq!(found.file_name().unwrap().to_ascii_lowercase(), "cmd.exe");
        // A name that already has an extension is looked up as is.
        assert!(which_on(&path, "cmd.exe").is_some());
    }

    #[test]
    fn exe_candidates_add_extensions_only_on_windows() {
        let c = exe_candidates(PathBuf::from("bin").join("claude"));
        if cfg!(target_os = "windows") {
            assert!(c.contains(&PathBuf::from("bin").join("claude.exe")), "{c:?}");
            assert!(c.contains(&PathBuf::from("bin").join("claude.cmd")), "{c:?}");
            assert!(!c.contains(&PathBuf::from("bin").join("claude")), "bare file isn't runnable: {c:?}");
        } else {
            assert_eq!(c, vec![PathBuf::from("bin").join("claude")]);
        }
        let with_ext = PathBuf::from("bin").join("code.cmd");
        assert_eq!(exe_candidates(with_ext.clone()), vec![with_ext]);
    }

    #[test]
    fn which_on_misses_a_nonexistent_binary() {
        assert!(which_on(&path_of(&["/bin", "/usr/bin"]), "definitely-not-a-real-binary-xyz").is_none());
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

    /// Codex's and Antigravity's Windows installers put the CLI under
    /// `%LOCALAPPDATA%` and only append it to the user `Path`.
    #[cfg(target_os = "windows")]
    #[test]
    fn codex_and_agy_fallbacks_probe_their_windows_installs() {
        let local = dirs::data_local_dir().unwrap();
        assert!(fallbacks("codex").contains(&local.join(r"Programs\OpenAI\Codex\bin\codex")));
        assert!(fallbacks("agy").contains(&local.join(r"agy\bin\agy")));
    }

    #[test]
    fn copilot_fallbacks_probe_homebrew_npm_and_the_install_script() {
        let f = fallbacks("copilot");
        assert!(f.contains(&PathBuf::from("/opt/homebrew/bin/copilot")));
        assert!(f.contains(&PathBuf::from("/usr/local/bin/copilot")));
        assert!(f.contains(&home().join(".local/bin/copilot")));
        assert!(f.contains(&home().join(".npm-global/bin/copilot")));
        assert!(f.iter().all(|p| !is_copilot_shim(p)));
    }

    #[test]
    fn code_fallbacks_probe_the_platform_installs() {
        let f = fallbacks("code");
        if cfg!(target_os = "windows") {
            let local = dirs::data_local_dir().unwrap().join(r"Programs\Microsoft VS Code\bin\code");
            assert!(f.contains(&local), "{f:?}");
            let pf = PathBuf::from(std::env::var_os("ProgramFiles").unwrap()).join(r"Microsoft VS Code\bin\code");
            assert!(f.contains(&pf), "{f:?}");
        } else {
            assert!(f.contains(&PathBuf::from(
                "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"
            )));
        }
    }

    /// The file a runnable `copilot` is on this platform (`copilot.exe` on Windows).
    fn copilot_file() -> &'static str {
        if cfg!(target_os = "windows") { "copilot.exe" } else { "copilot" }
    }

    /// A fake `copilot` at `dir/rel`, returning the directory holding it.
    fn fake_copilot(root: &std::path::Path, rel: &str) -> PathBuf {
        let dir = root.join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(copilot_file()), "#!/bin/sh\n").unwrap();
        dir
    }

    /// Auto-resolution skips VS Code's Copilot Chat shim: with the real CLI
    /// later on PATH it finds the real one, and with only the shim it finds
    /// nothing (so we never trigger the shim's interactive installer).
    #[test]
    fn which_on_skips_the_vscode_copilot_shim() {
        let root = tempfile::tempdir().unwrap();
        let shim = fake_copilot(
            root.path(),
            "Library/Application Support/Code/User/globalStorage/github.copilot-chat/copilotCli",
        );
        let real = fake_copilot(root.path(), "npm/bin");
        let both = std::env::join_paths([&shim, &real]).unwrap().into_string().unwrap();
        assert_eq!(which_on(&both, "copilot"), Some(real.join(copilot_file())));
        assert_eq!(copilot_shim_on(&both), Some(shim.join(copilot_file())));

        let only_shim = shim.to_string_lossy().into_owned();
        assert_eq!(which_on(&only_shim, "copilot"), None, "the shim alone is not found");
        assert_eq!(copilot_shim_on(&only_shim), Some(shim.join(copilot_file())));
        assert_eq!(copilot_shim_on(&real.to_string_lossy()), None);
    }

    /// Windows Terminal's app execution alias is probed on Windows only.
    #[test]
    fn wt_fallback_is_the_app_execution_alias() {
        let f = fallbacks("wt");
        if cfg!(target_os = "windows") {
            let alias = dirs::data_local_dir().unwrap().join(r"Microsoft\WindowsApps\wt");
            assert_eq!(f, [alias]);
        } else {
            assert!(f.is_empty());
        }
    }

    /// Settings lists each terminal host's tool only on its own OS; the agents,
    /// `git` and `code` everywhere.
    #[test]
    fn terminal_host_tools_are_listed_only_where_they_run() {
        let listed: Vec<&str> = TOOLS.iter().copied().filter(|t| listed_here(t)).collect();
        for t in ["claude", "codex", "agy", "copilot", "git", "code"] {
            assert!(listed.contains(&t), "{t}");
        }
        assert_eq!(listed.contains(&"cmux"), cfg!(target_os = "macos"));
        assert_eq!(listed.contains(&"wt"), cfg!(target_os = "windows"));
    }

    #[test]
    fn tools_list_includes_every_agent() {
        for agent in crate::agent::Agent::ALL {
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

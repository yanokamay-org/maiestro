//! Editor (VS Code) control: generate a worktree's `.vscode` workspace files,
//! launch/focus a window for a folder, and — for teardown — find, probe, and close
//! the window via the macOS accessibility API.
//!
//! mAIestro Code launches sessions but does not host them (see CLAUDE.md): these fns
//! open a real VS Code window whose integrated terminal starts the user-facing
//! agent session, and later close it. Window control goes through System Events
//! (`osascript`) rather than direct Apple events, matching `focus_editor_window`
//! and avoiding a second Automation grant. On Windows it uses Win32 window
//! enumeration and messages instead (the `win32` module, #166). Extracted from
//! `spawn.rs` (issue #99).

use std::path::Path;
use std::process::Command;

use crate::agent::Agent;
use crate::terminal_host::{session_argv, shell_command, LaunchOptions};

// ── VS Code workspace files ─────────────────────────────────────────────────────

/// Write the worktree's `.vscode/{settings,tasks}.json`: title-bar theming keyed
/// to `color`, a `window.title` marker teardown finds the window by, and a
/// folder-open task that starts the user-facing `agent` session in the integrated
/// terminal (see [`session_command`]), invoked through the resolved agent path
/// rather than PATH.
pub fn write_vscode_files(
    work_dir: &Path,
    work_parent: &str,
    color: &str,
    session_title: &str,
    agent: Agent,
    launch: LaunchOptions,
) -> Result<(), String> {
    let vscode = work_dir.join(".vscode");
    std::fs::create_dir_all(&vscode).map_err(|e| e.to_string())?;

    let settings = serde_json::json!({
        // The palette is Claude Code's own mid-tone session colors, so pin a
        // white foreground: VS Code's theme default (light grey on dark
        // themes, near-black on light ones) is unreadable on some of them.
        "workbench.colorCustomizations": {
            "titleBar.activeBackground": color,
            "titleBar.activeForeground": "#ffffff",
            "titleBar.inactiveBackground": format!("{color}99"),
            "titleBar.inactiveForeground": "#ffffffcc",
            "statusBar.background": color,
            "statusBar.foreground": "#ffffff",
            "activityBar.background": color,
            "activityBar.foreground": "#ffffff",
            "activityBar.inactiveForeground": "#ffffff99",
        },
        "workbench.startupEditor": "none",
        // Marker used by teardown to find this window via AppleScript.
        "window.title": format!("${{dirty}}${{activeEditorShort}}${{separator}}{work_parent}/${{rootName}}"),
        "terminal.integrated.gpuAcceleration": "off",
        // Claude Code's TUI draws box/powerline glyphs, so the terminal the
        // session runs in wants a Nerd Font. The stack is a Preferences setting
        // (`terminal_font_family`); its schema default degrades in order — a
        // patched JetBrains Mono, then Cascadia Code, then Menlo, which every
        // macOS has shipped since 10.6 — so an unpatched machine still lands on
        // a real monospace face rather than the generic `monospace` alias.
        "terminal.integrated.fontFamily": crate::app_settings::terminal_font_family(),
    });
    std::fs::write(
        vscode.join("settings.json"),
        serde_json::to_string_pretty(&settings).unwrap() + "\n",
    )
    .map_err(|e| e.to_string())?;

    let mut task = serde_json::json!({
        "label": format!("Start {}", agent.display_name()),
        "isBackground": true,
        "problemMatcher": [],
        "presentation": { "reveal": "always", "panel": "new", "focus": true },
        "runOptions": { "runOn": "folderOpen" },
    });
    let (program, args) = session_argv(agent, color, session_title, launch);
    if cfg!(target_os = "windows") {
        // A `process` task runs the program directly with an argv — no shell,
        // so no quoting that depends on the user's default terminal shell
        // (PowerShell would read a quoted path followed by flags as an
        // expression, cmd uses different quotes entirely).
        task["type"] = "process".into();
        task["command"] = program.into();
        task["args"] = args.into_iter().map(|(a, _)| a).collect::<Vec<_>>().into();
    } else {
        task["type"] = "shell".into();
        task["command"] = shell_command(&program, &args).into();
    }
    let tasks = serde_json::json!({ "version": "2.0.0", "tasks": [task] });
    std::fs::write(
        vscode.join("tasks.json"),
        serde_json::to_string_pretty(&tasks).unwrap() + "\n",
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Drop the folder-open task [`write_vscode_files`] generated, when a session
/// moves to another terminal host: left behind, opening the folder in VS Code
/// would start a second agent in the worktree. A `tasks.json` the repo itself
/// tracks is restored from `HEAD` rather than deleted; one that isn't ours (no
/// folder-open "Start …" task) is left alone. `settings.json` only themes the
/// window, so it stays.
pub async fn remove_session_task(work_dir: &Path) {
    let path = work_dir.join(".vscode").join("tasks.json");
    let ours = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .is_some_and(|v| is_session_task(&v));
    if !ours {
        return;
    }
    let rel = ".vscode/tasks.json";
    let result = if crate::gitops::git(work_dir, &["ls-files", "--error-unmatch", rel]).await.is_ok() {
        crate::gitops::git(work_dir, &["checkout", "HEAD", "--", rel]).await.map(drop)
    } else {
        std::fs::remove_file(&path).map_err(|e| e.to_string())
    };
    if let Err(e) = result {
        tracing::warn!(error = %e, "could not remove the VS Code session task");
    }
}

/// Whether `tasks` is the file [`write_vscode_files`] writes: a single
/// folder-open "Start <agent>" task.
fn is_session_task(tasks: &serde_json::Value) -> bool {
    let list = tasks["tasks"].as_array();
    list.is_some_and(|l| {
        l.len() == 1
            && l[0]["label"].as_str().is_some_and(|s| s.starts_with("Start "))
            && l[0]["runOptions"]["runOn"] == "folderOpen"
    })
}

// ── Launch / focus ──────────────────────────────────────────────────────────────

/// Open `dir` in VS Code, preferring the `code` CLI so we can pass
/// `--disable-workspace-trust` (skipping the trust prompt on every fresh
/// worktree), falling back to Launch Services when the CLI isn't found (macOS
/// only; on Windows a missing CLI is an error).
pub fn open_vscode(dir: &Path) -> Result<(), String> {
    // Prefer the `code` CLI so we can pass --disable-workspace-trust and skip the
    // "Do you trust the authors of the files in this folder?" prompt on every
    // freshly spawned worktree. The CLI forwards the flag even to an already
    // running VS Code, which `open -a --args` cannot.
    if crate::tools::find_tool("code").is_some() {
        crate::tools::spawn_reaped(
            crate::tools::command("code")
                .arg("--disable-workspace-trust")
                .arg(dir),
        )
        .map_err(|e| format!("failed to open VS Code: {e}"))?;
        return Ok(());
    }
    // Windows has no second launch path: `find_tool` already probed VS Code's
    // default install folders for its CLI (`tools::fallbacks`).
    if cfg!(target_os = "windows") {
        return Err("Visual Studio Code not found: no `code` CLI on PATH or in its default install folders".into());
    }
    // Fallback: Launch Services. --args forwards the flag, but only honored when
    // VS Code isn't already running.
    crate::tools::spawn_reaped(
        Command::new("open")
            .args(["-a", "Visual Studio Code", "--args", "--disable-workspace-trust"])
            .arg(dir),
    )
    .map_err(|e| format!("failed to open VS Code: {e}"))?;
    Ok(())
}

/// The substring that identifies a worktree's VS Code window — the same
/// `work_parent/rootName` we bake into `window.title` on spawn.
pub fn window_marker(work_dir: &Path) -> Option<String> {
    let name = work_dir.file_name()?.to_str()?;
    let parent = work_dir.parent()?.file_name()?.to_str()?;
    Some(format!("{parent}/{name}"))
}

/// Sanitize a value for embedding inside an AppleScript double-quoted string
/// literal: drop both `"` (would close the literal early) and `\` (AppleScript's
/// escape character — a trailing one would escape the closing quote and break the
/// script). `marker` is already path-safe in normal use; this is belt-and-braces.
#[cfg_attr(target_os = "windows", allow(dead_code))]
fn applescript_literal_safe(s: &str) -> String {
    s.replace(['"', '\\'], "")
}

/// Whether a top-level window is a VS Code window for the worktree `marker`
/// names: its title contains the marker (our `window.title` puts it there),
/// and it belongs to VS Code's own executable. The title alone isn't enough to
/// be safe — an Explorer window can show the same folder name — and it can't
/// carry "Visual Studio Code" either: a custom `window.title` replaces VS
/// Code's default, `${appName}` suffix included, so the real title is just
/// e.g. `main.rs - work-200-x/repo`. The executable plays the part the "Code"
/// process filter plays in the macOS scripts.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn is_editor_window(title: &str, exe_name: &str, marker: &str) -> bool {
    let exe = exe_name.to_ascii_lowercase();
    title.contains(marker) && (exe == "code.exe" || exe == "code - insiders.exe")
}

/// VS Code window control on Windows (#166), the counterpart of the System
/// Events scripts: enumerate top-level windows, match them with
/// [`is_editor_window`], and focus or close them with plain Win32 messages.
/// Unlike macOS this needs no permission grant for same-user windows, so the
/// Windows probe never answers `Denied`.
#[cfg(target_os = "windows")]
mod win32 {
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        PostMessageW, SetForegroundWindow, ShowWindow, SW_RESTORE, WM_CLOSE,
    };

    /// The file name of the executable that owns `hwnd` (e.g. `Code.exe`), or
    /// empty when it can't be read.
    fn owner_exe_name(hwnd: HWND) -> String {
        let mut pid = 0u32;
        // SAFETY: plain queries on a window handle / process id; the process
        // handle is closed before returning.
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == 0 {
                return String::new();
            }
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return String::new();
            }
            let mut buf = vec![0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
            CloseHandle(process);
            if ok == 0 {
                return String::new();
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            path.rsplit(['\\', '/']).next().unwrap_or_default().to_string()
        }
    }

    /// Visible top-level VS Code windows for `marker`.
    pub fn find(marker: &str) -> Vec<HWND> {
        struct Search<'a> {
            marker: &'a str,
            found: Vec<HWND>,
        }
        unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
            // SAFETY: `lparam` is the `&mut Search` passed to `EnumWindows`
            // below, alive for the whole (synchronous) enumeration.
            let search = unsafe { &mut *(lparam as *mut Search) };
            if unsafe { IsWindowVisible(hwnd) } == 0 {
                return 1;
            }
            let len = unsafe { GetWindowTextLengthW(hwnd) };
            if len <= 0 {
                return 1;
            }
            let mut buf = vec![0u16; len as usize + 1];
            let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
            let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
            // The cheap title test first; only a match pays for the process lookup.
            if title.contains(search.marker) && super::is_editor_window(&title, &owner_exe_name(hwnd), search.marker) {
                search.found.push(hwnd);
            }
            1 // keep enumerating
        }
        let mut search = Search { marker, found: Vec::new() };
        // SAFETY: the callback only reads window titles and writes to `search`.
        unsafe { EnumWindows(Some(visit), &mut search as *mut Search as LPARAM) };
        search.found
    }

    /// Restore (if minimized) and raise the first matching window. True only
    /// when Windows actually brought it to the front: its foreground lock can
    /// refuse `SetForegroundWindow`, leaving just a flashing taskbar button.
    pub fn focus(marker: &str) -> bool {
        let Some(&hwnd) = find(marker).first() else {
            return false;
        };
        // SAFETY: `hwnd` came from `EnumWindows`; a window that has since
        // closed just makes these calls fail harmlessly.
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetForegroundWindow(hwnd) != 0
        }
    }

    /// Ask every matching window to close, like clicking its close button. Fire
    /// and forget, as on macOS: the caller confirms with [`find`].
    pub fn close(marker: &str) {
        for hwnd in find(marker) {
            // SAFETY: as in `focus`.
            unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
        }
    }
}

/// Look for an open VS Code window whose title contains `marker` and, if found,
/// raise it to the front. Returns true when one was focused; false when none is
/// open *or* Windows refused the switch, so `focus_or_open` falls through to
/// `code <dir>`, which has VS Code raise its own window for an open folder.
#[cfg(target_os = "windows")]
async fn focus_editor_window(marker: &str) -> bool {
    win32::focus(marker)
}

/// Look for an open VS Code window whose title contains `marker` and, if found,
/// raise it to the front and activate the app. Returns true when one was
/// focused. Requires Accessibility permission for System Events; any failure
/// (including a missing grant) is treated as "not found" so the caller can fall
/// back to launching a window.
#[cfg(not(target_os = "windows"))]
async fn focus_editor_window(marker: &str) -> bool {
    // marker is path-safe (slug + repo dir name); sanitize defensively.
    let safe = applescript_literal_safe(marker);
    let script = format!(
        r#"tell application "System Events"
  if not (exists process "Code") then return "notfound"
  tell process "Code"
    repeat with w in windows
      if name of w contains "{safe}" then
        perform action "AXRaise" of w
        set frontmost to true
        return "focused"
      end if
    end repeat
  end tell
end tell
return "notfound""#
    );
    match tokio::process::Command::new("osascript").arg("-e").arg(&script).output().await {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim() == "focused",
        Err(_) => false,
    }
}

/// Open the worktree in VS Code: focus (and bring to the front) an existing
/// window for that folder if one is open, otherwise launch a new window. Backs
/// `session_agent::session_open_in_editor`.
pub async fn focus_or_open(path: &Path) -> Result<(), String> {
    if let Some(marker) = window_marker(path) {
        if focus_editor_window(&marker).await {
            return Ok(());
        }
    }
    open_vscode(path)
}

/// Open a tracked repo's main cloned repo directory in the repo's terminal
/// host. Unlike `session_open_in_editor` this is a pure launch — no worktree,
/// no session, no status, no agent: VS Code goes through the same `open_vscode`
/// path as spawned worktrees, Terminal.app gets a plain shell window there
/// (`terminal_app::open_folder`), cmux a plain workspace (`cmux::open_folder`),
/// and Windows Terminal a plain tab (`windows_terminal::open_folder`).
#[tauri::command]
pub async fn open_repo_in_editor(repo: String) -> Result<(), String> {
    crate::log_invoke!("open_repo_in_editor", repo = %repo);
    let settings = crate::repo_settings::repo_settings_get(repo.clone())?;
    let cloned_repo = crate::repo_context::validated_cloned_repo(&settings)?;
    match crate::repo_settings::effective_terminal_host(&settings) {
        crate::terminal_host::TerminalHost::Vscode => open_vscode(&cloned_repo),
        crate::terminal_host::TerminalHost::TerminalApp => crate::terminal_app::open_folder(&cloned_repo),
        crate::terminal_host::TerminalHost::Cmux => crate::cmux::open_folder(&cloned_repo),
        crate::terminal_host::TerminalHost::WindowsTerminal => {
            let window = crate::windows_terminal::folder_window_name(crate::app_settings::terminal_layout(), &repo);
            crate::windows_terminal::open_folder(&cloned_repo, &window).await
        }
    }
}

// ── Teardown window control ─────────────────────────────────────────────────────

/// Close the VS Code window(s) for this worktree by pressing each matching
/// window's native close button via the accessibility API. We deliberately use
/// System Events here (the same path `focus_editor_window` uses) rather than
/// direct Apple events to "Visual Studio Code": Electron's scripting suite is
/// unreliable, and the direct-events path also needs a *separate* Automation
/// grant that we'd never prompted for — so the close was failing silently.
/// No-op if VS Code isn't running.
#[cfg(not(target_os = "windows"))]
pub async fn close_editor_window(marker: &str) {
    let safe = applescript_literal_safe(marker);
    let script = format!(
        r#"tell application "System Events"
  if not (exists process "Code") then return
  tell process "Code"
    repeat with w in windows
      if name of w contains "{safe}" then
        try
          perform action "AXPress" of (first button of w whose subrole is "AXCloseButton")
        end try
      end if
    end repeat
  end tell
end tell"#
    );
    let _ = tokio::process::Command::new("osascript").arg("-e").arg(&script).output().await;
}

/// Close the VS Code window(s) for this worktree (`WM_CLOSE`, as the close
/// button sends). No-op when none is open.
#[cfg(target_os = "windows")]
pub async fn close_editor_window(marker: &str) {
    win32::close(marker);
}

/// What we could learn about a worktree's VS Code window. The `Denied` case is
/// critical: when mAIestro Code lacks Accessibility permission, osascript errors and
/// we genuinely cannot see the window — which must NOT be mistaken for "closed",
/// or teardown would delete the folder out from under a live VS Code and crash it.
pub enum WinProbe {
    /// A window whose title contains the marker is open.
    Open,
    /// VS Code isn't running, or no window matches the marker.
    Absent,
    /// Couldn't determine — almost always a missing Accessibility grant. macOS
    /// only; the Windows probe always knows.
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    Denied,
}

/// Probe for an open VS Code window whose title contains `marker`. Never
/// `Denied` on Windows: enumerating same-user windows needs no grant.
#[cfg(target_os = "windows")]
pub async fn probe_editor_window(marker: &str) -> WinProbe {
    if win32::find(marker).is_empty() {
        WinProbe::Absent
    } else {
        WinProbe::Open
    }
}

/// Probe for an open VS Code window whose title contains `marker`, via the
/// accessibility API (System Events).
#[cfg(not(target_os = "windows"))]
pub async fn probe_editor_window(marker: &str) -> WinProbe {
    let safe = applescript_literal_safe(marker);
    let script = format!(
        r#"tell application "System Events"
  if not (exists process "Code") then return "absent"
  tell process "Code"
    repeat with w in windows
      if name of w contains "{safe}" then return "open"
    end repeat
  end tell
end tell
return "absent""#
    );
    match tokio::process::Command::new("osascript").arg("-e").arg(&script).output().await {
        Ok(out) if out.status.success() => {
            match String::from_utf8_lossy(&out.stdout).trim() {
                "open" => WinProbe::Open,
                _ => WinProbe::Absent,
            }
        }
        // Non-zero exit (e.g. "-25211 not allowed assistive access") or spawn failure.
        _ => WinProbe::Denied,
    }
}

/// Outcome of [`close_window_and_wait`].
#[derive(Debug, PartialEq, Eq)]
pub enum WindowClose {
    /// The window is confirmed gone (or was never open).
    Closed,
    /// No Accessibility grant, so we could neither close nor see the window, and
    /// something is still using the worktree — the user must close it (or grant
    /// Accessibility so mAIestro Code can).
    InUse,
    /// Accessibility is granted but the window didn't close within the wait.
    StillOpen,
}

/// Close the worktree's VS Code window and **confirm** it is gone before
/// returning `Closed` — never on a guess, because the callers (teardown deletes
/// the folder, a restart relaunches the agent) must not act under a live window.
/// Without Accessibility we fall back to the permission-free `worktree_in_use`
/// check. Waits up to ~4 s for an in-flight close.
pub async fn close_window_and_wait(work_dir: &Path) -> WindowClose {
    let Some(marker) = window_marker(work_dir) else {
        return WindowClose::Closed;
    };
    close_editor_window(&marker).await;
    let mut waited = 0u64;
    loop {
        match probe_editor_window(&marker).await {
            WinProbe::Absent => return WindowClose::Closed,
            WinProbe::Denied => {
                return if worktree_in_use(work_dir).await { WindowClose::InUse } else { WindowClose::Closed };
            }
            WinProbe::Open => {
                if waited >= 4000 {
                    return WindowClose::StillOpen;
                }
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                waited += 300;
            }
        }
    }
}

/// Whether the worktree's VS Code window is (or, lacking Accessibility, looks)
/// open: the window probe, falling back to `worktree_in_use` when denied.
pub async fn editor_window_open(work_dir: &Path) -> bool {
    let Some(marker) = window_marker(work_dir) else {
        return false;
    };
    match probe_editor_window(&marker).await {
        WinProbe::Open => true,
        WinProbe::Absent => false,
        WinProbe::Denied => worktree_in_use(work_dir).await,
    }
}

/// Permission-free safety net: is any process's working directory inside this
/// worktree? Our spawned agent (`claude` or `codex`) runs in VS Code's integrated terminal with its
/// cwd in the worktree, so this catches the common "still open" case without
/// needing Accessibility. Uses `lsof -d cwd` (process CWDs only) to avoid the
/// slow tree walk that `lsof +D` would do over a full cloned repo.
#[cfg(not(target_os = "windows"))]
pub async fn worktree_in_use(work_dir: &Path) -> bool {
    let dir = work_dir.to_string_lossy();
    // Async so the reachable-from-`teardown` `lsof` scan doesn't block a tokio
    // worker (#101). `lsof` is a system binary always on the minimal PATH, so it
    // doesn't go through `tools`.
    match tokio::process::Command::new("lsof").args(["-d", "cwd", "-Fn"]).output().await {
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|l| l.strip_prefix('n').is_some_and(|p| path_at_or_under(p, &dir))),
        Err(_) => false,
    }
}

/// Never reached on Windows: it backs only the `WinProbe::Denied` branches, and
/// the Windows probe always knows (same-user windows need no grant). There is
/// no `lsof` there either, so this is a stub rather than a process scan.
#[cfg(target_os = "windows")]
pub async fn worktree_in_use(_work_dir: &Path) -> bool {
    false
}

/// Whether `path` is `base` itself or a descendant of it. A plain prefix test
/// would count a sibling like `<base>-2` as inside `<base>`, so require the match
/// to end at a path boundary.
#[cfg(not(target_os = "windows"))]
fn path_at_or_under(path: &str, base: &str) -> bool {
    path == base
        || path
            .strip_prefix(base)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Open System Settings → Privacy & Security → Accessibility so the user can
/// grant mAIestro Code the permission teardown needs to close VS Code windows.
/// Triggered only by an explicit user click — we never launch it automatically.
/// macOS only: Windows has no such grant, and the button that calls this never
/// shows there (its `accessibility` flag needs a `Denied` probe), so the command
/// stays registered but only logs.
#[tauri::command]
pub fn open_accessibility_settings() {
    crate::log_invoke!("open_accessibility_settings");
    #[cfg(not(target_os = "windows"))]
    let _ = crate::tools::spawn_reaped(
        Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"),
    );
    #[cfg(target_os = "windows")]
    tracing::warn!("open_accessibility_settings called on Windows, which has no Accessibility grant");
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "windows"))]
    use super::path_at_or_under;
    use super::{is_editor_window, write_vscode_files};
    use crate::terminal_host::LaunchOptions;

    /// The default launch: remote control on, as the schema default has it.
    const ON: LaunchOptions = LaunchOptions { remote_control: true };

    /// Teardown only ever matches (and so closes) a VS Code window for this
    /// exact worktree. The real Windows title carries no "Visual Studio Code"
    /// (our `window.title` replaces VS Code's default, suffix included), so
    /// VS Code is recognized by its executable instead.
    #[test]
    fn editor_window_matches_only_vscode_for_the_worktree() {
        let marker = "work-200-tray-brain-icon-red/maiestro";
        assert!(is_editor_window("Welcome - work-200-tray-brain-icon-red/maiestro", "Code.exe", marker));
        assert!(is_editor_window("main.rs - work-200-tray-brain-icon-red/maiestro", "code.exe", marker));
        assert!(is_editor_window("work-200-tray-brain-icon-red/maiestro", "Code - Insiders.exe", marker));
        assert!(!is_editor_window("work-200-tray-brain-icon-red/maiestro", "explorer.exe", marker));
        assert!(!is_editor_window("work-200-tray-brain-icon-red/maiestro", "", marker));
        assert!(!is_editor_window("Welcome - work-201-other/maiestro", "Code.exe", marker));
    }
    use crate::agent::Agent;

    /// The task's command line in its POSIX shell form, whichever platform
    /// wrote it: the `shell` task's own string (macOS), or the same string
    /// rebuilt from a Windows `process` task's argv — so one set of assertions
    /// covers both.
    fn task_command_line(task: &serde_json::Value) -> String {
        let command = task["command"].as_str().unwrap();
        if task["type"] == "shell" {
            return command.to_string();
        }
        assert_eq!(task["type"], "process", "{task}");
        let args: Vec<(String, bool)> = task["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                let a = a.as_str().unwrap().to_string();
                let quote = !a.starts_with('-');
                (a, quote)
            })
            .collect();
        crate::terminal_host::shell_command(command, &args)
    }

    fn read_task(dir: &std::path::Path) -> serde_json::Value {
        let tasks: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(".vscode/tasks.json")).unwrap()).unwrap();
        tasks["tasks"][0].clone()
    }

    /// Windows runs the session as a `process` task: the resolved binary and a
    /// plain argv, nothing for a shell to parse (#165). macOS keeps the `shell`
    /// task with a single-quoted command string.
    #[test]
    fn startup_task_is_a_process_on_windows_and_a_shell_command_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        write_vscode_files(dir.path(), "work-2-x", "#c46686", "\u{1f981} #2 \u{2014} It's here", Agent::Claude, ON).unwrap();
        let task = read_task(dir.path());
        let bin = crate::tools::resolve_tool("claude").to_string_lossy().into_owned();
        if cfg!(target_os = "windows") {
            assert_eq!(task["type"], "process");
            assert_eq!(task["command"], bin.as_str());
            assert_eq!(
                task["args"],
                serde_json::json!(["--remote-control", "--name", "\u{1f981} #2 \u{2014} It's here", "/color pink"])
            );
        } else {
            assert_eq!(task["type"], "shell");
            assert!(task.get("args").is_none(), "{task}");
            assert!(task["command"].as_str().unwrap().contains("--name '\u{1f981} #2 \u{2014} It'\\''s here'"), "{task}");
        }
    }

    /// The generated folder-open task carries the worktree's theme into the
    /// session as a `/color <name>` initial prompt, quoted as one argv entry, and
    /// still names the session. A palette hex with no mapping degrades to
    /// `default` rather than emitting a color Claude would reject.
    #[test]
    fn startup_task_themes_the_session() {
        let dir = tempfile::tempdir().unwrap();
        write_vscode_files(dir.path(), "work-127-add-session-color", "#c46686", "\u{1f380} #127 \u{2014} Add session color", Agent::Claude, ON).unwrap();

        let command = task_command_line(&read_task(dir.path()));
        assert!(command.contains("'/color pink'"), "not themed: {command}");
        assert!(command.contains("--name '\u{1f380} #127 \u{2014} Add session color'"), "not named: {command}");

        write_vscode_files(dir.path(), "work-1-x", "#nonsense", "x", Agent::Claude, ON).unwrap();
        assert!(task_command_line(&read_task(dir.path())).contains("'/color default'"));
    }

    /// The task invokes the *resolved* `claude` binary (shell-quoted, so a path
    /// with spaces survives), not a bare `claude` left to the integrated
    /// terminal's PATH (issue #134).
    #[test]
    fn startup_task_uses_the_resolved_claude_path() {
        let dir = tempfile::tempdir().unwrap();
        write_vscode_files(dir.path(), "work-134-x", "#c46686", "x", Agent::Claude, ON).unwrap();

        let command = task_command_line(&read_task(dir.path()));
        let expected = crate::tools::shell_quote(&crate::tools::resolve_tool("claude").to_string_lossy());
        assert!(
            command.starts_with(&format!("{expected} --remote-control")),
            "expected the resolved claude path {expected}: {command}"
        );
    }

    /// With remote control turned off for the repo, the Claude task drops
    /// `--remote-control` and keeps everything else; the other agents never
    /// pass it either way.
    #[test]
    fn startup_task_omits_remote_control_when_off() {
        // The Codex launch command embeds a path under MAIESTRO_HOME; hold a
        // TempHome so a concurrent env-overriding test can't change it between
        // the two writes compared below.
        let _home = crate::testutil::TempHome::new();
        let dir = tempfile::tempdir().unwrap();
        let off = LaunchOptions { remote_control: false };
        write_vscode_files(dir.path(), "work-209-x", "#c46686", "x", Agent::Claude, off).unwrap();

        let command = task_command_line(&read_task(dir.path()));
        let expected = crate::tools::shell_quote(&crate::tools::resolve_tool("claude").to_string_lossy());
        assert_eq!(command, format!("{expected} --name 'x' '/color pink'"));

        for agent in [Agent::Codex, Agent::Antigravity, Agent::Copilot] {
            write_vscode_files(dir.path(), "work-209-x", "#c46686", "x", agent, ON).unwrap();
            let on = task_command_line(&read_task(dir.path()));
            assert!(!on.contains("--remote-control"), "{agent:?}: {on}");
            write_vscode_files(dir.path(), "work-209-x", "#c46686", "x", agent, off).unwrap();
            assert_eq!(task_command_line(&read_task(dir.path())), on, "{agent:?} ignores the flag");
        }
    }

    /// The terminal font stack comes from the `terminal_font_family` preference
    /// (schema default when unset), not a copy hardcoded here.
    #[test]
    fn terminal_font_comes_from_preferences() {
        let dir = tempfile::tempdir().unwrap();
        write_vscode_files(dir.path(), "work-134-x", "#c46686", "x", Agent::Claude, ON).unwrap();

        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".vscode/settings.json")).unwrap()).unwrap();
        let font = settings["terminal.integrated.fontFamily"].as_str().unwrap();
        assert_eq!(font, crate::app_settings::terminal_font_family());
    }

    /// A Codex worktree's task runs the resolved `codex` binary with the status
    /// hooks as `-c` session flags — no `--name`, no `/color`, no initial prompt —
    /// under a "Start Codex" label, while the VS Code bars are still themed.
    #[test]
    fn codex_task_runs_the_bare_resolved_codex() {
        let dir = tempfile::tempdir().unwrap();
        write_vscode_files(dir.path(), "work-162-x", "#c46686", "x", Agent::Codex, ON).unwrap();

        let task = &read_task(dir.path());
        let expected = crate::tools::shell_quote(&crate::tools::resolve_tool("codex").to_string_lossy());
        let command = task_command_line(task);
        assert!(command.starts_with(&format!("{expected} -c 'hooks.SessionStart=")), "{command}");
        assert_eq!(command.matches(" -c ").count(), 7, "{command}");
        assert!(!command.contains("/color") && !command.contains("--name"), "{command}");
        assert_eq!(task["label"], "Start Codex");
        assert_eq!(task["runOptions"]["runOn"], "folderOpen");

        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".vscode/settings.json")).unwrap()).unwrap();
        assert_eq!(settings["workbench.colorCustomizations"]["titleBar.activeBackground"], "#c46686");
    }

    /// An Antigravity worktree's task runs the resolved `agy` alone — no flags,
    /// no initial prompt (hooks live in `.agents/hooks.json`) — as "Start
    /// Antigravity", with the VS Code bars still themed.
    #[test]
    fn antigravity_task_runs_the_bare_resolved_agy() {
        let dir = tempfile::tempdir().unwrap();
        write_vscode_files(dir.path(), "work-185-x", "#c46686", "x", Agent::Antigravity, ON).unwrap();
        let task = &read_task(dir.path());
        let expected = crate::tools::shell_quote(&crate::tools::resolve_tool("agy").to_string_lossy());
        assert_eq!(task_command_line(task), expected);
        assert_eq!(task["label"], "Start Antigravity");
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".vscode/settings.json")).unwrap()).unwrap();
        assert_eq!(settings["workbench.colorCustomizations"]["titleBar.activeBackground"], "#c46686");
    }

    /// A Copilot worktree's task runs the resolved `copilot` with the session
    /// title as `--name` — no initial prompt (it would spend a premium request)
    /// and no `/color` — as "Start Copilot", with the VS Code bars still themed.
    #[test]
    fn copilot_task_runs_the_resolved_copilot_with_a_name() {
        let dir = tempfile::tempdir().unwrap();
        write_vscode_files(dir.path(), "work-203-x", "#c46686", "⭐ #203 — It's Copilot", Agent::Copilot, ON).unwrap();
        let task = &read_task(dir.path());
        let expected = crate::tools::shell_quote(&crate::tools::resolve_tool("copilot").to_string_lossy());
        assert_eq!(task_command_line(task), format!(r"{expected} --name '⭐ #203 — It'\''s Copilot'"));
        assert_eq!(task["label"], "Start Copilot");
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".vscode/settings.json")).unwrap()).unwrap();
        assert_eq!(settings["workbench.colorCustomizations"]["titleBar.activeBackground"], "#c46686");
    }

    /// Only the task file we generate counts as ours.
    #[test]
    fn recognizes_only_the_generated_session_task() {
        let ours = serde_json::json!({ "version": "2.0.0", "tasks": [
            { "label": "Start Claude", "runOptions": { "runOn": "folderOpen" } }
        ]});
        assert!(super::is_session_task(&ours));
        let theirs = serde_json::json!({ "version": "2.0.0", "tasks": [{ "label": "build" }] });
        assert!(!super::is_session_task(&theirs));
        let more = serde_json::json!({ "tasks": [
            { "label": "Start Claude", "runOptions": { "runOn": "folderOpen" } }, { "label": "build" }
        ]});
        assert!(!super::is_session_task(&more));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn path_at_or_under_requires_boundary() {
        let base = "/src/work-8/repo";
        assert!(path_at_or_under(base, base));
        assert!(path_at_or_under("/src/work-8/repo/frontend", base));
        // A sibling whose name merely extends the base is NOT inside it.
        assert!(!path_at_or_under("/src/work-8/repo-2", base));
        assert!(!path_at_or_under("/src/work-80/repo", base));
        assert!(!path_at_or_under("/elsewhere", base));
    }
}

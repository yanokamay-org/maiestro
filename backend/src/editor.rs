//! Editor (VS Code) control: generate a worktree's `.vscode` workspace files,
//! launch/focus a window for a folder, and — for teardown — find, probe, and close
//! the window via the macOS accessibility API.
//!
//! mAIestro Code launches sessions but does not host them (see CLAUDE.md): these fns
//! open a real VS Code window whose integrated terminal starts the user-facing
//! agent session, and later close it. Window control goes through System Events
//! (`osascript`) rather than direct Apple events, matching `focus_editor_window`
//! and avoiding a second Automation grant. Extracted from `spawn.rs` (issue #99).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::agent::Agent;
use crate::tools::shell_quote;

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

    let command = session_command(agent, color, session_title);
    let tasks = serde_json::json!({
        "version": "2.0.0",
        "tasks": [{
            "label": format!("Start {}", agent.display_name()),
            "type": "shell",
            "command": command,
            "isBackground": true,
            "problemMatcher": [],
            "presentation": { "reveal": "always", "panel": "new", "focus": true },
            "runOptions": { "runOn": "folderOpen" },
        }],
    });
    std::fs::write(
        vscode.join("tasks.json"),
        serde_json::to_string_pretty(&tasks).unwrap() + "\n",
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// The shell command the folder-open task runs to start a real, user-facing
/// `agent` session in the integrated terminal.
///
/// **Claude:** `--remote-control` lets the user drive the session remotely;
/// mAIestro Code still only launches it, it does not host it. `--name` gives the
/// session the same display name mAIestro Code tracks it by, and the trailing
/// `/color <name>` prompt carries the worktree's theme into the session UI so it
/// matches the dashboard row and the title bar. The color goes through the
/// initial *prompt* rather than a flag because Claude Code's `--agent-color` is
/// only honored alongside `--agent-id`/`--agent-name`/`--team-name` (teammate
/// sessions); passing it on its own is silently ignored. A leading-slash initial
/// prompt is dispatched as a command, so it costs one line in the transcript and
/// no model call.
///
/// **Codex:** the binary plus mAIestro Code's status hooks as `-c hooks.…`
/// session flags (`hooks::codex_hook_overrides`) — identical for every worktree,
/// so the user's one-time Codex hook trust covers them all. No initial prompt:
/// Codex has no `--name` or `/color`, and any prompt would start a real model
/// turn, so the session carries no name or color of its own — the VS Code bars
/// are still themed.
///
/// Either way the binary is the **resolved** agent path (`tools::resolve_tool`),
/// not a bare name left to PATH (issue #134). The task runs in VS Code's
/// integrated terminal, whose PATH is whatever the VS Code process inherited —
/// and when mAIestro Code launched that VS Code from the packaged bundle at login,
/// that can be the minimal Launch Services PATH with no agent on it. The same
/// `tool_paths` override that pins mAIestro Code's own drafting calls therefore
/// also decides which binary the session starts with. When nothing concrete
/// resolves, `resolve_tool` yields the bare name, i.e. exactly the previous
/// behavior.
fn session_command(agent: Agent, color: &str, session_title: &str) -> String {
    let bin = shell_quote(&crate::tools::resolve_tool(agent.tool()).to_string_lossy());
    match agent {
        Agent::Claude => format!(
            "{bin} --remote-control --name {} {}",
            shell_quote(session_title),
            shell_quote(&format!("/color {}", crate::theming::claude_color(color)))
        ),
        Agent::Codex => std::iter::once(bin)
            .chain(crate::hooks::codex_hook_overrides().iter().map(|o| format!("-c {}", shell_quote(o))))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

// ── Launch / focus ──────────────────────────────────────────────────────────────

/// Open `dir` in VS Code, preferring the `code` CLI so we can pass
/// `--disable-workspace-trust` (skipping the trust prompt on every fresh
/// worktree), falling back to Launch Services when the CLI isn't found.
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
fn applescript_literal_safe(s: &str) -> String {
    s.replace(['"', '\\'], "")
}

/// Look for an open VS Code window whose title contains `marker` and, if found,
/// raise it to the front and activate the app. Returns true when one was
/// focused. Requires Accessibility permission for System Events; any failure
/// (including a missing grant) is treated as "not found" so the caller can fall
/// back to launching a window.
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
/// window for that folder if one is open, otherwise launch a new window.
#[tauri::command]
pub async fn open_in_editor(work_dir: String) -> Result<(), String> {
    crate::log_invoke!("open_in_editor", work_dir = %work_dir);
    let path = PathBuf::from(&work_dir);
    if let Some(marker) = window_marker(&path) {
        if focus_editor_window(&marker).await {
            return Ok(());
        }
    }
    open_vscode(&path)
}

/// Open a tracked repo's main cloned repo directory in VS Code. Unlike
/// `open_in_editor` this is a pure launch — no worktree, no session, no status —
/// reusing the same `open_vscode` path logic as spawned worktrees.
#[tauri::command]
pub async fn open_repo_in_editor(repo: String) -> Result<(), String> {
    crate::log_invoke!("open_repo_in_editor", repo = %repo);
    let settings = crate::repo_settings::repo_settings_get(repo)?;
    let cloned_repo = crate::repo_context::validated_cloned_repo(&settings)?;
    open_vscode(&cloned_repo)
}

// ── Teardown window control ─────────────────────────────────────────────────────

/// Close the VS Code window(s) for this worktree by pressing each matching
/// window's native close button via the accessibility API. We deliberately use
/// System Events here (the same path `focus_editor_window` uses) rather than
/// direct Apple events to "Visual Studio Code": Electron's scripting suite is
/// unreliable, and the direct-events path also needs a *separate* Automation
/// grant that we'd never prompted for — so the close was failing silently.
/// No-op if VS Code isn't running.
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

/// What we could learn about a worktree's VS Code window. The `Denied` case is
/// critical: when mAIestro Code lacks Accessibility permission, osascript errors and
/// we genuinely cannot see the window — which must NOT be mistaken for "closed",
/// or teardown would delete the folder out from under a live VS Code and crash it.
pub enum WinProbe {
    /// A window whose title contains the marker is open.
    Open,
    /// VS Code isn't running, or no window matches the marker.
    Absent,
    /// Couldn't determine — almost always a missing Accessibility grant.
    Denied,
}

/// Probe for an open VS Code window whose title contains `marker`, via the
/// accessibility API (System Events).
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

/// Permission-free safety net: is any process's working directory inside this
/// worktree? Our spawned agent (`claude` or `codex`) runs in VS Code's integrated terminal with its
/// cwd in the worktree, so this catches the common "still open" case without
/// needing Accessibility. Uses `lsof -d cwd` (process CWDs only) to avoid the
/// slow tree walk that `lsof +D` would do over a full cloned repo.
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

/// Whether `path` is `base` itself or a descendant of it. A plain prefix test
/// would count a sibling like `<base>-2` as inside `<base>`, so require the match
/// to end at a path boundary.
fn path_at_or_under(path: &str, base: &str) -> bool {
    path == base
        || path
            .strip_prefix(base)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Open System Settings → Privacy & Security → Accessibility so the user can
/// grant mAIestro Code the permission teardown needs to close VS Code windows.
/// Triggered only by an explicit user click — we never launch it automatically.
#[tauri::command]
pub fn open_accessibility_settings() {
    crate::log_invoke!("open_accessibility_settings");
    let _ = crate::tools::spawn_reaped(
        Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"),
    );
}

#[cfg(test)]
mod tests {
    use super::{path_at_or_under, write_vscode_files};
    use crate::agent::Agent;

    /// The generated folder-open task carries the worktree's theme into the
    /// session as a `/color <name>` initial prompt, quoted as one argv entry, and
    /// still names the session. A palette hex with no mapping degrades to
    /// `default` rather than emitting a color Claude would reject.
    #[test]
    fn startup_task_themes_the_session() {
        let dir = tempfile::tempdir().unwrap();
        write_vscode_files(dir.path(), "work-127-add-session-color", "#c46686", "\u{1f380} #127 \u{2014} Add session color", Agent::Claude).unwrap();

        let tasks: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".vscode/tasks.json")).unwrap()).unwrap();
        let command = tasks["tasks"][0]["command"].as_str().unwrap();
        assert!(command.contains("'/color pink'"), "not themed: {command}");
        assert!(command.contains("--name '\u{1f380} #127 \u{2014} Add session color'"), "not named: {command}");

        write_vscode_files(dir.path(), "work-1-x", "#nonsense", "x", Agent::Claude).unwrap();
        let tasks: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".vscode/tasks.json")).unwrap()).unwrap();
        assert!(tasks["tasks"][0]["command"].as_str().unwrap().contains("'/color default'"));
    }

    /// The task invokes the *resolved* `claude` binary (shell-quoted, so a path
    /// with spaces survives), not a bare `claude` left to the integrated
    /// terminal's PATH (issue #134).
    #[test]
    fn startup_task_uses_the_resolved_claude_path() {
        let dir = tempfile::tempdir().unwrap();
        write_vscode_files(dir.path(), "work-134-x", "#c46686", "x", Agent::Claude).unwrap();

        let tasks: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".vscode/tasks.json")).unwrap()).unwrap();
        let command = tasks["tasks"][0]["command"].as_str().unwrap();
        let expected = crate::tools::shell_quote(&crate::tools::resolve_tool("claude").to_string_lossy());
        assert!(
            command.starts_with(&format!("{expected} --remote-control")),
            "expected the resolved claude path {expected}: {command}"
        );
    }

    /// The terminal font stack comes from the `terminal_font_family` preference
    /// (schema default when unset), not a copy hardcoded here.
    #[test]
    fn terminal_font_comes_from_preferences() {
        let dir = tempfile::tempdir().unwrap();
        write_vscode_files(dir.path(), "work-134-x", "#c46686", "x", Agent::Claude).unwrap();

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
        write_vscode_files(dir.path(), "work-162-x", "#c46686", "x", Agent::Codex).unwrap();

        let tasks: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".vscode/tasks.json")).unwrap()).unwrap();
        let task = &tasks["tasks"][0];
        let expected = crate::tools::shell_quote(&crate::tools::resolve_tool("codex").to_string_lossy());
        let command = task["command"].as_str().unwrap();
        assert!(command.starts_with(&format!("{expected} -c 'hooks.SessionStart=")), "{command}");
        assert_eq!(command.matches(" -c ").count(), 7, "{command}");
        assert!(!command.contains("/color") && !command.contains("--name"), "{command}");
        assert_eq!(task["label"], "Start Codex");
        assert_eq!(task["runOptions"]["runOn"], "folderOpen");

        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".vscode/settings.json")).unwrap()).unwrap();
        assert_eq!(settings["workbench.colorCustomizations"]["titleBar.activeBackground"], "#c46686");
    }

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

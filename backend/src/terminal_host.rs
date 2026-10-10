//! Where a session runs: its **terminal host**, the user-facing app whose
//! terminal the agent runs in — a VS Code window (`editor.rs`), a Terminal.app
//! window (`terminal_app.rs`), a cmux workspace (`cmux.rs`) or a Windows
//! Terminal tab (`windows_terminal.rs`). mAIestro Code launches the session into
//! its terminal host and never hosts the conversation itself (see CLAUDE.md).
//!
//! A repo chooses its terminal host (`terminal_host`); each session records the
//! one it runs in. Switching the repo's terminal host moves every existing
//! session of the repo along, closing their windows first (after asking, when
//! any is open), so a repo never has sessions in two apps. Everything that
//! opens, focuses, probes or closes a session's window goes through the
//! dispatch here, so the spawn, reopen, agent-switch and teardown paths stay
//! terminal-host-agnostic.
//!
//! The session command line itself ([`session_argv`]) lives here too: VS Code's
//! folder-open task, Terminal.app and Windows Terminal run exactly the same
//! resolved-agent command.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agent::Agent;
use crate::editor::WindowClose;
use crate::sessions::Session;
use crate::tools::shell_quote;

/// The app a repo opens its sessions in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalHost {
    /// A VS Code window whose folder-open task starts the agent.
    #[default]
    Vscode,
    /// A Terminal.app window (macOS only).
    TerminalApp,
    /// A cmux workspace (macOS only).
    Cmux,
    /// A Windows Terminal tab (Windows only).
    WindowsTerminal,
}

impl TerminalHost {
    /// Whether this terminal host exists on the OS mAIestro Code was built for.
    pub fn supported_here(self) -> bool {
        match self {
            TerminalHost::Vscode => true,
            TerminalHost::TerminalApp | TerminalHost::Cmux => cfg!(target_os = "macos"),
            TerminalHost::WindowsTerminal => cfg!(target_os = "windows"),
        }
    }

    /// The app's name, for user-facing messages.
    pub fn app_name(self) -> &'static str {
        match self {
            TerminalHost::Vscode => "Visual Studio Code",
            TerminalHost::TerminalApp => "Terminal",
            TerminalHost::Cmux => "cmux",
            TerminalHost::WindowsTerminal => "Windows Terminal",
        }
    }

    /// What a session's window is called in this terminal host's messages.
    pub fn window_noun(self) -> &'static str {
        match self {
            TerminalHost::Cmux => "workspace",
            TerminalHost::WindowsTerminal => "tab",
            TerminalHost::Vscode | TerminalHost::TerminalApp => "window",
        }
    }

    /// The macOS grant mAIestro Code needs to see and close this terminal host's
    /// windows: Accessibility for VS Code (System Events), Automation for
    /// Terminal (its own Apple events). `None` for cmux, whose gate is its own
    /// socket control mode rather than a macOS privacy grant, and for Windows
    /// Terminal, whose tabs UI Automation reaches with no grant at all.
    pub fn permission(self) -> Option<Permission> {
        match self {
            TerminalHost::Vscode => Some(Permission::Accessibility),
            TerminalHost::TerminalApp => Some(Permission::Automation),
            TerminalHost::Cmux | TerminalHost::WindowsTerminal => None,
        }
    }

    /// How to ask the user to let mAIestro Code close a window itself,
    /// completing "…, or <this> so it can close the window for you."
    fn grant_phrase(self) -> &'static str {
        match self {
            TerminalHost::Vscode => Permission::Accessibility.grant_phrase(),
            TerminalHost::TerminalApp => Permission::Automation.grant_phrase(),
            TerminalHost::Cmux => "set cmux's Settings → Automation → Socket Control Mode to Automation or Password",
            // Not reached in practice: a tab UI Automation can't see is
            // `StillOpen`, never `InUse`.
            TerminalHost::WindowsTerminal => "try again once Windows Terminal responds",
        }
    }
}

impl std::fmt::Display for TerminalHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TerminalHost::Vscode => "vscode",
            TerminalHost::TerminalApp => "terminal_app",
            TerminalHost::Cmux => "cmux",
            TerminalHost::WindowsTerminal => "windows_terminal",
        })
    }
}

/// How a terminal host that can group sessions (cmux, Windows Terminal)
/// arranges them. Global (`app_settings::terminal_layout`), read at spawn;
/// Terminal.app and VS Code ignore it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalLayout {
    /// A window of its own for every session.
    Windows,
    /// One window per repo, its sessions as tabs.
    #[default]
    PerRepo,
    /// Every session as a tab in one shared window.
    Tabs,
}

/// A macOS privacy grant the UI can open System Settings at, when its absence
/// is what blocked closing a session's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Privacy & Security → Accessibility (System Events window control).
    Accessibility,
    /// Privacy & Security → Automation (Apple events to Terminal).
    Automation,
}

impl Permission {
    /// How to ask the user for the grant, completing "…, or <this> so it can
    /// close the window for you."
    pub fn grant_phrase(self) -> &'static str {
        match self {
            Permission::Accessibility => "enable Accessibility for mAIestro Code",
            Permission::Automation => "allow mAIestro Code to control Terminal (Automation)",
        }
    }
}

// ── The session command ─────────────────────────────────────────────────────────

/// Per-repo launch preferences for the session command, read from the repo's
/// **current** settings each time the task is written (spawn, reopen, agent
/// switch) — unlike the color and agent, which come from the session record,
/// these are preferences rather than part of the worktree's identity. See
/// [`session_argv`] for which agent honors which.
#[derive(Debug, Clone, Copy)]
pub struct LaunchOptions {
    /// Claude: pass `--remote-control` (`agent_settings.claude.remote_control`).
    pub remote_control: bool,
}

impl LaunchOptions {
    /// The launch options `settings` asks for.
    pub fn from_settings(settings: &crate::repo_settings::RepoSettings) -> Self {
        Self { remote_control: crate::repo_settings::claude_remote_control(settings) }
    }
}

/// The program and arguments that start a real, user-facing `agent` session:
/// what VS Code's folder-open task runs in its integrated terminal, what a
/// Terminal.app types into its new window, and what a Windows Terminal tab runs. Each argument carries whether the
/// POSIX shell form ([`shell_command`], macOS) quotes it; VS Code on Windows
/// runs the argv directly as a `process` task.
///
/// **Claude:** `--remote-control` (unless the repo turned it off, see
/// [`LaunchOptions`]) lets the user drive the session remotely; mAIestro Code
/// still only launches it, it does not host it. `--name` gives the
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
/// turn, so the session carries no name or color of its own — the terminal host's
/// window is still themed.
///
/// **Antigravity:** the bare binary. Its status hooks live in the worktree's
/// `.agents/hooks.json` (`hooks/antigravity.rs`), and it has no session-name flag or
/// `/color`; an initial prompt (`-i`) would start a real model turn. Antigravity
/// asks the user to trust each new worktree folder at startup — its own prompt.
///
/// **Copilot:** the binary plus `--name <session title>`, as Claude gets. Its
/// status hooks live in the worktree's `.github/hooks/maiestro-status.json`
/// (`hooks/copilot.rs`), loaded once the user answers Copilot's own "Do you trust the
/// files in this folder?" prompt. No initial prompt: `-i` would start a real
/// model turn (a premium request), and Copilot has no `/color` anyway. No
/// `--no-auto-update` either — updating the interactive CLI is the user's call.
///
/// Whatever the agent, the binary is the **resolved** agent path (`tools::resolve_tool`),
/// not a bare name left to PATH. In VS Code the task runs in the integrated
/// terminal, whose PATH is whatever the VS Code process inherited —
/// and when mAIestro Code launched that VS Code from the packaged bundle at login,
/// that can be the minimal Launch Services PATH with no agent on it. The same
/// `tool_paths` override that pins mAIestro Code's own drafting calls therefore
/// also decides which binary the session starts with. When nothing concrete
/// resolves, `resolve_tool` yields the bare name, i.e. exactly the previous
/// behavior.
pub(crate) fn session_argv(agent: Agent, color: &str, session_title: &str, launch: LaunchOptions) -> (String, Vec<(String, bool)>) {
    let bin = crate::tools::resolve_tool(agent.tool()).to_string_lossy().into_owned();
    let flag = |s: &str| (s.to_string(), false);
    let value = |s: String| (s, true);
    let args = match agent {
        Agent::Claude => launch
            .remote_control
            .then(|| flag("--remote-control"))
            .into_iter()
            .chain([
                flag("--name"),
                value(session_title.to_string()),
                value(format!("/color {}", crate::theming::claude_color(color))),
            ])
            .collect(),
        Agent::Codex => crate::hooks::codex_hook_overrides()
            .into_iter()
            .flat_map(|o| [flag("-c"), value(o)])
            .collect(),
        Agent::Antigravity => Vec::new(),
        Agent::Copilot => vec![flag("--name"), value(session_title.to_string())],
    };
    (bin, args)
}

/// The POSIX shell form of [`session_argv`]: the program and every value
/// argument single-quoted, flags bare.
pub(crate) fn shell_command(program: &str, args: &[(String, bool)]) -> String {
    std::iter::once(shell_quote(program))
        .chain(args.iter().map(|(a, quote)| if *quote { shell_quote(a) } else { a.clone() }))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The launch options for a session of `repo`, from the repo's *current*
/// settings. A settings file that can't be read warns and falls back to the
/// schema defaults: the launch itself must still go ahead.
pub fn launch_options(repo: &str) -> LaunchOptions {
    match crate::repo_settings::repo_settings_get(repo.to_string()) {
        Ok(settings) => LaunchOptions::from_settings(&settings),
        Err(e) => {
            tracing::warn!(error = %e, "could not read repo settings; launching with the default options");
            LaunchOptions::from_settings(&crate::repo_settings::RepoSettings::default_for(repo))
        }
    }
}

/// The title a terminal host gives the session's window or tab: `<repo>:
/// <session title>`.
fn terminal_title(session: &Session) -> String {
    let repo_name = session.repo.split('/').next_back().unwrap_or(&session.repo);
    format!("{repo_name}: {}", session.session_title)
}

// ── Dispatch ────────────────────────────────────────────────────────────────────

/// Start the session in a fresh window of its terminal host. For Terminal this records
/// the new window's handle on the session, so focus and teardown can find it.
pub async fn open(session: &Session) -> Result<(), String> {
    let work_dir = PathBuf::from(&session.work_dir);
    match session.terminal_host {
        TerminalHost::Vscode => crate::editor::open_vscode(&work_dir),
        TerminalHost::TerminalApp => {
            if !TerminalHost::TerminalApp.supported_here() {
                return Err("Terminal.app sessions are only available on macOS".into());
            }
            let (program, args) =
                session_argv(session.agent, &session.color, &session.session_title, launch_options(&session.repo));
            let handle = crate::terminal_app::launch(
                &work_dir,
                &shell_command(&program, &args),
                &terminal_title(session),
                &session.color,
            )
            .await?;
            tracing::info!(window_id = handle.window_id, tty = %handle.tty, "opened the session in Terminal");
            // Re-read the record: it may have changed while the launch waited on
            // Terminal (or on the user answering its Automation prompt).
            let mut current = crate::sessions::get(&session.id).unwrap_or_else(|| session.clone());
            current.terminal_app_window = Some(handle);
            crate::sessions::save(&current).map_err(|e| format!("could not record the Terminal window: {e}"))
        }
        TerminalHost::Cmux => {
            if !TerminalHost::Cmux.supported_here() {
                return Err("cmux sessions are only available on macOS".into());
            }
            // The bare agent name, not the resolved path: cmux's per-surface
            // shims on the terminal's PATH (its Claude wrapper adds cmux's own
            // hooks) then catch it, and cmux decides which binary runs.
            let (_, args) =
                session_argv(session.agent, &session.color, &session.session_title, launch_options(&session.repo));
            let layout = crate::app_settings::terminal_layout();
            let handle = crate::cmux::launch(
                &work_dir,
                &shell_command(session.agent.tool(), &args),
                &terminal_title(session),
                &session.color,
                layout,
                &cmux_siblings(session, layout),
            )
            .await?;
            tracing::info!(workspace = %handle.workspace_id, window = %handle.window_id, layout = ?layout, "opened the session in cmux");
            let mut current = crate::sessions::get(&session.id).unwrap_or_else(|| session.clone());
            current.cmux_workspace = Some(handle);
            crate::sessions::save(&current).map_err(|e| format!("could not record the cmux workspace: {e}"))
        }
        TerminalHost::WindowsTerminal => {
            if !TerminalHost::WindowsTerminal.supported_here() {
                return Err("Windows Terminal sessions are only available on Windows".into());
            }
            // The resolved agent path, run directly by the tab. The session
            // title reaches `wt` both as the tab title and as `--name`, so it is
            // made safe for `wt` once, before either is built.
            let mut safe = session.clone();
            safe.session_title = crate::windows_terminal::user_text(&session.session_title);
            let (program, args) = session_argv(safe.agent, &safe.color, &safe.session_title, launch_options(&safe.repo));
            let args: Vec<String> = args.into_iter().map(|(a, _)| a).collect();
            let layout = crate::app_settings::terminal_layout();
            let window = crate::windows_terminal::window_name(layout, &session.id, &session.repo);
            let handle = crate::windows_terminal::launch(
                &work_dir,
                &program,
                &args,
                &terminal_title(&safe),
                &session.color,
                &window,
            )
            .await?;
            tracing::info!(title = %handle.title, window = %window, layout = ?layout, "opened the session in Windows Terminal");
            let mut current = crate::sessions::get(&session.id).unwrap_or_else(|| session.clone());
            current.windows_terminal_tab = Some(handle);
            crate::sessions::save(&current).map_err(|e| format!("could not record the Windows Terminal tab: {e}"))
        }
    }
}

/// The recorded cmux workspaces whose window a new workspace of `session` may
/// join under `layout`: the repo's other cmux sessions for `per-repo`, every
/// cmux session for `tabs`, none for `windows`.
fn cmux_siblings(session: &Session, layout: TerminalLayout) -> Vec<crate::cmux::CmuxWorkspace> {
    if layout == TerminalLayout::Windows {
        return Vec::new();
    }
    crate::sessions::load_all()
        .into_iter()
        .filter(|s| s.id != session.id && s.terminal_host == TerminalHost::Cmux)
        .filter(|s| layout == TerminalLayout::Tabs || s.repo == session.repo)
        .filter_map(|s| s.cmux_workspace)
        .collect()
}

/// Record the session's cmux workspace as it is now, when it moved to another
/// window since it was recorded.
fn note_cmux_window(session: &Session, current: &crate::cmux::CmuxWorkspace) {
    if session.cmux_workspace.as_ref() == Some(current) {
        return;
    }
    if let Some(mut s) = crate::sessions::get(&session.id) {
        s.cmux_workspace = Some(current.clone());
        if let Err(e) = crate::sessions::save(&s) {
            tracing::warn!(error = %e, "could not record the cmux workspace's new window");
        }
    }
}

/// Bring the session up when its worktree is reopened. VS Code is simply asked
/// to open the folder again (it brings an existing window forward itself);
/// Terminal focuses the session's window and opens a new one only when that
/// window is gone — never a second window next to a live one, which would run
/// a second agent in the same worktree.
pub async fn reopen(session: &Session) -> Result<(), String> {
    match session.terminal_host {
        TerminalHost::Vscode => crate::editor::open_vscode(Path::new(&session.work_dir)),
        TerminalHost::TerminalApp | TerminalHost::Cmux | TerminalHost::WindowsTerminal => focus_or_open(session).await,
    }
}

/// Focus the session's window if it is open, otherwise open one.
pub async fn focus_or_open(session: &Session) -> Result<(), String> {
    match session.terminal_host {
        TerminalHost::Vscode => crate::editor::focus_or_open(Path::new(&session.work_dir)).await,
        TerminalHost::TerminalApp => {
            if let Some(handle) = &session.terminal_app_window {
                // A probe we can't run (no Automation grant) is an error, not
                // "absent": launching would fail the same way, and if it didn't
                // it would start a second agent next to the live one.
                if crate::terminal_app::focus(handle).await? {
                    return Ok(());
                }
            }
            open(session).await
        }
        TerminalHost::Cmux => {
            if let Some(handle) = &session.cmux_workspace {
                // As for Terminal: a lookup we can't make is an error, never
                // "absent", which would start a second agent next to a live one.
                if let Some(current) = crate::cmux::focus(handle).await.map_err(|e| e.message())? {
                    note_cmux_window(session, &current);
                    return Ok(());
                }
            }
            open(session).await
        }
        TerminalHost::WindowsTerminal => {
            if let Some(handle) = &session.windows_terminal_tab {
                // As for Terminal: a lookup we can't make is an error, never
                // "absent", which would start a second agent next to a live one.
                if crate::windows_terminal::focus(handle).await? {
                    return Ok(());
                }
            }
            open(session).await
        }
    }
}

/// Whether the session's window is (or, when we can't look, appears to be)
/// open. Without the grant to look, falls back to the permission-free check
/// that something is running inside the worktree.
pub async fn window_open(session: &Session) -> bool {
    let work_dir = PathBuf::from(&session.work_dir);
    match session.terminal_host {
        TerminalHost::Vscode => crate::editor::editor_window_open(&work_dir).await,
        TerminalHost::TerminalApp => match &session.terminal_app_window {
            None => false,
            Some(handle) => match crate::terminal_app::probe(handle).await {
                Ok(open) => open,
                Err(e) => {
                    tracing::warn!(error = %e, "could not probe the Terminal window; checking the worktree instead");
                    crate::editor::worktree_in_use(&work_dir).await
                }
            },
        },
        TerminalHost::Cmux => match &session.cmux_workspace {
            None => false,
            Some(handle) => match crate::cmux::locate(handle).await {
                Ok(current) => current.is_some(),
                Err(e) => {
                    tracing::warn!(error = %e, "could not look up the cmux workspace; checking the worktree instead");
                    crate::editor::worktree_in_use(&work_dir).await
                }
            },
        },
        TerminalHost::WindowsTerminal => match &session.windows_terminal_tab {
            None => false,
            // A lookup that fails counts as open: `worktree_in_use` can't tell
            // on Windows, and asking before a switch is the safe side.
            Some(handle) => crate::windows_terminal::probe(handle).await.unwrap_or_else(|e| {
                tracing::warn!(error = %e, "could not look up the Windows Terminal tab");
                true
            }),
        },
    }
}

/// Close the session's window and confirm it is gone (see
/// [`crate::editor::close_window_and_wait`] for why confirmation matters).
pub async fn close_and_wait(session: &Session) -> WindowClose {
    let work_dir = PathBuf::from(&session.work_dir);
    match session.terminal_host {
        TerminalHost::Vscode => crate::editor::close_window_and_wait(&work_dir).await,
        TerminalHost::TerminalApp => match &session.terminal_app_window {
            None => WindowClose::Closed,
            Some(handle) => crate::terminal_app::close_and_wait(handle, &work_dir).await,
        },
        TerminalHost::Cmux => match &session.cmux_workspace {
            None => WindowClose::Closed,
            Some(handle) => crate::cmux::close_and_wait(handle, &work_dir).await,
        },
        TerminalHost::WindowsTerminal => match &session.windows_terminal_tab {
            None => WindowClose::Closed,
            Some(handle) => crate::windows_terminal::close_and_wait(handle).await,
        },
    }
}

/// Why a session's window couldn't be closed, worded for `action` ("tear
/// down", "restart the session"), and the grant that would let mAIestro Code
/// close it itself (`None` when the grant is there and the close just didn't
/// take). `None` overall when the window is closed.
pub fn blocked(terminal_host: TerminalHost, close: WindowClose, action: &str) -> Option<(String, Option<Permission>)> {
    let app = terminal_host.app_name();
    let noun = terminal_host.window_noun();
    match close {
        WindowClose::Closed => None,
        WindowClose::InUse => Some((
            format!(
                "I couldn't {action} because the {app} {noun} is still open.\n\nYou have two options: \
                 close the {noun} yourself, or {} so it can close the {noun} for you.",
                terminal_host.grant_phrase()
            ),
            terminal_host.permission(),
        )),
        WindowClose::StillOpen => Some((
            format!("I couldn't {action} because the {app} {noun} is still open. Close its {noun}, then try again."),
            None,
        )),
    }
}

// ── Switching a repo's terminal host ────────────────────────────────────────────

/// Result of [`repo_set_terminal_host`]. Tagged (`status`) like teardown's.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SetTerminalHostOutcome {
    /// Saved; `moved` existing sessions now run in the new terminal host.
    Done { moved: usize },
    /// Some of the repo's sessions have a window open. Switching closes them,
    /// ending their agents, so ask first; `open` names those sessions.
    NeedsConfirmation { open: Vec<String> },
    /// A window couldn't be closed. Nothing was changed.
    BlockedByEditor { message: String, permission: Option<Permission> },
}

/// Switch a repo's terminal host (`None` = the global default) and move every
/// existing session of the repo over with it, so the repo never runs sessions
/// in two apps at once. See [`switch_sessions`] for the confirm/close/move
/// sequence. Emits `repo-settings-changed` so an open Settings form re-reads.
#[tauri::command]
#[tracing::instrument(skip_all, fields(repo = %repo))]
pub async fn repo_set_terminal_host(
    app: tauri::AppHandle,
    repo: String,
    terminal_host: Option<TerminalHost>,
    confirmed: bool,
) -> Result<SetTerminalHostOutcome, String> {
    crate::log_invoke!("repo_set_terminal_host", repo = %repo, terminal_host = ?terminal_host, confirmed);
    let target = crate::repo_settings::resolve_terminal_host(terminal_host);
    let sessions: Vec<Session> = crate::sessions::load_all().into_iter().filter(|s| s.repo == repo).collect();
    let outcome =
        switch_sessions(sessions, target, confirmed, || crate::repo_settings::set_terminal_host(&repo, terminal_host))
            .await?;
    if matches!(outcome, SetTerminalHostOutcome::Done { .. }) {
        use tauri::Emitter;
        let _ = app.emit("repo-settings-changed", &repo);
    }
    Ok(outcome)
}

/// Switch the global default terminal host (`None` = its schema default) and
/// move the existing sessions of every repo that follows the default (its own
/// `terminal_host` is `null`) over with it. Same sequence as
/// [`repo_set_terminal_host`]. The Settings form's autosave never writes this
/// setting, so the sessions can't be left behind.
#[tauri::command]
pub async fn app_set_terminal_host(
    terminal_host: Option<TerminalHost>,
    confirmed: bool,
) -> Result<SetTerminalHostOutcome, String> {
    crate::log_invoke!("app_set_terminal_host", terminal_host = ?terminal_host, confirmed);
    let target = terminal_host.unwrap_or_else(crate::app_settings::terminal_host_schema_default);
    // A repo whose settings can't be read is left alone rather than guessed at.
    let follows_default = |repo: &str| {
        crate::repo_settings::repo_settings_get(repo.to_string()).is_ok_and(|s| s.terminal_host.is_none())
    };
    let sessions: Vec<Session> = crate::sessions::load_all().into_iter().filter(|s| follows_default(&s.repo)).collect();
    switch_sessions(sessions, target, confirmed, || {
        crate::app_settings::update(|s| s.terminal_host = terminal_host).map_err(|e| e.to_string())
    })
    .await
}

/// Move `sessions` (those not already there) to `target` and `save` the
/// setting that put them there. Moving a session closes its window — ending its
/// agent; the conversation doesn't carry over — so when any is open this first
/// returns `NeedsConfirmation` and changes nothing until called again with
/// `confirmed`. All windows are closed (and confirmed gone) **before** anything
/// is written: one that won't close blocks the switch and leaves the setting
/// and every session as they were. A moved session's next open launches its
/// agent in the new terminal host. A workspace still being created refuses the
/// switch until it's ready.
async fn switch_sessions(
    sessions: Vec<Session>,
    target: TerminalHost,
    confirmed: bool,
    save: impl FnOnce() -> Result<(), String>,
) -> Result<SetTerminalHostOutcome, String> {
    if !target.supported_here() {
        return Err(format!("{} isn't available on this computer", target.app_name()));
    }
    let moving: Vec<Session> = sessions.into_iter().filter(|s| s.terminal_host != target).collect();
    if moving.iter().any(|s| crate::status::is_creating(&s.id)) {
        return Err("A workspace is still being created. Try again once it's ready.".into());
    }
    if !confirmed {
        let mut open = Vec::new();
        for s in &moving {
            if window_open(s).await {
                open.push(s.session_title.clone());
            }
        }
        if !open.is_empty() {
            return Ok(SetTerminalHostOutcome::NeedsConfirmation { open });
        }
    }
    for s in &moving {
        let close = close_and_wait(s).await;
        if let Some((message, permission)) = blocked(s.terminal_host, close, "switch the terminal host") {
            return Ok(SetTerminalHostOutcome::BlockedByEditor { message, permission });
        }
    }
    save()?;
    let moved = moving.len();
    for s in moving {
        move_session(s, target).await;
    }
    tracing::info!(terminal_host = %target, moved, "switched the terminal host");
    Ok(SetTerminalHostOutcome::Done { moved })
}

/// Move one session, whose window is already closed, to `target`: record it,
/// forget the old window, and swap the generated VS Code files in or out.
async fn move_session(mut session: Session, target: TerminalHost) {
    let work_dir = PathBuf::from(&session.work_dir);
    let from = session.terminal_host;
    session.terminal_host = target;
    session.terminal_app_window = None;
    session.cmux_workspace = None;
    session.windows_terminal_tab = None;
    // The window that was running the old agent is gone.
    session.editor_agent = None;
    if let Err(e) = crate::sessions::save(&session) {
        tracing::warn!(session = %session.id, error = %e, "could not record the session's new terminal host");
        return;
    }
    // Its agent ended with the window; the next open's hooks report afresh.
    crate::status::remove(&session.id);
    if target == TerminalHost::Vscode {
        crate::spawn::refresh_vscode_files(&work_dir, &session.id);
    } else if from == TerminalHost::Vscode {
        crate::editor::remove_session_task(&work_dir).await;
    }
    tracing::info!(session = %session.id, from = %from, to = %target, "moved the session to the repo's terminal host");
}

/// The terminal host a repo whose own `terminal_host` is `null` uses (the
/// global default, else its schema default), so the popover can pick the
/// repo's open button without a hardcoded copy of the default.
#[tauri::command]
pub fn terminal_host_default() -> TerminalHost {
    crate::log_invoke_debug!("terminal_host_default");
    crate::repo_settings::resolve_terminal_host(None)
}

/// Open System Settings → Privacy & Security → Automation, where the user lets
/// mAIestro Code control Terminal. Triggered only by an explicit user click.
/// macOS only, like `editor::open_accessibility_settings`.
#[tauri::command]
pub fn open_automation_settings() {
    crate::log_invoke!("open_automation_settings");
    #[cfg(not(target_os = "windows"))]
    let _ = crate::tools::spawn_reaped(
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Automation"),
    );
    #[cfg(target_os = "windows")]
    tracing::warn!("open_automation_settings called on Windows, which has no Automation grant");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_values_match_the_settings_schema() {
        assert_eq!(serde_json::to_value(TerminalHost::Vscode).unwrap(), "vscode");
        assert_eq!(serde_json::to_value(TerminalHost::TerminalApp).unwrap(), "terminal_app");
        assert_eq!(TerminalHost::TerminalApp.to_string(), "terminal_app");
        let schema = crate::repo_settings::repo_settings_schema();
        let values: Vec<String> = schema["properties"]["terminal_host"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        assert_eq!(values, ["vscode", "terminal_app", "cmux", "windows_terminal"]);
        assert_eq!(serde_json::to_value(TerminalHost::Cmux).unwrap(), "cmux");
        assert_eq!(TerminalHost::Cmux.to_string(), "cmux");
        assert_eq!(serde_json::to_value(TerminalHost::WindowsTerminal).unwrap(), "windows_terminal");
        assert_eq!(TerminalHost::WindowsTerminal.to_string(), "windows_terminal");
        let global = crate::app_settings::schema_value()["properties"]["terminal_host"]["enum"].clone();
        assert_eq!(global, serde_json::json!(["vscode", "terminal_app", "cmux", "windows_terminal", null]));
    }

    #[test]
    fn layout_values_match_the_settings_schema() {
        let schema = crate::app_settings::schema_value();
        let values: Vec<String> = schema["properties"]["terminal_layout"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        let ours: Vec<String> = [TerminalLayout::Windows, TerminalLayout::PerRepo, TerminalLayout::Tabs]
            .iter()
            .map(|l| serde_json::to_value(l).unwrap().as_str().unwrap().to_string())
            .collect();
        assert_eq!(values, ours);
        assert_eq!(crate::app_settings::schema_value()["properties"]["terminal_layout"]["default"], "per-repo");
    }

    /// Which recorded workspaces a new cmux session may share a window with.
    #[test]
    fn cmux_siblings_follow_the_layout() {
        let _home = crate::testutil::TempHome::new();
        let mk = |id: &str, repo: &str, host: &str, ws: Option<&str>| -> Session {
            let mut v = serde_json::json!({
                "id": id, "repo": repo, "issue_number": 1, "issue_url": "u", "branch": "b",
                "work_dir": "/w", "cloned_repo_dir": "/c", "session_title": "t", "color": "#ca8a04", "emoji": "🍋",
                "terminal_host": host,
            });
            if let Some(ws) = ws {
                v["cmux_workspace"] = serde_json::json!({ "workspace_id": ws, "window_id": "win" });
            }
            serde_json::from_value(v).unwrap()
        };
        let me = mk("1-me", "acme/widgets", "cmux", None);
        for s in [
            me.clone(),
            mk("2-same-repo", "acme/widgets", "cmux", Some("ws-2")),
            mk("3-other-repo", "acme/gadgets", "cmux", Some("ws-3")),
            mk("4-terminal", "acme/widgets", "terminal_app", None),
            mk("5-not-opened", "acme/widgets", "cmux", None),
        ] {
            crate::sessions::save(&s).unwrap();
        }
        let ids = |layout| {
            let mut v: Vec<String> = cmux_siblings(&me, layout).into_iter().map(|w| w.workspace_id).collect();
            v.sort();
            v
        };
        assert!(ids(TerminalLayout::Windows).is_empty());
        assert_eq!(ids(TerminalLayout::PerRepo), ["ws-2"]);
        assert_eq!(ids(TerminalLayout::Tabs), ["ws-2", "ws-3"]);
    }

    #[test]
    fn terminal_title_is_repo_then_session_title() {
        let s: Session = serde_json::from_value(serde_json::json!({
            "id": "243-x", "repo": "acme/widgets", "issue_number": 243, "issue_url": "u", "branch": "b",
            "work_dir": "/w", "cloned_repo_dir": "/c", "session_title": "🍋 #243 — Terminal host", "color": "#ca8a04", "emoji": "🍋",
        }))
        .unwrap();
        assert_eq!(terminal_title(&s), "widgets: 🍋 #243 — Terminal host");
        assert_eq!(s.terminal_host, TerminalHost::Vscode, "records without a terminal host load as VS Code");
    }

    /// Moving a session swaps the generated VS Code files out and back in, and
    /// forgets the old window and any pending agent restart.
    #[tokio::test]
    async fn moving_a_session_swaps_its_vscode_task() {
        let _home = crate::testutil::TempHome::new();
        let wt = tempfile::tempdir().unwrap();
        let mut s: Session = serde_json::from_value(serde_json::json!({
            "id": "243-x", "repo": "acme/widgets", "issue_number": 243, "issue_url": "u", "branch": "b",
            "work_dir": wt.path().display().to_string(), "cloned_repo_dir": "/c", "session_title": "t",
            "color": "#ca8a04", "emoji": "🍋", "editor_agent": "codex",
        }))
        .unwrap();
        crate::sessions::save(&s).unwrap();
        crate::spawn::refresh_vscode_files(wt.path(), &s.id);
        let tasks = wt.path().join(".vscode/tasks.json");
        assert!(tasks.exists());

        move_session(s.clone(), TerminalHost::TerminalApp).await;
        let moved = crate::sessions::get(&s.id).unwrap();
        assert_eq!(moved.terminal_host, TerminalHost::TerminalApp);
        assert_eq!(moved.editor_agent, None);
        assert!(!tasks.exists(), "a Terminal session keeps no folder-open task");
        assert!(wt.path().join(".vscode/settings.json").exists(), "the theming stays");

        s = moved;
        s.terminal_app_window = Some(crate::terminal_app::TerminalAppWindow { window_id: 7, tty: "/dev/ttys007".into() });
        crate::sessions::save(&s).unwrap();
        move_session(s.clone(), TerminalHost::Vscode).await;
        let back = crate::sessions::get(&s.id).unwrap();
        assert_eq!(back.terminal_host, TerminalHost::Vscode);
        assert_eq!(back.terminal_app_window, None);
        assert!(tasks.exists(), "back in VS Code, the folder-open task returns");
    }

    #[test]
    fn blocked_names_the_host_and_its_grant() {
        assert!(blocked(TerminalHost::Vscode, WindowClose::Closed, "tear down").is_none());
        let (msg, perm) = blocked(TerminalHost::TerminalApp, WindowClose::InUse, "tear down").unwrap();
        assert!(msg.contains("Terminal window"), "{msg}");
        assert!(msg.contains("Automation"), "{msg}");
        assert_eq!(perm, Some(Permission::Automation));
        let (msg, perm) = blocked(TerminalHost::Vscode, WindowClose::InUse, "tear down").unwrap();
        assert!(msg.contains("Visual Studio Code window") && msg.contains("Accessibility"), "{msg}");
        assert_eq!(perm, Some(Permission::Accessibility));
        let (_, perm) = blocked(TerminalHost::Vscode, WindowClose::StillOpen, "tear down").unwrap();
        assert_eq!(perm, None);
        let (msg, perm) = blocked(TerminalHost::Cmux, WindowClose::InUse, "tear down").unwrap();
        assert!(msg.contains("cmux workspace") && msg.contains("Socket Control Mode"), "{msg}");
        assert_eq!(perm, None, "cmux's gate isn't a macOS privacy grant");
        let (msg, perm) = blocked(TerminalHost::WindowsTerminal, WindowClose::StillOpen, "tear down").unwrap();
        assert!(msg.contains("Windows Terminal tab is still open") && msg.contains("Close its tab"), "{msg}");
        assert_eq!(perm, None);
    }

    #[test]
    fn windows_terminal_is_windows_only_and_names_tabs() {
        assert_eq!(TerminalHost::WindowsTerminal.supported_here(), cfg!(target_os = "windows"));
        assert_eq!(TerminalHost::WindowsTerminal.window_noun(), "tab");
        assert_eq!(TerminalHost::WindowsTerminal.permission(), None);
    }

    /// Moving a session off Windows Terminal forgets its tab.
    #[tokio::test]
    async fn moving_a_session_forgets_its_windows_terminal_tab() {
        let _home = crate::testutil::TempHome::new();
        let wt = tempfile::tempdir().unwrap();
        let s: Session = serde_json::from_value(serde_json::json!({
            "id": "242-x", "repo": "acme/widgets", "issue_number": 242, "issue_url": "u", "branch": "b",
            "work_dir": wt.path().display().to_string(), "cloned_repo_dir": "/c", "session_title": "t",
            "color": "#ca8a04", "emoji": "🍋", "terminal_host": "windows_terminal",
            "windows_terminal_tab": { "title": "widgets: t" },
        }))
        .unwrap();
        assert_eq!(s.windows_terminal_tab.as_ref().map(|t| t.title.as_str()), Some("widgets: t"));
        crate::sessions::save(&s).unwrap();
        move_session(s.clone(), TerminalHost::Vscode).await;
        let moved = crate::sessions::get(&s.id).unwrap();
        assert_eq!(moved.terminal_host, TerminalHost::Vscode);
        assert_eq!(moved.windows_terminal_tab, None);
        assert!(wt.path().join(".vscode/tasks.json").exists(), "in VS Code, the folder-open task is written");
    }
}

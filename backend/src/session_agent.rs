//! Switching an existing session's agent (Claude ⇄ Codex) without re-spawning
//! its worktree (issue #186), and the VS Code restart that applies it.
//!
//! The agent session only changes when VS Code opens the folder fresh — the
//! generated `.vscode/tasks.json` runs on `folderOpen` — so a switch rewrites the
//! worktree's launch files and hooks, and when the worktree's window is already
//! open it records the agent that window is still running
//! (`Session::editor_agent`) until mAIestro Code restarts it. The worktree's
//! theme, title, and branch never change; the old agent's conversation does not
//! carry over.

use std::path::PathBuf;

use crate::agent::Agent;
use crate::editor::{close_window_and_wait, editor_window_open, focus_or_open, open_vscode, WindowClose};
use crate::sessions::Session;

#[derive(serde::Serialize)]
pub struct SetAgentOutcome {
    /// True when the worktree's VS Code window is still running the other agent,
    /// so the UI should offer a restart.
    pub restart_needed: bool,
    /// The agent that window is running, when `restart_needed`.
    pub editor_agent: Option<Agent>,
}

#[derive(serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OpenOutcome {
    /// The window was focused or opened.
    Opened,
    /// The session was switched while its window was open, and that window is
    /// still running `editor_agent`: focusing it would show the old agent, so the
    /// UI asks to restart instead.
    RestartRequired { agent: Agent, editor_agent: Agent },
}

#[derive(serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RestartOutcome {
    Restarted,
    /// The window couldn't be closed — same shape and meaning as teardown's
    /// `BlockedByEditor`, so the UI renders it the same way.
    BlockedByEditor { message: String, accessibility: bool },
}

/// The record after switching `s` to `agent`. With the window open, it keeps
/// running whatever it was already running (the old agent, or — if an earlier
/// switch was never applied — the one before that); a switch back to that agent
/// therefore cancels the pending restart. With no window open, nothing is
/// running, so the next open simply launches `agent`.
fn switched(mut s: Session, agent: Agent, window_open: bool) -> Session {
    let running = window_open.then(|| s.editor_agent.unwrap_or(s.agent));
    s.agent = agent;
    s.editor_agent = running.filter(|r| *r != agent);
    s
}

/// Switch a session to `agent`: install the new agent's status hooks (and drop
/// our Claude hooks when leaving Claude), record the switch, and regenerate the
/// `.vscode` files from the record so the folder-open task launches the new
/// agent. Never re-themes the worktree. A no-op for the agent it already runs.
#[tauri::command]
#[tracing::instrument(skip_all, fields(session = %session_id))]
pub async fn session_set_agent(session_id: String, agent: Agent) -> Result<SetAgentOutcome, String> {
    crate::log_invoke!("session_set_agent", agent = %agent);
    let session = crate::sessions::get(&session_id).ok_or_else(|| format!("session not found: {session_id}"))?;
    if session.agent != agent {
        let work_dir = PathBuf::from(&session.work_dir);
        let from = session.agent;
        let window_open = editor_window_open(&work_dir).await;

        crate::hooks::write_session_hooks(&work_dir, &session_id, agent).await?;
        if from == Agent::Claude {
            crate::hooks::remove_claude_hooks(&work_dir, &session_id);
        }
        let session = switched(session, agent, window_open);
        crate::sessions::save(&session).map_err(|e| e.to_string())?;
        crate::spawn::refresh_vscode_files(&work_dir, &session_id);
        tracing::info!(from = %from, to = %agent, window_open, restart_pending = session.restart_pending(), "switched session agent");
        return Ok(outcome(&session));
    }
    Ok(outcome(&session))
}

fn outcome(s: &Session) -> SetAgentOutcome {
    SetAgentOutcome { restart_needed: s.restart_pending(), editor_agent: s.editor_agent.filter(|_| s.restart_pending()) }
}

/// Open a session's worktree in VS Code: focus its window if open, else launch
/// one. While a switch is pending and the window is still open, returns
/// `RestartRequired` instead of focusing the old agent. A pending switch whose
/// window has since closed just clears — the fresh window runs the new agent.
///
/// `focus_existing` skips that check and just focuses (or opens) the window,
/// leaving any pending switch in place — for the "couldn't restart" prompt,
/// where the user wants the old window in front to close it themselves.
#[tauri::command]
pub async fn session_open_in_editor(session_id: String, focus_existing: Option<bool>) -> Result<OpenOutcome, String> {
    let focus_existing = focus_existing.unwrap_or(false);
    crate::log_invoke!("session_open_in_editor", session = %session_id, focus_existing);
    let mut session = crate::sessions::get(&session_id).ok_or_else(|| format!("session not found: {session_id}"))?;
    let work_dir = PathBuf::from(&session.work_dir);
    if focus_existing {
        focus_or_open(&work_dir).await?;
        return Ok(OpenOutcome::Opened);
    }
    if let Some(editor_agent) = session.editor_agent {
        if session.restart_pending() && editor_window_open(&work_dir).await {
            return Ok(OpenOutcome::RestartRequired { agent: session.agent, editor_agent });
        }
        session.editor_agent = None;
        crate::sessions::save(&session).map_err(|e| e.to_string())?;
        open_vscode(&work_dir)?;
        return Ok(OpenOutcome::Opened);
    }
    focus_or_open(&work_dir).await?;
    Ok(OpenOutcome::Opened)
}

/// Restart a session's VS Code window so it launches the recorded agent: close
/// the window and confirm it's gone (as teardown does), clear the old agent's
/// status, then open the worktree fresh.
#[tauri::command]
#[tracing::instrument(skip_all, fields(session = %session_id))]
pub async fn session_restart_editor(session_id: String) -> Result<RestartOutcome, String> {
    crate::log_invoke!("session_restart_editor");
    let mut session = crate::sessions::get(&session_id).ok_or_else(|| format!("session not found: {session_id}"))?;
    let work_dir = PathBuf::from(&session.work_dir);
    match close_window_and_wait(&work_dir).await {
        WindowClose::Closed => {}
        WindowClose::InUse => {
            return Ok(RestartOutcome::BlockedByEditor {
                message: "I couldn't restart VS Code because I can't close its window.\n\nClose the \
                          window yourself and open it again, or enable Accessibility for mAIestro \
                          Code so it can restart the window for you."
                    .to_string(),
                accessibility: true,
            });
        }
        WindowClose::StillOpen => {
            return Ok(RestartOutcome::BlockedByEditor {
                message: "The Visual Studio Code window didn't close. Close it, then open it again."
                    .to_string(),
                accessibility: false,
            });
        }
    }
    // The old agent's pill would otherwise linger until the new agent's first hook.
    crate::status::remove(&session_id);
    session.editor_agent = None;
    crate::sessions::save(&session).map_err(|e| e.to_string())?;
    open_vscode(&work_dir)?;
    tracing::info!(agent = %session.agent, "restarted VS Code for the session's agent");
    Ok(RestartOutcome::Restarted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(agent: Agent) -> Session {
        serde_json::from_value(serde_json::json!({
            "id": "186-x", "repo": "a/b", "issue_number": 186, "issue_url": "u", "branch": "feature/186-x",
            "work_dir": "/w", "cloned_repo_dir": "/c", "session_title": "t", "color": "#c46686", "emoji": "e",
            "agent": agent,
        }))
        .unwrap()
    }

    /// With the window closed there's nothing to restart: the next open launches
    /// the new agent.
    #[test]
    fn switch_with_no_window_needs_no_restart() {
        let s = switched(session(Agent::Claude), Agent::Codex, false);
        assert_eq!(s.agent, Agent::Codex);
        assert_eq!(s.editor_agent, None);
        assert!(!s.restart_pending());
    }

    /// With the window open it keeps running the old agent until restarted, and
    /// switching back before restarting cancels the pending restart.
    #[test]
    fn switch_with_window_open_is_pending_until_switched_back() {
        let s = switched(session(Agent::Claude), Agent::Codex, true);
        assert_eq!(s.editor_agent, Some(Agent::Claude));
        assert!(s.restart_pending());

        let back = switched(s, Agent::Claude, true);
        assert_eq!(back.agent, Agent::Claude);
        assert_eq!(back.editor_agent, None);
        assert!(!back.restart_pending());
    }

    /// The switch rewrites the launch files and hooks for the new agent from the
    /// session record, keeping the worktree's theme; leaving Claude drops our
    /// Claude hooks.
    #[tokio::test]
    async fn set_agent_rewrites_the_worktree_for_the_new_agent() {
        let _home = crate::testutil::TempHome::new();
        let wt = tempfile::tempdir().unwrap();
        let mut s = session(Agent::Codex);
        s.work_dir = wt.path().display().to_string();
        crate::sessions::save(&s).unwrap();

        let out = session_set_agent(s.id.clone(), Agent::Claude).await.unwrap();
        assert!(!out.restart_needed);
        assert_eq!(crate::sessions::get(&s.id).unwrap().agent, Agent::Claude);
        let hooks = std::fs::read_to_string(wt.path().join(".claude/settings.local.json")).unwrap();
        assert!(hooks.contains("--workspace '186-x'"), "{hooks}");
        let read = |f: &str| -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(wt.path().join(".vscode").join(f)).unwrap()).unwrap()
        };
        assert_eq!(read("tasks.json")["tasks"][0]["label"], "Start Claude");
        assert_eq!(read("settings.json")["workbench.colorCustomizations"]["titleBar.activeBackground"], "#c46686");

        session_set_agent(s.id.clone(), Agent::Codex).await.unwrap();
        assert!(!wt.path().join(".claude/settings.local.json").exists());
        assert_eq!(read("tasks.json")["tasks"][0]["label"], "Start Codex");
    }
}

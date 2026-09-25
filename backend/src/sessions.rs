//! Spawned-session registry: one JSON file per session under
//! `~/.maiestro/sessions/<id>.json`. Records the details of a launched
//! workspace (worktree path, branch, theming, originating issue) so mAIestro Code
//! can reason about what's in flight — e.g. which title-bar colors are taken —
//! without scraping each worktree's `.vscode/settings.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::agent::Agent;
use crate::repo_settings::HideState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique, human-readable id (the workspace name, e.g. "488-wider-window").
    pub id: String,
    /// "owner/name" of the originating repo.
    pub repo: String,
    pub issue_number: u64,
    pub issue_url: String,
    pub branch: String,
    /// The repo's default branch the worktree was created from (teardown
    /// compares against `origin/<default_branch>`). Older records default to "main".
    #[serde(default = "default_branch")]
    pub default_branch: String,
    /// Absolute path to the spawned worktree.
    pub work_dir: String,
    /// Absolute path to the source cloned repo the worktree was created from.
    pub cloned_repo_dir: String,
    /// Human-facing session name, including the leading emoji.
    pub session_title: String,
    /// Title-bar background color (hex).
    pub color: String,
    pub emoji: String,
    /// The coding agent this session launches (issue #162). Stored at spawn and
    /// read back on reopen, like `color`, so changing the repo's agent later never
    /// switches an existing worktree to the other one — only an explicit
    /// per-session switch does (`session_agent::session_set_agent`, #186).
    /// Records written before the field existed load as Claude.
    #[serde(default)]
    pub agent: Agent,
    /// Set when `agent` was switched while the worktree's VS Code window was
    /// open: the agent that window is still running, until it is restarted (or
    /// closed and reopened). `None` = the window, if any, runs `agent`. A restart
    /// is pending exactly when this is `Some` (see [`Session::restart_pending`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_agent: Option<Agent>,
    /// Hide/snooze state for this work item. `None` = visible. Snooze expiry is
    /// resolved on the frontend at render time.
    #[serde(default)]
    pub hidden: Option<HideState>,
    /// Something mAIestro Code changed in the worktree that the user should know
    /// about — e.g. appending `.agents/hooks.json` to its `.gitignore` (#185).
    /// Shown on the work item until dismissed (`session_dismiss_notice`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

impl Session {
    /// Whether the open VS Code window runs a different agent than the record's,
    /// so it must restart to apply a switch.
    pub fn restart_pending(&self) -> bool {
        self.editor_agent.is_some_and(|a| a != self.agent)
    }
}

fn default_branch() -> String {
    "main".to_string()
}

fn sessions_dir() -> PathBuf {
    crate::paths::maiestro_dir("sessions")
}

fn session_path(id: &str) -> PathBuf {
    sessions_dir().join(format!("{id}.json"))
}

pub fn get(id: &str) -> Option<Session> {
    let data = std::fs::read_to_string(session_path(id)).ok()?;
    serde_json::from_str(&data).ok()
}

pub fn delete(id: &str) -> std::io::Result<()> {
    match std::fs::remove_file(session_path(id)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

pub fn save(session: &Session) -> std::io::Result<()> {
    let dir = sessions_dir();
    std::fs::create_dir_all(&dir)?;
    let data = serde_json::to_string_pretty(session).unwrap();
    crate::paths::write_atomic(&session_path(&session.id), data.as_bytes())
}

pub fn load_all() -> Vec<Session> {
    let Ok(entries) = std::fs::read_dir(sessions_dir()) else { return Vec::new(); };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| {
            let data = std::fs::read_to_string(e.path()).ok()?;
            serde_json::from_str(&data).ok()
        })
        .collect()
}

/// Title-bar colors already claimed by tracked sessions.
pub fn used_colors() -> Vec<String> {
    load_all().into_iter().map(|s| s.color).collect()
}

#[tauri::command]
pub fn sessions_list() -> Vec<Session> {
    crate::log_invoke_debug!("sessions_list");
    load_all()
}

/// Record `notice` on a session so the popover shows it (replacing any earlier
/// one). Best-effort: a missing record or failed write is only logged.
pub fn set_notice(session_id: &str, notice: String) {
    let Some(mut session) = get(session_id) else { return };
    session.notice = Some(notice);
    if let Err(e) = save(&session) {
        tracing::warn!(session = %session_id, error = %e, "couldn't record the session notice");
    }
}

/// Clear a work item's notice once the user dismisses it. No-op if none.
#[tauri::command]
pub fn session_dismiss_notice(session_id: String) -> Result<(), String> {
    crate::log_invoke!("session_dismiss_notice", session_id = %session_id);
    let mut session = get(&session_id).ok_or_else(|| format!("session not found: {session_id}"))?;
    if session.notice.take().is_none() {
        return Ok(());
    }
    save(&session).map_err(|e| e.to_string())
}

/// Set (or clear) a work item's hide/snooze state. `hidden = None` unhides.
#[tauri::command]
pub fn session_set_visibility(session_id: String, hidden: Option<HideState>) -> Result<(), String> {
    crate::log_invoke!("session_set_visibility", session_id = %session_id);
    let mut session = get(&session_id).ok_or_else(|| format!("session not found: {session_id}"))?;
    session.hidden = hidden;
    save(&session).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record written before #162 has no `agent` and still loads, as Claude.
    #[test]
    fn old_record_loads_as_claude() {
        let old = serde_json::json!({
            "id": "1-x", "repo": "a/b", "issue_number": 1, "issue_url": "u", "branch": "feature/1-x",
            "work_dir": "/w", "cloned_repo_dir": "/c", "session_title": "t", "color": "#fff", "emoji": "e"
        });
        let s: Session = serde_json::from_value(old).unwrap();
        assert_eq!(s.agent, Agent::Claude);
        assert_eq!(s.editor_agent, None);
        assert!(!s.restart_pending());
    }
}

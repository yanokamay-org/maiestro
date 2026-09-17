//! Spawned-session registry: one JSON file per session under
//! `~/.maiestro/sessions/<id>.json`. Records the details of a launched
//! workspace (worktree path, branch, theming, originating issue) so mAIestro Code
//! can reason about what's in flight — e.g. which title-bar colors are taken —
//! without scraping each worktree's `.vscode/settings.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
    /// Hide/snooze state for this work item. `None` = visible. Snooze expiry is
    /// resolved on the frontend at render time.
    #[serde(default)]
    pub hidden: Option<HideState>,
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

/// Set (or clear) a work item's hide/snooze state. `hidden = None` unhides.
#[tauri::command]
pub fn session_set_visibility(session_id: String, hidden: Option<HideState>) -> Result<(), String> {
    crate::log_invoke!("session_set_visibility", session_id = %session_id);
    let mut session = get(&session_id).ok_or_else(|| format!("session not found: {session_id}"))?;
    session.hidden = hidden;
    save(&session).map_err(|e| e.to_string())
}

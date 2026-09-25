//! Per-session live status: **busy / needs-you / idle**.
//!
//! mAIestro Code launches the repo's agent (`claude` or `codex`) into VS Code and
//! no longer owns its stdio (see CLAUDE.md → "mAIestro Code launches sessions; it
//! does not host them"), so it can't read working/waiting state from the stream.
//! Instead, each spawned worktree gets agent hooks (written by `hooks.rs`) that
//! invoke this very binary as `maiestro hook <state> --workspace <ws-id>`. The
//! hook reads the agent's event JSON on stdin, writes a small status record to `~/.maiestro/status/<ws-id>.json`,
//! and the backend watches that directory and pushes changes to the popover.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A single session's current status, written by the `hook` helper and watched
/// by the backend. Keyed on disk by `<workspace>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusRecord {
    /// Workspace id (= `Session.id`, e.g. "8-surface-per-session").
    pub workspace: String,
    /// One of: creating | running | busy | needs_you | idle | ended. `creating`
    /// is written by `spawn.rs` while a fresh worktree is being built in the
    /// background (not a Claude hook state) and is cleared once the worktree is
    /// ready, after which Claude's own hooks own the record.
    pub state: String,
    /// Claude session id from the hook payload, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The worktree cwd from the hook payload, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Short human detail for the UI (e.g. "permission: Bash", a tool name, the
    /// notification message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The most recent failed tool call, if any. Unlike `state`/`detail` (which
    /// the next hook overwrites within seconds), this is preserved across writes
    /// until the user dismisses it or starts a new turn — so a failure stays
    /// visible long enough to be read. See `run_hook_cli`'s lifecycle rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<ToolError>,
    /// RFC-3339 timestamp of the transition.
    pub ts: String,
}

/// One failed tool call. Logged on every failure, but only shown in the popover
/// once `surfaced` is true — see the `last_error` lifecycle in `run_hook_cli`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolError {
    /// The tool that failed (from the hook payload's `tool_name`), when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// The error message extracted from the `PostToolUseFailure` payload.
    pub message: String,
    /// RFC-3339 timestamp of the (most recent) failure.
    pub ts: String,
    /// Consecutive failures of this same tool. A second strike (>=
    /// `RETRY_SURFACE_THRESHOLD`) marks the error persistent and surfaces it.
    #[serde(default)]
    pub count: u32,
    /// Whether this error has been promoted to prominent popover display. While
    /// false the failure is logged and tracked but hidden — Claude may still
    /// recover. Promoted on Stop/idle (Claude stopped without recovering) or a
    /// repeated same-tool failure; cleared when a later tool succeeds.
    #[serde(default)]
    pub surfaced: bool,
}

/// Number of consecutive same-tool failures at which a still-pending error is
/// promoted to prominent display (the second strike). Not user-configurable.
const RETRY_SURFACE_THRESHOLD: u32 = 2;

fn status_dir() -> PathBuf {
    crate::paths::maiestro_dir("status")
}

fn status_path(ws: &str) -> PathBuf {
    status_dir().join(format!("{ws}.json"))
}

/// True if `ws` is safe to use as a single status-file component — non-empty, not
/// `.`/`..`, and containing no path separator or NUL. Internally generated ids
/// (`<issue>-<slug>`, slug is `[a-z0-9-]`) always pass; this only rejects crafted
/// input reaching the untrusted boundaries (`run_hook_cli`'s `--workspace` argv and
/// the `clear_session_error` command), which could otherwise make `status_path`
/// escape `~/.maiestro/status/` via `..`/`/` and write attacker-controlled JSON to
/// an arbitrary user-writable path.
fn is_safe_workspace_id(ws: &str) -> bool {
    !ws.is_empty()
        && ws != "."
        && ws != ".."
        && !ws.contains('/')
        && !ws.contains('\\')
        && !ws.contains('\0')
}

// ── Hook CLI (`maiestro hook <state> --workspace <ws-id>`) ──────────────────────

/// Map the CLI state argument + the parsed hook payload into the record's
/// `state` and `detail`. `notification` is the only one that inspects the
/// payload to choose between waiting-on-permission and an idle nudge — both
/// surface as `needs_you`, differing only in `detail`.
fn resolve_state(arg: &str, payload: &serde_json::Value) -> (String, Option<String>) {
    match arg {
        "running" => ("running".into(), None),
        // UserPromptSubmit: a fresh turn. Same `busy` state as a running tool,
        // but its own verb so the helper can clear a stale `last_error`.
        "prompt" => ("busy".into(), None),
        "busy" => {
            // PreToolUse carries the tool name; UserPromptSubmit does not.
            let detail = payload["tool_name"].as_str().map(|t| t.to_string());
            ("busy".into(), detail)
        }
        // A failed tool call. Claude keeps working after it, so the *state* stays
        // `busy`; the failure itself rides on `last_error` (set in run_hook_cli).
        "tool_failed" => {
            let detail = payload["tool_name"].as_str().map(|t| t.to_string());
            ("busy".into(), detail)
        }
        // PostToolUse: a tool *completed successfully*. Reads as `busy` like any
        // other working signal; its own verb (vs `busy`/PreToolUse) lets the
        // helper clear a pending `last_error` — Claude recovered and moved on.
        "tool_ok" => {
            let detail = payload["tool_name"].as_str().map(|t| t.to_string());
            ("busy".into(), detail)
        }
        // Claude's `Notification` carries a `message`. Codex has no such event
        // and fires this verb from `PermissionRequest`, whose payload names the
        // tool instead — so fall back to that for the detail.
        "notification" => {
            let msg = payload["message"].as_str().unwrap_or("").trim();
            let detail = if !msg.is_empty() {
                Some(msg.to_string())
            } else {
                payload["tool_name"]
                    .as_str()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(|t| format!("Permission requested: `{t}`"))
            };
            ("needs_you".into(), detail)
        }
        "idle" => ("idle".into(), None),
        "ended" => ("ended".into(), None),
        // Unknown verb: record it verbatim rather than guessing.
        other => (other.into(), None),
    }
}

/// Decide a workspace's next `last_error` from the incoming event and the carried
/// prior value. Pure (no I/O) so the surfacing rules can be unit-tested.
///
/// The status file is last-write-wins and the next hook lands within seconds, so
/// a failure can't live in `state`. The error is logged on every failure but only
/// *surfaced* (shown in the popover) once it proves to matter — Claude often
/// recovers and works straight past a transient failure (issue #48):
///
/// - `tool_failed` → set/refresh it (pending). A repeated same-tool failure
///   (`count >= RETRY_SURFACE_THRESHOLD`) is persistent → surface.
/// - `tool_ok` → a tool succeeded; clear a still-*pending* error (Claude
///   recovered). A surfaced one stays (user dismisses it).
/// - `idle` → Claude stopped without recovering → surface a pending error.
/// - `prompt`/`running` → new turn/session clears it.
/// - everything else → carry the prior value forward (survives until dismissed).
///
/// `failure` carries the freshly-parsed `(tool, message)` on a `tool_failed`
/// event and is `None` otherwise.
fn next_last_error(
    state_arg: &str,
    prior: Option<ToolError>,
    failure: Option<(Option<String>, String)>,
    ts: &str,
) -> Option<ToolError> {
    match state_arg {
        "tool_failed" => {
            let (tool, message) = failure?;
            // A consecutive failure of the *same* tool bumps the count; a different
            // tool (or first failure) resets to 1.
            let count = prior.filter(|e| e.tool == tool).map_or(1, |e| e.count + 1);
            Some(ToolError {
                tool,
                message,
                ts: ts.to_string(),
                count,
                surfaced: count >= RETRY_SURFACE_THRESHOLD,
            })
        }
        "tool_ok" => prior.filter(|e| e.surfaced),
        "idle" => prior.map(|mut e| {
            e.surfaced = true;
            e
        }),
        "prompt" | "running" => None,
        _ => prior,
    }
}

/// Entry point for `maiestro hook …`, dispatched from `main()` before Tauri
/// starts. Reads the hook payload from stdin, writes the status record, and
/// returns. Deliberately failure-tolerant: a hook must never block or crash the
/// user's Claude session, so every error is swallowed.
pub fn run_hook_cli(args: &[String]) {
    // args = ["<state>", "--workspace", "<ws-id>"] (order-tolerant for the flag).
    // Codex hooks pass only the state: they must be identical for every worktree
    // (see hooks.rs), so the workspace comes from the payload's `cwd` instead.
    let state_arg = args.first().map(|s| s.as_str()).unwrap_or("");
    let flag = args.iter().position(|a| a == "--workspace").and_then(|i| args.get(i + 1)).cloned();
    if state_arg.is_empty() {
        return;
    }

    // Best-effort stdin parse: the agent sends a JSON event, but we still record
    // a status even if it's empty or malformed.
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    let payload: serde_json::Value = serde_json::from_str(&buf).unwrap_or(serde_json::Value::Null);

    let workspace = match flag {
        Some(ws) => ws,
        None => {
            let Some(cwd) = payload["cwd"].as_str() else { return };
            let sessions: Vec<(String, String)> =
                crate::sessions::load_all().into_iter().map(|s| (s.id, s.work_dir)).collect();
            let Some(ws) = workspace_for_cwd(cwd, &sessions) else { return };
            ws
        }
    };
    if !is_safe_workspace_id(&workspace) {
        return;
    }

    let (state, detail) = resolve_state(state_arg, &payload);
    let ts = chrono::Utc::now().to_rfc3339();

    // `last_error` lifecycle (see `next_last_error`). On a failure, hand the raw
    // tool+message to the decision fn (it computes the count and surfacing) and
    // then log the result — surfacing is gated, logging is not.
    let prior = read_record(&workspace).and_then(|r| r.last_error);
    let failure = (state_arg == "tool_failed")
        .then(|| (payload["tool_name"].as_str().map(|t| t.to_string()), extract_error_message(&payload)));
    let last_error = next_last_error(state_arg, prior, failure, &ts);
    if state_arg == "tool_failed" {
        if let Some(err) = &last_error {
            crate::logging::append_line(&format!(
                "session={workspace} tool call failed [{}] (#{}): {}",
                err.tool.as_deref().unwrap_or("?"),
                err.count,
                err.message
            ));
        }
    }

    let record = StatusRecord {
        workspace: workspace.clone(),
        state,
        session_id: payload["session_id"].as_str().map(|s| s.to_string()),
        cwd: payload["cwd"].as_str().map(|s| s.to_string()),
        detail,
        last_error,
        ts,
    };

    let _ = write_record_atomic(&record);
}

/// The workspace whose worktree contains `cwd` — the session's own directory or
/// any folder inside it — from `(id, work_dir)` pairs. The deepest match wins, so
/// a worktree nested under another (unusual, but possible with a custom prefix)
/// resolves to itself. `None` when `cwd` is in no tracked worktree (a Codex
/// session mAIestro Code didn't launch), and the hook then records nothing.
fn workspace_for_cwd(cwd: &str, sessions: &[(String, String)]) -> Option<String> {
    let cwd = cwd.trim_end_matches('/');
    sessions
        .iter()
        .filter(|(_, dir)| {
            let dir = dir.trim_end_matches('/');
            !dir.is_empty() && (cwd == dir || cwd.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/')))
        })
        .max_by_key(|(_, dir)| dir.len())
        .map(|(id, _)| id.clone())
}

/// Pull a human-readable failure message out of a `PostToolUseFailure` payload.
/// The exact field isn't pinned down in the docs, so try the likely ones in
/// order and fall back to a generic message rather than dropping the failure.
fn extract_error_message(payload: &serde_json::Value) -> String {
    let candidates = [
        payload["error"].as_str(),
        payload["tool_response"]["error"].as_str(),
        payload["tool_response"]["stderr"].as_str(),
        payload["message"].as_str(),
    ];
    for c in candidates.into_iter().flatten() {
        let trimmed = c.trim();
        if !trimmed.is_empty() {
            return redact_secrets(trimmed);
        }
    }
    "Tool call failed".to_string()
}

/// Redact credentials that can appear in a tool's error text before it is written
/// to the persistent log or shown in the popover. A failed session command (e.g.
/// `curl https://user:token@host/…`) can put a secret in stderr; this masks the
/// `user:pass@` userinfo of any URL in the message, and bounds the length so a huge
/// error can't bloat the log. Dependency-free (no regex) to keep the hook helper
/// minimal, matching the rest of the hook path.
fn redact_secrets(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with("://") {
            out.push_str("://");
            i += 3;
            // The authority runs until the next path/query/fragment/quote/space.
            let start = i;
            while i < s.len()
                && !matches!(bytes[i], b'/' | b'?' | b'#' | b' ' | b'\t' | b'"' | b'\'')
            {
                i += 1;
            }
            let authority = &s[start..i];
            match authority.rfind('@') {
                Some(at) => {
                    out.push_str("***@");
                    out.push_str(&authority[at + 1..]);
                }
                None => out.push_str(authority),
            }
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out.chars().take(500).collect()
}

/// Read a workspace's current status record from disk, if present and valid.
fn read_record(ws: &str) -> Option<StatusRecord> {
    let data = std::fs::read_to_string(status_path(ws)).ok()?;
    serde_json::from_str(&data).ok()
}

/// Write `~/.maiestro/status/<ws>.json` atomically (temp file + rename) so the
/// watcher never reads a half-written record.
fn write_record_atomic(record: &StatusRecord) -> std::io::Result<()> {
    // Defense in depth: never let a crafted workspace id escape the status dir via
    // the temp/final path, even if a future caller skips the entry-point checks.
    if !is_safe_workspace_id(&record.workspace) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unsafe workspace id",
        ));
    }
    let dir = status_dir();
    std::fs::create_dir_all(&dir)?;
    let data = serde_json::to_string_pretty(record).unwrap_or_default();
    let final_path = status_path(&record.workspace);
    let tmp = dir.join(format!(".{}.tmp", record.workspace));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, &final_path)
}

// ── Backend read side: list, sweep, watch ──────────────────────────────────────

/// All current status records on disk.
fn load_all() -> Vec<StatusRecord> {
    let Ok(entries) = std::fs::read_dir(status_dir()) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| {
            let data = std::fs::read_to_string(e.path()).ok()?;
            serde_json::from_str(&data).ok()
        })
        .collect()
}

/// Remove a workspace's status file (called on teardown). The watcher then
/// emits an `ended` record so the popover clears the row. No-op if absent.
pub fn remove(ws: &str) {
    let _ = std::fs::remove_file(status_path(ws));
}

/// Mark a workspace as `creating` — the worktree is being built in the
/// background (`spawn.rs`). Written synchronously at spawn start, *before* the
/// `Session` row's heavy work, so the popover shows a "Creating…" pill right
/// away. Not a Claude hook state; cleared by `clear_creating` when the worktree
/// is ready or replaced by `write_spawn_error` on failure.
pub fn write_creating(ws: &str) {
    let _ = write_record_atomic(&StatusRecord {
        workspace: ws.to_string(),
        state: "creating".into(),
        session_id: None,
        cwd: None,
        detail: None,
        last_error: None,
        ts: chrono::Utc::now().to_rfc3339(),
    });
}

/// Clear a `creating` marker once the worktree is ready, so Claude's own hooks
/// (SessionStart → running, …) take over the record. Removes the file only if it
/// still reads `creating`, so a status Claude may already have written (it can't,
/// since this runs before the editor opens — belt and braces) is never clobbered.
/// The watcher then emits `ended`, clearing the pill until Claude's first hook.
pub fn clear_creating(ws: &str) {
    if read_record(ws).map(|r| r.state == "creating").unwrap_or(false) {
        remove(ws);
    }
}

/// Record a failed background spawn as a surfaced `last_error` on the workspace's
/// status, reusing the dismissible error block the popover already shows for
/// failed tool calls. The `Session` row is kept so the user can read the error
/// and tear the broken workspace down.
pub fn write_spawn_error(ws: &str, message: &str) {
    let ts = chrono::Utc::now().to_rfc3339();
    let _ = write_record_atomic(&StatusRecord {
        workspace: ws.to_string(),
        state: "idle".into(),
        session_id: None,
        cwd: None,
        detail: None,
        last_error: Some(ToolError {
            tool: None,
            message: message.to_string(),
            ts: ts.clone(),
            count: 1,
            surfaced: true,
        }),
        ts,
    });
}

/// Snapshot of every tracked session's status — read by the popover on open so
/// it shows correct state even if it missed live events while hidden/reloaded.
#[tauri::command]
pub fn sessions_status_list() -> Vec<StatusRecord> {
    crate::log_invoke_debug!("sessions_status_list");
    load_all()
}

/// Clear a session's `last_error` (the dismissible failed-tool block in the
/// popover). Rewrites the record without it — so a reopened popover doesn't
/// re-show a dismissed error — and the watcher re-emits the change. No-op if the
/// record is missing or already clear.
#[tauri::command]
pub fn clear_session_error(workspace: String) {
    crate::log_invoke!("clear_session_error", workspace = %workspace);
    if !is_safe_workspace_id(&workspace) {
        return;
    }
    let Some(mut record) = read_record(&workspace) else {
        return;
    };
    if record.last_error.is_none() {
        return;
    }
    record.last_error = None;
    record.ts = chrono::Utc::now().to_rfc3339();
    let _ = write_record_atomic(&record);
}

/// Remove status files with no matching session record (e.g. left over from a
/// session torn down while mAIestro Code wasn't running). Run once at startup.
pub fn sweep_stale() {
    let live: std::collections::HashSet<String> =
        crate::sessions::load_all().into_iter().map(|s| s.id).collect();
    let Ok(entries) = std::fs::read_dir(status_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let stale = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|ws| !live.contains(ws))
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Rescue sessions left stuck in `creating` by a spawn that never finished.
///
/// A fresh spawn's background task (`finish_spawn`) is the only thing that clears
/// the `creating` marker — on success (`clear_creating`) or failure
/// (`write_spawn_error`). If the app quits or crashes mid-spawn (plausible during
/// a long `post_spawn_commands` run), that task dies with its `JoinHandle`
/// dropped, so the marker is never cleared and the row shows "Creating…" forever
/// (`sweep_stale` keeps the file because the `Session` record still exists). On
/// the next launch the background task is gone for good, so any surviving
/// `creating` record is definitively orphaned: convert it into a surfaced spawn
/// error the popover shows on the row, so the user can tear the broken workspace
/// down and retry. Run once at startup, after `sweep_stale`.
pub fn reconcile_stale_creating() {
    let stuck: Vec<String> = load_all()
        .into_iter()
        .filter(|r| r.state == "creating")
        .map(|r| r.workspace)
        .collect();
    for ws in stuck {
        tracing::warn!(session = %ws, "found a session stuck in `creating` at startup; surfacing as a spawn error");
        write_spawn_error(
            &ws,
            "Spawn was interrupted before it finished (mAIestro Code quit or crashed mid-spawn). \
             Tear this workspace down and start it again.",
        );
    }
}

/// The workspace id a status file path corresponds to (its file stem).
fn workspace_of(path: &Path) -> Option<String> {
    if path.extension().is_none_or(|ext| ext != "json") {
        return None;
    }
    path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string())
}

/// Start watching `~/.maiestro/status/` and emit a `session-status` event to the
/// frontend on every change. The returned watcher must be kept alive for the app
/// lifetime (dropping it stops watching), so the caller stores it in app state.
pub fn start_watcher(app: tauri::AppHandle) -> notify::Result<notify::RecommendedWatcher> {
    use notify::{Event, EventKind, RecursiveMode, Watcher};
    use tauri::Emitter;

    let dir = status_dir();
    std::fs::create_dir_all(&dir).ok();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        // Ignore access/metadata-only events; we only care about content changes.
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        // macOS FSEvents coalesces and reports imprecise kinds, so decide by what's
        // actually on disk now rather than trusting the event kind: present →
        // forward its record; gone → synthesize `ended` so the row clears.
        for path in &event.paths {
            let Some(ws) = workspace_of(path) else { continue };
            match std::fs::read_to_string(path) {
                Ok(data) => {
                    if let Ok(record) = serde_json::from_str::<StatusRecord>(&data) {
                        let _ = app.emit("session-status", &record);
                    }
                }
                Err(_) => {
                    let record = StatusRecord {
                        workspace: ws,
                        state: "ended".into(),
                        session_id: None,
                        cwd: None,
                        detail: None,
                        last_error: None,
                        ts: chrono::Utc::now().to_rfc3339(),
                    };
                    let _ = app.emit("session-status", &record);
                }
            }
        }
    })?;

    watcher.watch(&dir, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(tool: &str, count: u32, surfaced: bool) -> ToolError {
        ToolError {
            tool: Some(tool.into()),
            message: "boom".into(),
            ts: "t".into(),
            count,
            surfaced,
        }
    }

    fn fail(tool: &str) -> Option<(Option<String>, String)> {
        Some((Some(tool.into()), "boom".into()))
    }

    // A first failure is recorded but stays pending (hidden) — Claude may recover.
    #[test]
    fn first_failure_is_pending() {
        let e = next_last_error("tool_failed", None, fail("Bash"), "t").unwrap();
        assert_eq!(e.count, 1);
        assert!(!e.surfaced);
    }

    // A second consecutive failure of the *same* tool is persistent → surfaced.
    #[test]
    fn repeated_same_tool_surfaces() {
        let prior = Some(err("Bash", 1, false));
        let e = next_last_error("tool_failed", prior, fail("Bash"), "t").unwrap();
        assert_eq!(e.count, 2);
        assert!(e.surfaced);
    }

    // A failure of a *different* tool resets the count (not the same retry).
    #[test]
    fn different_tool_resets_count() {
        let prior = Some(err("Bash", 1, false));
        let e = next_last_error("tool_failed", prior, fail("WebFetch"), "t").unwrap();
        assert_eq!(e.count, 1);
        assert!(!e.surfaced);
    }

    // A tool succeeding after a pending failure clears it — Claude recovered.
    #[test]
    fn tool_ok_clears_pending() {
        let prior = Some(err("Bash", 1, false));
        assert!(next_last_error("tool_ok", prior, None, "t").is_none());
    }

    // A tool succeeding does NOT clear an already-surfaced error (user dismisses).
    #[test]
    fn tool_ok_keeps_surfaced() {
        let prior = Some(err("Bash", 2, true));
        assert!(next_last_error("tool_ok", prior, None, "t").is_some());
    }

    // Claude stopping with a pending error promotes it to surfaced.
    #[test]
    fn idle_surfaces_pending() {
        let prior = Some(err("Bash", 1, false));
        let e = next_last_error("idle", prior, None, "t").unwrap();
        assert!(e.surfaced);
    }

    // Real ids pass; path-traversal / separator / dot ids are rejected so a
    // crafted `--workspace` can't make status_path escape ~/.maiestro/status/.
    #[test]
    fn workspace_for_cwd_matches_the_containing_worktree() {
        let sessions = vec![
            ("8-a".to_string(), "/src/work-8-a/repo".to_string()),
            ("9-b".to_string(), "/src/work-9-b/repo".to_string()),
        ];
        assert_eq!(workspace_for_cwd("/src/work-8-a/repo", &sessions).as_deref(), Some("8-a"));
        assert_eq!(workspace_for_cwd("/src/work-9-b/repo/frontend/", &sessions).as_deref(), Some("9-b"));
        // A sibling whose name merely extends the worktree's is not inside it.
        assert_eq!(workspace_for_cwd("/src/work-8-a/repo-2", &sessions), None);
        assert_eq!(workspace_for_cwd("/elsewhere", &sessions), None);
    }

    #[test]
    fn workspace_id_validation() {
        assert!(is_safe_workspace_id("8-surface-per-session"));
        assert!(is_safe_workspace_id("123"));
        assert!(!is_safe_workspace_id(""));
        assert!(!is_safe_workspace_id("."));
        assert!(!is_safe_workspace_id(".."));
        assert!(!is_safe_workspace_id("../../etc/passwd"));
        assert!(!is_safe_workspace_id("a/b"));
        assert!(!is_safe_workspace_id("a\\b"));
        assert!(!is_safe_workspace_id("a\0b"));
    }

    // Credentialed URLs in tool stderr are masked; ordinary text is untouched.
    #[test]
    fn redact_masks_url_userinfo() {
        assert_eq!(
            redact_secrets("curl https://user:tok3n@github.com/x failed"),
            "curl https://***@github.com/x failed"
        );
        assert_eq!(
            redact_secrets("connect to postgres://admin:s3cret@db:5432/app"),
            "connect to postgres://***@db:5432/app"
        );
        // No userinfo → unchanged; plain messages pass through verbatim.
        assert_eq!(redact_secrets("GET https://api.github.com/x 404"), "GET https://api.github.com/x 404");
        assert_eq!(redact_secrets("file not found: /tmp/x"), "file not found: /tmp/x");
    }

    // A new turn/session clears any error; other events carry it forward.
    #[test]
    fn prompt_clears_and_busy_carries() {
        let prior = Some(err("Bash", 1, false));
        assert!(next_last_error("prompt", prior.clone(), None, "t").is_none());
        assert!(next_last_error("running", prior.clone(), None, "t").is_none());
        assert!(next_last_error("busy", prior, None, "t").is_some());
    }

    // The hook verb → (state, detail) mapping. Working verbs all collapse to
    // `busy`; only `notification` reads the payload for its detail.
    #[test]
    fn resolve_state_maps_verbs() {
        let empty = serde_json::json!({});
        assert_eq!(resolve_state("running", &empty), ("running".into(), None));
        assert_eq!(resolve_state("prompt", &empty), ("busy".into(), None));
        assert_eq!(resolve_state("idle", &empty), ("idle".into(), None));
        assert_eq!(resolve_state("ended", &empty), ("ended".into(), None));
        // Unknown verb is recorded verbatim rather than guessed.
        assert_eq!(resolve_state("mystery", &empty), ("mystery".into(), None));
    }

    // Working verbs that carry a tool name surface it as detail.
    #[test]
    fn resolve_state_carries_tool_name() {
        let payload = serde_json::json!({ "tool_name": "Bash" });
        for verb in ["busy", "tool_failed", "tool_ok"] {
            let (state, detail) = resolve_state(verb, &payload);
            assert_eq!(state, "busy");
            assert_eq!(detail.as_deref(), Some("Bash"));
        }
    }

    // A notification with a message is needs_you + that message; an empty message
    // still flips to needs_you but with no detail.
    #[test]
    fn resolve_state_notification() {
        let with_msg = serde_json::json!({ "message": "Waiting on approval" });
        assert_eq!(
            resolve_state("notification", &with_msg),
            ("needs_you".into(), Some("Waiting on approval".into()))
        );
        let empty_msg = serde_json::json!({ "message": "   " });
        assert_eq!(resolve_state("notification", &empty_msg), ("needs_you".into(), None));
        // Codex's PermissionRequest: no message, but a tool name.
        let codex = serde_json::json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": { "command": "rm -rf x" } });
        assert_eq!(
            resolve_state("notification", &codex),
            ("needs_you".into(), Some("Permission requested: `Bash`".into()))
        );
    }

    // ── Filesystem-level tests (MAIESTRO_HOME-injected temp root) ───────────────
    //
    // The pure-logic tests above cover `next_last_error`/`resolve_state`; these
    // drive the actual on-disk read-merge-write cycle through `TempHome`, so
    // nothing touches `~/.maiestro/status/`.

    use crate::testutil::TempHome;

    fn record(ws: &str, state: &str) -> StatusRecord {
        StatusRecord {
            workspace: ws.into(),
            state: state.into(),
            session_id: None,
            cwd: None,
            detail: None,
            last_error: None,
            ts: "t".into(),
        }
    }

    // A written record round-trips through the file and the temp root actually
    // holds the file (proving the MAIESTRO_HOME override reaches status_dir).
    #[test]
    fn write_then_read_roundtrips() {
        let home = TempHome::new();
        write_record_atomic(&record("8-add-foo", "busy")).unwrap();
        assert!(home.join("status/8-add-foo.json").exists());
        let got = read_record("8-add-foo").expect("record should read back");
        assert_eq!(got.state, "busy");
        assert!(!home.join("status/.8-add-foo.json.tmp").exists(), "temp must be renamed away");
    }

    // creating → clear_creating removes the file only while still `creating`.
    #[test]
    fn clear_creating_only_removes_creating() {
        let _home = TempHome::new();
        write_creating("9-bar");
        assert_eq!(read_record("9-bar").unwrap().state, "creating");
        clear_creating("9-bar");
        assert!(read_record("9-bar").is_none(), "creating marker should be removed");

        // A non-creating record is left untouched by clear_creating.
        write_record_atomic(&record("9-bar", "busy")).unwrap();
        clear_creating("9-bar");
        assert_eq!(read_record("9-bar").unwrap().state, "busy");
    }

    // clear_session_error rewrites the record without last_error; idempotent when
    // already clear or missing.
    #[test]
    fn clear_session_error_drops_last_error() {
        let _home = TempHome::new();
        let mut rec = record("10-baz", "idle");
        rec.last_error = Some(ToolError {
            tool: Some("Bash".into()),
            message: "boom".into(),
            ts: "t".into(),
            count: 2,
            surfaced: true,
        });
        write_record_atomic(&rec).unwrap();

        clear_session_error("10-baz".into());
        let got = read_record("10-baz").expect("record should survive");
        assert!(got.last_error.is_none(), "last_error should be cleared");
        assert_eq!(got.state, "idle", "state is preserved");

        // No-op paths: already-clear record and a missing one don't panic.
        clear_session_error("10-baz".into());
        clear_session_error("does-not-exist".into());
    }

    // write_spawn_error records a surfaced last_error the popover will show.
    #[test]
    fn spawn_error_is_surfaced() {
        let _home = TempHome::new();
        write_spawn_error("11-broken", "git worktree add failed");
        let got = read_record("11-broken").expect("record written");
        let err = got.last_error.expect("last_error present");
        assert!(err.surfaced, "spawn errors surface immediately");
        assert_eq!(err.message, "git worktree add failed");
    }

    // run_hook_cli end-to-end: the read-merge-write cycle preserves a pending
    // error across a carrying event, exactly as the hook helper does live.
    #[test]
    fn hook_cli_merges_prior_error_forward() {
        let _home = TempHome::new();
        // Seed a pending (unsurfaced) failure.
        let mut seed = record("12-merge", "busy");
        seed.last_error = Some(ToolError {
            tool: Some("Bash".into()),
            message: "boom".into(),
            ts: "t".into(),
            count: 1,
            surfaced: false,
        });
        write_record_atomic(&seed).unwrap();

        // A `busy` (PreToolUse) event carries the pending error forward.
        run_hook_cli(&["busy".into(), "--workspace".into(), "12-merge".into()]);
        let after = read_record("12-merge").expect("record after hook");
        assert!(after.last_error.is_some(), "pending error carried forward on busy");

        // A `prompt` (new turn) clears it.
        run_hook_cli(&["prompt".into(), "--workspace".into(), "12-merge".into()]);
        let cleared = read_record("12-merge").expect("record after prompt");
        assert!(cleared.last_error.is_none(), "new turn clears the error");
    }
}

//! Claude Code hooks that give mAIestro Code live per-session status.
//!
//! At worktree creation we write hooks into the worktree's
//! `.claude/settings.local.json` (the personal, gitignored layer that merges with
//! the user's settings and applies to both terminal and VS Code integrated-terminal
//! sessions). Each hook invokes *this* binary as `maiestro hook <state> --workspace
//! <ws-id>`, which writes a status record the backend watches. See `status.rs` for
//! the record side and CLAUDE.md ("Live per-session status via Claude Code hooks")
//! for the full design.
//!
//! Because the helper path is `current_exe()` baked in at spawn time, a
//! torn-down/rebuilt spawner leaves stale paths behind; `reconcile_session_hooks`
//! (and `reconcile_all_session_hooks` at startup) heal them to the running binary.
//! Extracted from `spawn.rs` (issue #99).

use std::path::Path;

use crate::gitops::git;
use crate::paths::expand_tilde;
use crate::tools::shell_quote;

/// Write Claude Code hooks into the worktree's `.claude/settings.local.json`,
/// merging into any existing file rather than overwriting, so user/repo settings
/// and unrelated hooks survive. Also excludes the generated files from git.
pub async fn write_claude_hooks(work_dir: &Path, ws_id: &str) -> Result<(), String> {
    let bin = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;

    let dir = work_dir.join(".claude");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("settings.local.json");

    // Start from any existing settings, then merge ours in (replacing any prior
    // entries of ours so a changed binary path heals rather than duplicating).
    let root: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(|v: &serde_json::Value| v.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    let root = merge_hooks(root, &bin, ws_id);

    std::fs::write(&path, serde_json::to_string_pretty(&root).unwrap() + "\n")
        .map_err(|e| e.to_string())?;

    exclude_generated_files(work_dir).await;
    Ok(())
}

/// Build mAIestro Code's status-hook entries (event name → hook group) for a worktree,
/// using `bin` as the helper binary path. Shared by spawn (which writes them) and
/// startup reconcile (which rewrites them at the current binary). The commands run
/// through a shell, so the binary path (may contain spaces, e.g. inside
/// "/Applications/.../mAIestro Code.app") and the ws id are single-quoted.
fn maiestro_hook_groups(bin: &Path, ws_id: &str) -> Vec<(&'static str, serde_json::Value)> {
    let bin_q = shell_quote(&bin.to_string_lossy());
    let ws_q = shell_quote(ws_id);
    let cmd = |state: &str| format!("{bin_q} hook {state} --workspace {ws_q}");

    // Bare hook group (events that take no matcher).
    let group = |state: &str| {
        serde_json::json!({
            "hooks": [{ "type": "command", "command": cmd(state) }]
        })
    };
    // PreToolUse takes a matcher group; "" matches all tools.
    let matcher_group = |state: &str| {
        serde_json::json!({
            "matcher": "",
            "hooks": [{ "type": "command", "command": cmd(state) }]
        })
    };

    vec![
        ("SessionStart", group("running")),
        // Its own `prompt` verb (not `busy`) so a fresh turn clears a stale
        // failed-tool error while still reading as working.
        ("UserPromptSubmit", group("prompt")),
        ("PreToolUse", matcher_group("busy")),
        // PostToolUse is the event that fires *after* an approved permission
        // prompt's tool completes — the only signal that Claude has resumed
        // working. Without it, a session sticks on `needs_you` (from the prompt's
        // Notification) all the way through the rest of the turn, even while
        // Claude is actively thinking. PreToolUse alone can't cover this: it
        // fires *before* the prompt, not after approval. The distinct `tool_ok`
        // verb (vs PreToolUse's `busy`) also marks a tool *succeeding*, which
        // clears a pending transient `last_error` (issue #48); it still reads as
        // `busy`.
        ("PostToolUse", matcher_group("tool_ok")),
        // A failed tool call: captures the error into `last_error` (kept until
        // dismissed) and logs it. State stays `busy` — Claude works on past it.
        ("PostToolUseFailure", matcher_group("tool_failed")),
        ("Notification", group("notification")),
        ("Stop", group("idle")),
        ("SessionEnd", group("ended")),
    ]
}

/// True when `command` is one of mAIestro Code's status hooks for `ws_id` — matched by
/// the trailing `--workspace '<ws-id>'` we always emit, so unrelated hooks (and
/// other workspaces' hooks) in the same file are left untouched.
fn is_maiestro_hook(command: &str, ws_id: &str) -> bool {
    command.contains(" hook ") && command.contains(&format!("--workspace {}", shell_quote(ws_id)))
}

/// True when a hook *group* contains a command that's one of ours for `ws_id`.
fn group_is_ours(group: &serde_json::Value, ws_id: &str) -> bool {
    group["hooks"]
        .as_array()
        .is_some_and(|hooks| hooks.iter().any(|h| h["command"].as_str().is_some_and(|c| is_maiestro_hook(c, ws_id))))
}

/// True when a parsed settings root already carries any of our hooks for `ws_id`.
fn has_maiestro_hooks(root: &serde_json::Value, ws_id: &str) -> bool {
    root["hooks"].as_object().is_some_and(|events| {
        events
            .values()
            .any(|arr| arr.as_array().is_some_and(|gs| gs.iter().any(|g| group_is_ours(g, ws_id))))
    })
}

/// Merge mAIestro Code's hooks into a parsed settings `root`: for each event, drop any
/// existing entries that are ours (stale paths from a prior spawner), then append
/// a fresh group built from `bin`. Every other hook and setting is preserved.
fn merge_hooks(mut root: serde_json::Value, bin: &Path, ws_id: &str) -> serde_json::Value {
    if !root.is_object() {
        root = serde_json::json!({});
    }
    let mut hooks = serde_json::Value::Object(root["hooks"].as_object().cloned().unwrap_or_default());
    for (event, group) in maiestro_hook_groups(bin, ws_id) {
        let mut arr = hooks[event].as_array().cloned().unwrap_or_default();
        arr.retain(|g| !group_is_ours(g, ws_id));
        arr.push(group);
        hooks[event] = serde_json::Value::Array(arr);
    }
    root["hooks"] = hooks;
    root
}

/// Rewrite a tracked session's status hooks to point at the *currently running*
/// binary, healing a stale `current_exe()` path baked in by a spawner that has
/// since been torn down or rebuilt (issue #35). Best-effort and quiet:
///
/// - No-op if the worktree or its `.claude/settings.local.json` is gone.
/// - No-op if the file carries none of our hooks (we never inject into a worktree
///   that didn't already have them).
/// - Writes only when the resulting JSON actually changed, so it doesn't churn
///   the file on every launch.
///
/// Returns true when it rewrote the file.
pub fn reconcile_session_hooks(work_dir: &Path, ws_id: &str) -> bool {
    let path = work_dir.join(".claude").join("settings.local.json");
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&existing) else {
        return false;
    };
    if !root.is_object() || !has_maiestro_hooks(&root, ws_id) {
        return false;
    }
    let Ok(bin) = std::env::current_exe() else {
        return false;
    };
    let updated = serde_json::to_string_pretty(&merge_hooks(root, &bin, ws_id)).unwrap() + "\n";
    if updated == existing {
        return false;
    }
    std::fs::write(&path, updated).is_ok()
}

/// At startup, heal stale hook binary paths across every tracked session (see
/// `reconcile_session_hooks`). Logs how many sessions were rewritten.
pub fn reconcile_all_session_hooks() {
    let fixed = crate::sessions::load_all()
        .into_iter()
        .filter(|s| reconcile_session_hooks(Path::new(&s.work_dir), &s.id))
        .count();
    if fixed > 0 {
        tracing::info!(sessions = fixed, "reconciled stale status-hook paths");
    }
}

/// Append mAIestro Code's generated files to the worktree's shared git exclude file so
/// they don't show up as untracked changes (which would trip teardown's
/// `git status --porcelain` dirty check before Claude has run / in repos that
/// don't already ignore them). Idempotent and best-effort.
async fn exclude_generated_files(work_dir: &Path) {
    // Worktrees share the main repo's exclude via the common git dir; resolve it
    // rather than assuming `<work_dir>/.git` is a directory (in a worktree it's a
    // file pointing elsewhere).
    let Ok(common) = git(work_dir, &["rev-parse", "--git-common-dir"]).await else {
        return;
    };
    let common = expand_tilde(&common);
    let common = if common.is_absolute() { common } else { work_dir.join(common) };
    let exclude = common.join("info").join("exclude");

    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    let mut to_add: Vec<&str> = Vec::new();
    for pat in [".claude/settings.local.json", ".vscode/"] {
        if !existing.lines().any(|l| l.trim() == pat) {
            to_add.push(pat);
        }
    }
    if to_add.is_empty() {
        return;
    }
    if let Some(parent) = exclude.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut body = existing;
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str("# Added by mAIestro Code\n");
    for pat in to_add {
        body.push_str(pat);
        body.push('\n');
    }
    let _ = std::fs::write(&exclude, body);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-merging with a new binary rewrites only *our* hooks for this workspace,
    /// leaving an unrelated user hook and another workspace's hook untouched.
    #[test]
    fn merge_replaces_only_our_hooks_for_this_ws() {
        let ws = "35-status-hooks-fix";
        let old_bin = Path::new("/old/work-8/backend/target/debug/maiestro");

        // A settings file as a prior spawn wrote it, plus an unrelated user hook
        // and a *different* workspace's status hook sharing the Stop event.
        let mut root = merge_hooks(serde_json::json!({}), old_bin, ws);
        let stop = root["hooks"]["Stop"].as_array_mut().unwrap();
        stop.push(serde_json::json!({ "hooks": [{ "type": "command", "command": "echo hi" }] }));
        stop.push(serde_json::json!({
            "hooks": [{ "type": "command", "command": "'/x/maiestro' hook idle --workspace '99-other'" }]
        }));

        let new_bin = Path::new("/Applications/mAIestro Code.app/Contents/MacOS/maiestro");
        let merged = merge_hooks(root, new_bin, ws);

        let cmds: Vec<&str> = merged["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|g| g["hooks"][0]["command"].as_str())
            .collect();

        // Our stale entry was rewritten to the new binary; the old path is gone.
        assert!(cmds.iter().any(|c| c.contains("/Applications/mAIestro Code.app") && c.contains("--workspace '35-status-hooks-fix'")));
        assert!(!cmds.iter().any(|c| c.contains("/old/work-8")));
        // Exactly one of our entries for this ws remains (no duplication).
        assert_eq!(cmds.iter().filter(|c| is_maiestro_hook(c, ws)).count(), 1);
        // Unrelated and other-workspace hooks survive untouched.
        assert!(cmds.contains(&"echo hi"));
        assert!(cmds.iter().any(|c| c.contains("--workspace '99-other'")));
    }

    /// A worktree carrying our hooks is detected; an unrelated-only file is not.
    #[test]
    fn detects_our_hooks() {
        let ws = "12-foo";
        let ours = merge_hooks(serde_json::json!({}), Path::new("/bin/maiestro"), ws);
        assert!(has_maiestro_hooks(&ours, ws));

        let unrelated = serde_json::json!({
            "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "echo hi" }] }] }
        });
        assert!(!has_maiestro_hooks(&unrelated, ws));
        // Our hooks for a *different* ws don't count as this ws's.
        assert!(!has_maiestro_hooks(&ours, "99-other"));
    }
}

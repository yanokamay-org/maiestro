//! Agent hooks (Claude Code, Codex, or Antigravity) that give mAIestro Code live
//! per-session status.
//!
//! **Claude:** at worktree creation we write hooks into the worktree's
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
//!
//! **Codex** trusts a hook only after the user reviews it, recording the approval
//! against the hook's *key and definition hash* in `~/.codex/config.toml`. So
//! Codex hooks must be byte-identical for every worktree, or each spawn would
//! need a fresh review. They are therefore passed on the launch command as
//! `-c hooks.<Event>=…` session flags ([`codex_hook_overrides`]) — whose trust key
//! is `/<session-flags>/config.toml:<event>:…`, with no folder path in it — and
//! run a **stable wrapper** (`~/.maiestro/bin/maiestro-hook <verb>`, see
//! [`ensure_hook_wrapper`]) with no workspace id; the helper finds the session
//! from the payload's `cwd`. The user trusts them once (Codex asks at session
//! start → trust all) and never again, across worktrees and mAIestro Code
//! updates (only the wrapper's contents change, not the hook definitions). A project `.codex/hooks.json` is
//! not used: Codex doesn't load it from a spawned worktree at all.
//!
//! **Antigravity** (issue #185) reads workspace hooks from the worktree's
//! `.agents/hooks.json`, a map of *named* hook groups; we own the
//! `maiestro-status` group ([`antigravity_hook_group`]) and leave any others.
//! Antigravity loads it once the user trusts the folder (its own per-folder
//! prompt at session start, which it shows for any new folder anyway). Unlike
//! Claude, a `PreToolUse` hook's stdout is a *decision* — `{}`, an empty
//! decision, or a failing command all **deny** the tool — so every command we
//! install discards its output and always exits 0 (see [`antigravity_hook_command`]),
//! and `PreToolUse` is registered only for the tools that ask the user something.
//! Extracted from `spawn.rs` (issue #99).

use std::path::{Path, PathBuf};

use crate::agent::Agent;
use crate::gitops::git;
use crate::paths::expand_tilde;
use crate::tools::shell_quote;

/// The worktree file Claude Code reads mAIestro Code's status hooks from.
fn claude_hook_file(work_dir: &Path) -> PathBuf {
    work_dir.join(".claude").join("settings.local.json")
}

/// Install the agent's status hooks for a freshly spawned worktree. Claude: merge
/// them into [`claude_hook_file`] (never overwriting user/repo settings or
/// unrelated hooks). Codex: nothing is written into the worktree — its hooks ride
/// on the launch command ([`codex_hook_overrides`]) — so only the stable wrapper
/// they call is refreshed. Antigravity: set our named group in
/// [`antigravity_hook_file`]. Either way the generated files are excluded from git.
pub async fn write_session_hooks(work_dir: &Path, ws_id: &str, agent: Agent) -> Result<(), String> {
    if agent == Agent::Codex {
        ensure_hook_wrapper()?;
        exclude_generated_files(work_dir, agent).await;
        return Ok(());
    }
    let bin = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    if agent == Agent::Antigravity {
        write_antigravity_hooks(work_dir, ws_id, &bin)?;
        exclude_generated_files(work_dir, agent).await;
        return Ok(());
    }

    let path = claude_hook_file(work_dir);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }

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

    exclude_generated_files(work_dir, agent).await;
    Ok(())
}

/// Build mAIestro Code's Claude status-hook entries (event name → hook group) for
/// a worktree, using `bin` as the helper binary path. Shared by spawn (which writes them) and
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
/// - Codex: the hook definitions never change (they call the stable wrapper), so
///   this only re-points the wrapper at the running binary ([`ensure_hook_wrapper`]).
/// - Claude: no-op if the worktree or its `.claude/settings.local.json` is gone,
///   or if the file carries none of our hooks (we never inject into a worktree
///   that didn't already have them). Writes only when the resulting JSON
///   actually changed, so it doesn't churn the file on every launch.
/// - Antigravity: the same rules for our group in `.agents/hooks.json`.
///
/// Returns true when it rewrote something.
pub fn reconcile_session_hooks(work_dir: &Path, ws_id: &str, agent: Agent) -> bool {
    if agent == Agent::Codex {
        return ensure_hook_wrapper().unwrap_or(false);
    }
    let Ok(bin) = std::env::current_exe() else {
        return false;
    };
    if agent == Agent::Antigravity {
        return reconcile_antigravity_hooks_with(work_dir, ws_id, &bin);
    }
    reconcile_hooks_with(work_dir, ws_id, &bin)
}

/// The Claude half of [`reconcile_session_hooks`] against an explicit `bin`, so
/// tests can prove a stale path is rewritten without depending on the test
/// binary's own path.
fn reconcile_hooks_with(work_dir: &Path, ws_id: &str, bin: &Path) -> bool {
    let path = claude_hook_file(work_dir);
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&existing) else {
        return false;
    };
    if !root.is_object() || !has_maiestro_hooks(&root, ws_id) {
        return false;
    }
    let updated = serde_json::to_string_pretty(&merge_hooks(root, bin, ws_id)).unwrap() + "\n";
    if updated == existing {
        return false;
    }
    std::fs::write(&path, updated).is_ok()
}

/// Remove mAIestro Code's Claude status hooks for `ws_id` from the worktree's
/// [`claude_hook_file`], keeping every other hook and setting — used when a
/// session switches away from Claude (issue #186), so a `claude` run by hand in
/// that worktree no longer writes the session's status. Deletes the file if
/// nothing else is left in it. Best-effort; returns whether it changed anything.
pub fn remove_claude_hooks(work_dir: &Path, ws_id: &str) -> bool {
    let path = claude_hook_file(work_dir);
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&existing) else {
        return false;
    };
    if !root.is_object() || !has_maiestro_hooks(&root, ws_id) {
        return false;
    }
    let root = strip_hooks(root, ws_id);
    if root.as_object().is_some_and(|o| o.is_empty()) {
        return std::fs::remove_file(&path).is_ok();
    }
    std::fs::write(&path, serde_json::to_string_pretty(&root).unwrap() + "\n").is_ok()
}

/// Drop our hook groups for `ws_id` from a parsed settings `root`, then any event
/// left empty, then `hooks` itself if empty. Everything else is preserved.
fn strip_hooks(mut root: serde_json::Value, ws_id: &str) -> serde_json::Value {
    let Some(events) = root["hooks"].as_object().cloned() else {
        return root;
    };
    let mut kept = serde_json::Map::new();
    for (event, groups) in events {
        match groups.as_array() {
            Some(gs) => {
                let gs: Vec<_> = gs.iter().filter(|g| !group_is_ours(g, ws_id)).cloned().collect();
                if !gs.is_empty() {
                    kept.insert(event, serde_json::Value::Array(gs));
                }
            }
            None => {
                kept.insert(event, groups);
            }
        }
    }
    let obj = root.as_object_mut().unwrap();
    if kept.is_empty() {
        obj.remove("hooks");
    } else {
        obj.insert("hooks".into(), serde_json::Value::Object(kept));
    }
    root
}

/// At startup, heal stale hook binary paths across every tracked Claude and
/// Antigravity session (see `reconcile_session_hooks`), and point the Codex
/// wrapper at the running binary (once, however many Codex sessions exist). Logs
/// what it rewrote.
pub fn reconcile_all_session_hooks() {
    let wrapper = matches!(ensure_hook_wrapper(), Ok(true));
    let fixed = crate::sessions::load_all()
        .into_iter()
        .filter(|s| s.agent != Agent::Codex)
        .filter(|s| reconcile_session_hooks(Path::new(&s.work_dir), &s.id, s.agent))
        .count();
    if fixed > 0 || wrapper {
        tracing::info!(sessions = fixed, codex_wrapper = wrapper, "reconciled stale status-hook paths");
    }
}

// ── Antigravity: a named group in the worktree's `.agents/hooks.json` ──────────

/// The worktree file Antigravity reads workspace hooks from.
fn antigravity_hook_file(work_dir: &Path) -> PathBuf {
    work_dir.join(".agents").join("hooks.json")
}

/// The key of mAIestro Code's hook group in [`antigravity_hook_file`]. Hook
/// groups are named, so owning one key is the whole merge.
const ANTIGRAVITY_GROUP: &str = "maiestro-status";

/// The tools through which an Antigravity agent asks the user something — the
/// only "needs you" signal it exposes (its native permission prompts fire no
/// hook). Also the only tools our `PreToolUse` hook is registered for.
const ANTIGRAVITY_ASK_TOOLS: &str = "ask_question|ask_permission|ask_custom_permission";

/// One Antigravity hook command: run the helper with `verb` for `ws_id`, then
/// discard its output and exit 0 no matter what. For `PreToolUse`, Antigravity
/// treats any stdout (even `{}`) or a non-zero exit as a *deny*, and empty
/// output as "no opinion"; so this must stay silent and succeed even when the
/// baked binary no longer exists, or it would block the user's tools.
fn antigravity_hook_command(bin: &Path, ws_id: &str, verb: &str) -> String {
    format!(
        "{} hook {verb} --workspace {} >/dev/null 2>&1 || true",
        shell_quote(&bin.to_string_lossy()),
        shell_quote(ws_id)
    )
}

/// mAIestro Code's Antigravity hook group for a worktree. Events → helper verbs
/// (see `status::normalize_verb` for the payload-dependent ones):
///
/// - `PreInvocation` → `invocation`: working. It fires before every model call;
///   the first of a turn (`invocationNum` 0) acts as a new prompt.
/// - `PreToolUse` on [`ANTIGRAVITY_ASK_TOOLS`] only → `notification`: needs you.
/// - `PostToolUse` (every tool) → `tool_done`: `tool_ok`, or `tool_failed` when
///   the payload carries an `error`.
/// - `Stop` → `stop`: idle (surfacing the stop's `error`, if any).
///
/// There is no session start/end or permission-prompt event to map.
fn antigravity_hook_group(bin: &Path, ws_id: &str) -> serde_json::Value {
    let handler = |verb: &str| {
        serde_json::json!({ "type": "command", "command": antigravity_hook_command(bin, ws_id, verb), "timeout": 10 })
    };
    serde_json::json!({
        "PreInvocation": [handler("invocation")],
        "PreToolUse": [{ "matcher": ANTIGRAVITY_ASK_TOOLS, "hooks": [handler("notification")] }],
        "PostToolUse": [{ "matcher": "*", "hooks": [handler("tool_done")] }],
        "Stop": [handler("stop")],
    })
}

/// `root` (a parsed `.agents/hooks.json`) with our group set for `bin`/`ws_id`.
/// Every other named group is preserved.
fn merge_antigravity_hooks(mut root: serde_json::Value, bin: &Path, ws_id: &str) -> serde_json::Value {
    if !root.is_object() {
        root = serde_json::json!({});
    }
    root[ANTIGRAVITY_GROUP] = antigravity_hook_group(bin, ws_id);
    root
}

/// Set our group in the worktree's `.agents/hooks.json`, creating it if needed.
/// A file that exists but isn't a JSON object (a repo may commit `.agents/`) is
/// left alone — the session then just shows no status — rather than clobbered.
fn write_antigravity_hooks(work_dir: &Path, ws_id: &str, bin: &Path) -> Result<(), String> {
    let path = antigravity_hook_file(work_dir);
    let root = match std::fs::read_to_string(&path) {
        Err(_) => serde_json::json!({}),
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) if v.is_object() => v,
            _ => {
                tracing::warn!(path = %path.display(), "not installing status hooks: existing .agents/hooks.json isn't a JSON object");
                return Ok(());
            }
        },
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_string_pretty(&merge_antigravity_hooks(root, bin, ws_id)).unwrap() + "\n";
    std::fs::write(&path, body).map_err(|e| e.to_string())
}

/// Remove our group from the worktree's `.agents/hooks.json` — used when a
/// session switches away from Antigravity (issue #186), so an `agy` run by hand
/// there no longer writes the session's status. Other groups are kept; a file
/// left empty is deleted. Best-effort; returns whether it changed anything.
pub fn remove_antigravity_hooks(work_dir: &Path) -> bool {
    let path = antigravity_hook_file(work_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(serde_json::Value::Object(mut root)) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    if root.remove(ANTIGRAVITY_GROUP).is_none() {
        return false;
    }
    if root.is_empty() {
        return std::fs::remove_file(&path).is_ok();
    }
    std::fs::write(&path, serde_json::to_string_pretty(&root).unwrap() + "\n").is_ok()
}

/// The Antigravity half of [`reconcile_session_hooks`] against an explicit
/// `bin`: rewrite our group only if the file already has it and it changed.
fn reconcile_antigravity_hooks_with(work_dir: &Path, ws_id: &str, bin: &Path) -> bool {
    let path = antigravity_hook_file(work_dir);
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&existing) else {
        return false;
    };
    if root.get(ANTIGRAVITY_GROUP).is_none() {
        return false;
    }
    let updated = serde_json::to_string_pretty(&merge_antigravity_hooks(root, bin, ws_id)).unwrap() + "\n";
    if updated == existing {
        return false;
    }
    std::fs::write(&path, updated).is_ok()
}

// ── Codex: session-flag hooks through a stable wrapper ──────────────────────────

/// Codex hook events → helper verbs. The same verbs as Claude's, with two gaps:
/// no `PostToolUseFailure` (so Codex sessions never get a `last_error`) and no
/// `Notification` — `PermissionRequest` is Codex's "needs you" signal, and
/// `status::resolve_state` names the tool when there's no message. The bool marks
/// tool events, which take a matcher group (`""` = every tool).
const CODEX_HOOK_EVENTS: &[(&str, &str, bool)] = &[
    ("SessionStart", "running", false),
    ("UserPromptSubmit", "prompt", false),
    ("PreToolUse", "busy", true),
    ("PostToolUse", "tool_ok", true),
    ("PermissionRequest", "notification", true),
    ("Stop", "idle", false),
    ("SessionEnd", "ended", false),
];

/// The stable path every Codex hook invokes. Its *contents* follow the running
/// binary; its *path* never changes, which is what keeps the hook definitions —
/// and so the user's one-time Codex trust — valid across mAIestro Code updates.
pub fn hook_wrapper_path() -> PathBuf {
    crate::paths::maiestro_dir("bin/maiestro-hook")
}

/// The wrapper script for `bin`: forward every argument to `maiestro hook`.
fn hook_wrapper_script(bin: &Path) -> String {
    format!(
        "#!/bin/sh\n# Written by mAIestro Code; runs its status-hook helper.\nexec {} hook \"$@\"\n",
        shell_quote(&bin.to_string_lossy())
    )
}

/// Write (or re-point) the wrapper at [`hook_wrapper_path`] so it runs the
/// current binary. Returns whether it changed. Called on every Codex spawn and
/// reopen and at startup.
pub fn ensure_hook_wrapper() -> Result<bool, String> {
    let bin = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    write_hook_wrapper(&hook_wrapper_path(), &bin)
}

fn write_hook_wrapper(path: &Path, bin: &Path) -> Result<bool, String> {
    use std::os::unix::fs::PermissionsExt;
    let script = hook_wrapper_script(bin);
    if std::fs::read_to_string(path).is_ok_and(|s| s == script) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    crate::paths::write_atomic(path, script.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Quote `s` as a TOML basic string.
fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The `-c` values that install mAIestro Code's status hooks into a Codex session,
/// one `hooks.<Event>=[…]` TOML override per event, all calling
/// [`hook_wrapper_path`] with just a verb. They are identical for every worktree
/// (the helper resolves the session from the payload's `cwd`), so Codex's
/// per-definition trust is granted once and holds everywhere.
pub fn codex_hook_overrides() -> Vec<String> {
    codex_hook_overrides_for(&hook_wrapper_path())
}

fn codex_hook_overrides_for(wrapper: &Path) -> Vec<String> {
    let wrapper = shell_quote(&wrapper.to_string_lossy());
    CODEX_HOOK_EVENTS
        .iter()
        .map(|(event, verb, tool_event)| {
            let handler = format!("{{type=\"command\",command={}}}", toml_string(&format!("{wrapper} {verb}")));
            let matcher = if *tool_event { "matcher=\"\"," } else { "" };
            format!("hooks.{event}=[{{{matcher}hooks=[{handler}]}}]")
        })
        .collect()
}

// ── Codex: will the session ask the user to review our hooks? ──────────────────

/// Whether Codex's `hooks/list` reply shows any of mAIestro Code's session-flag
/// hooks as not yet trusted (`untrusted`, or `modified` after a definition
/// change) — i.e. whether the next Codex session will ask the user to review
/// them. Hooks from other sources (the user's own, a repo's) are ignored. A
/// reply with none of ours is `false`: we only prompt when we know Codex will.
fn hooks_list_needs_review(reply: &serde_json::Value) -> bool {
    reply["result"]["data"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|d| d["hooks"].as_array().into_iter().flatten())
        .filter(|h| h["key"].as_str().is_some_and(|k| k.starts_with("/<session-flags>/")))
        .any(|h| h["trustStatus"].as_str() != Some("trusted"))
}

/// Ask Codex — read-only, through its app-server's `hooks/list` — whether the
/// status hooks a Codex session is launched with ([`codex_hook_overrides`]) still
/// need the user's one-time review. Codex computes trust itself (the stored
/// approval is keyed by hook definition hash), so this is the only reliable way
/// to know. Best-effort: any failure (no `codex`, an older Codex without
/// `hooks/list`, a timeout) reads as `false`, so the popover never nags on a
/// guess. Takes ~0.1s.
pub async fn codex_hooks_need_review() -> bool {
    match tokio::time::timeout(std::time::Duration::from_secs(10), query_codex_hooks()).await {
        Ok(Ok(reply)) => hooks_list_needs_review(&reply),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "couldn't ask codex whether its hooks are trusted");
            false
        }
        Err(_) => {
            tracing::warn!("timed out asking codex whether its hooks are trusted");
            false
        }
    }
}

/// One `initialize` + `hooks/list` round trip with `codex app-server` over stdio
/// JSON-RPC, with our hooks passed as the same `-c` overrides a session gets.
async fn query_codex_hooks() -> Result<serde_json::Value, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut cmd = crate::tools::tokio_command("codex");
    cmd.arg("app-server");
    for ov in codex_hook_overrides() {
        cmd.arg("-c").arg(ov);
    }
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not run codex: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut lines = BufReader::new(child.stdout.take().ok_or("no stdout")?).lines();
    let cwd = crate::paths::home().to_string_lossy().into_owned();
    for msg in [
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "clientInfo": { "name": "maiestro", "version": env!("CARGO_PKG_VERSION") } } }),
        serde_json::json!({ "jsonrpc": "2.0", "method": "initialized" }),
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "hooks/list", "params": { "cwds": [cwd] } }),
    ] {
        stdin.write_all(format!("{msg}\n").as_bytes()).await.map_err(|e| e.to_string())?;
    }
    stdin.flush().await.map_err(|e| e.to_string())?;
    while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if v["id"] == 2 {
            if let Some(err) = v.get("error") {
                return Err(format!("hooks/list failed: {err}"));
            }
            return Ok(v);
        }
    }
    Err("codex app-server closed before answering".into())
}

/// Whether opening this session (or, with no session, spawning in this repo)
/// will start a Codex session that asks the user to review mAIestro Code's status
/// hooks — so the popover can explain the one-time "trust all" step
/// *before* VS Code opens. The agent is the session record's when given (a reopen
/// launches what the worktree was spawned with), else the repo's effective agent.
/// Always `false` for Claude.
#[tauri::command]
pub async fn codex_hooks_review_needed(repo: String, session_id: Option<String>) -> Result<bool, String> {
    crate::log_invoke_debug!("codex_hooks_review_needed", repo = %repo);
    let agent = match session_id.as_deref().and_then(crate::sessions::get) {
        Some(s) => s.agent,
        None => crate::repo_settings::effective_agent(&crate::repo_settings::repo_settings_get(repo)?),
    };
    if agent != Agent::Codex {
        return Ok(false);
    }
    Ok(codex_hooks_need_review().await)
}

/// The generated files to keep out of git for a worktree running `agent`. The
/// Antigravity hook file is added only for Antigravity worktrees: the exclude
/// file is shared by the whole repo, and elsewhere `.agents/hooks.json` is the
/// user's own.
fn generated_file_patterns(agent: Agent) -> Vec<&'static str> {
    let mut pats = vec![".claude/settings.local.json", ".vscode/"];
    if agent == Agent::Antigravity {
        pats.push(".agents/hooks.json");
    }
    pats
}

/// Append mAIestro Code's generated files to the worktree's shared git exclude file so
/// they don't show up as untracked changes (which would trip teardown's
/// `git status --porcelain` dirty check before the agent has run / in repos that
/// don't already ignore them). Idempotent and best-effort. (An exclude can't hide
/// changes to a *tracked* file: a repo that commits `.agents/hooks.json` sees our
/// group as a modification in an Antigravity worktree.)
async fn exclude_generated_files(work_dir: &Path, agent: Agent) {
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
    for pat in generated_file_patterns(agent) {
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

    /// The Codex hooks are the same for every worktree — no workspace id, no
    /// worktree path — so one Codex trust approval covers them all. Each event
    /// calls the wrapper with its verb; tool events carry a match-all matcher.
    #[test]
    fn codex_hook_overrides_are_worktree_independent() {
        let ov = codex_hook_overrides_for(Path::new("/home/u/.maiestro/bin/maiestro-hook"));
        assert_eq!(ov.len(), 7);
        assert!(ov.contains(&r#"hooks.Stop=[{hooks=[{type="command",command="'/home/u/.maiestro/bin/maiestro-hook' idle"}]}]"#.to_string()), "{ov:?}");
        assert!(ov.contains(&r#"hooks.PermissionRequest=[{matcher="",hooks=[{type="command",command="'/home/u/.maiestro/bin/maiestro-hook' notification"}]}]"#.to_string()), "{ov:?}");
        assert!(!ov.iter().any(|o| o.contains("PostToolUseFailure") || o.contains("Notification=") || o.contains("--workspace")));
    }

    /// Only our session-flag hooks count, and any state but `trusted` needs review.
    #[test]
    fn needs_review_reads_our_hooks_trust_status() {
        let reply = |hooks: serde_json::Value| serde_json::json!({ "id": 2, "result": { "data": [{ "cwd": "/h", "hooks": hooks }] } });
        let ours = |status: &str| serde_json::json!({ "key": "/<session-flags>/config.toml:stop:0:0", "trustStatus": status });
        assert!(!hooks_list_needs_review(&reply(serde_json::json!([ours("trusted"), ours("trusted")]))));
        assert!(hooks_list_needs_review(&reply(serde_json::json!([ours("trusted"), ours("untrusted")]))));
        assert!(hooks_list_needs_review(&reply(serde_json::json!([ours("modified")]))));
        // Someone else's untrusted hook isn't ours to explain; no hooks at all → don't nag.
        let theirs = serde_json::json!({ "key": "/h/.codex/config.toml:stop:0:0", "trustStatus": "untrusted" });
        assert!(!hooks_list_needs_review(&reply(serde_json::json!([theirs]))));
        assert!(!hooks_list_needs_review(&reply(serde_json::json!([]))));
        assert!(!hooks_list_needs_review(&serde_json::json!({ "id": 2, "error": {} })));
    }

    /// A wrapper path with a quote or backslash in it still yields valid TOML.
    #[test]
    fn toml_string_escapes() {
        assert_eq!(toml_string(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    /// The wrapper execs the given binary's `hook` subcommand, is executable, and
    /// is only rewritten when the binary changes.
    #[test]
    fn hook_wrapper_follows_the_binary() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin/maiestro-hook");
        assert!(write_hook_wrapper(&path, Path::new("/old/maiestro")).unwrap());
        assert!(!write_hook_wrapper(&path, Path::new("/old/maiestro")).unwrap(), "no churn");
        assert!(write_hook_wrapper(&path, Path::new("/Applications/mAIestro Code.app/Contents/MacOS/maiestro")).unwrap());
        let script = std::fs::read_to_string(&path).unwrap();
        assert!(script.contains(r#"exec '/Applications/mAIestro Code.app/Contents/MacOS/maiestro' hook "$@""#), "{script}");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o111, 0o111);
    }

    /// Reconcile rewrites a Claude worktree's hook file to the current binary.
    #[test]
    fn reconcile_rewrites_the_claude_hook_file() {
        let dir = tempfile::tempdir().unwrap();
        let ws = "9-claude";
        let path = claude_hook_file(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let root = merge_hooks(serde_json::json!({}), Path::new("/old/maiestro"), ws);
        std::fs::write(&path, serde_json::to_string_pretty(&root).unwrap()).unwrap();
        let new_bin = Path::new("/Applications/mAIestro Code.app/Contents/MacOS/maiestro");
        assert!(reconcile_hooks_with(dir.path(), ws, new_bin));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("/old/maiestro") && text.contains("mAIestro Code.app"), "{text}");
        assert!(!reconcile_hooks_with(dir.path(), ws, new_bin), "no churn when current");
    }

    /// Switching away from Claude (#186) strips only our hooks for this
    /// workspace: a user hook, another workspace's hook, and unrelated settings
    /// survive; a file left with nothing in it is deleted.
    #[test]
    fn remove_claude_hooks_keeps_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        let ws = "186-switch";
        let path = claude_hook_file(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bin = Path::new("/x/maiestro");
        let mut root = merge_hooks(serde_json::json!({ "model": "opus" }), bin, ws);
        root["hooks"]["Stop"].as_array_mut().unwrap().push(serde_json::json!({
            "hooks": [{ "type": "command", "command": "echo hi" }]
        }));
        root["hooks"]["Stop"].as_array_mut().unwrap().push(serde_json::json!({
            "hooks": [{ "type": "command", "command": "'/x/maiestro' hook idle --workspace '99-other'" }]
        }));
        std::fs::write(&path, serde_json::to_string_pretty(&root).unwrap()).unwrap();

        assert!(remove_claude_hooks(dir.path(), ws));
        let left: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!has_maiestro_hooks(&left, ws), "{left}");
        assert_eq!(left["model"], "opus");
        let stop = left["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2, "{left}");
        assert!(left["hooks"].get("SessionStart").is_none(), "empty events are dropped: {left}");
        assert!(!remove_claude_hooks(dir.path(), ws), "nothing left to remove");

        // Only ours → the file goes away entirely.
        std::fs::write(&path, serde_json::to_string_pretty(&merge_hooks(serde_json::json!({}), bin, ws)).unwrap()).unwrap();
        assert!(remove_claude_hooks(dir.path(), ws));
        assert!(!path.exists());
    }

    /// Antigravity treats any `PreToolUse` output — even `{}` — or a non-zero
    /// exit as a deny. Every command we install is silent and exits 0, even when
    /// the baked binary is gone (a moved app must never block the user's tools).
    #[test]
    fn antigravity_hook_commands_are_silent_and_never_fail() {
        let gone = Path::new("/nonexistent/mAIestro Code.app/Contents/MacOS/maiestro");
        let group = antigravity_hook_group(gone, "185-x");
        let mut commands = Vec::new();
        for (event, handlers) in group.as_object().unwrap() {
            for h in handlers.as_array().unwrap() {
                let hooks = h.get("hooks").and_then(|v| v.as_array()).cloned().unwrap_or_else(|| vec![h.clone()]);
                for hook in hooks {
                    commands.push((event.clone(), hook["command"].as_str().unwrap().to_string()));
                }
            }
        }
        assert_eq!(commands.len(), 4, "{commands:?}");
        for (event, cmd) in &commands {
            assert!(cmd.ends_with(">/dev/null 2>&1 || true"), "{event}: {cmd}");
            let out = std::process::Command::new("sh")
                .args(["-c", cmd])
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(out.status.success(), "{event} failed");
            assert!(out.stdout.is_empty(), "{event} printed {:?}", String::from_utf8_lossy(&out.stdout));
        }
    }

    /// PreToolUse fires only for the tools that ask the user something; the
    /// rest of the event map is the documented one.
    #[test]
    fn antigravity_hook_group_maps_the_events() {
        let group = antigravity_hook_group(Path::new("/bin/maiestro"), "185-x");
        let pre = &group["PreToolUse"][0];
        assert_eq!(pre["matcher"], "ask_question|ask_permission|ask_custom_permission");
        assert!(pre["hooks"][0]["command"].as_str().unwrap().contains(" hook notification --workspace '185-x'"));
        assert_eq!(group["PostToolUse"][0]["matcher"], "*");
        assert!(group["PostToolUse"][0]["hooks"][0]["command"].as_str().unwrap().contains(" hook tool_done "));
        assert!(group["PreInvocation"][0]["command"].as_str().unwrap().contains(" hook invocation "));
        assert!(group["Stop"][0]["command"].as_str().unwrap().contains(" hook stop "));
        assert!(group.get("PostInvocation").is_none());
    }

    /// Writing keeps the user's own hook groups, creates the file when absent,
    /// never clobbers a file that isn't a JSON object, and reconcile re-points
    /// our group at a new binary without churn.
    #[test]
    fn antigravity_hooks_merge_and_reconcile() {
        let dir = tempfile::tempdir().unwrap();
        let path = antigravity_hook_file(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"lint": {"PostToolUse": []}}"#).unwrap();
        write_antigravity_hooks(dir.path(), "185-x", Path::new("/old/maiestro")).unwrap();
        let root: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(root.get("lint").is_some() && root.get("maiestro-status").is_some());

        let new_bin = Path::new("/Applications/mAIestro Code.app/Contents/MacOS/maiestro");
        assert!(reconcile_antigravity_hooks_with(dir.path(), "185-x", new_bin));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("/old/maiestro") && text.contains("mAIestro Code.app") && text.contains("\"lint\""), "{text}");
        assert!(!reconcile_antigravity_hooks_with(dir.path(), "185-x", new_bin), "no churn when current");

        std::fs::write(&path, "not json").unwrap();
        write_antigravity_hooks(dir.path(), "185-x", new_bin).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
        assert!(!reconcile_antigravity_hooks_with(dir.path(), "185-x", new_bin));

        let fresh = tempfile::tempdir().unwrap();
        assert!(!reconcile_antigravity_hooks_with(fresh.path(), "185-x", new_bin), "never injects");
        write_antigravity_hooks(fresh.path(), "185-x", new_bin).unwrap();
        assert!(antigravity_hook_file(fresh.path()).is_file());
    }

    /// Switching away from Antigravity drops only our group; a file left empty
    /// is deleted.
    #[test]
    fn remove_antigravity_hooks_keeps_other_groups() {
        let dir = tempfile::tempdir().unwrap();
        let path = antigravity_hook_file(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"lint": {"PostToolUse": []}}"#).unwrap();
        write_antigravity_hooks(dir.path(), "185-x", Path::new("/x/maiestro")).unwrap();
        assert!(remove_antigravity_hooks(dir.path()));
        let left: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(left, serde_json::json!({ "lint": { "PostToolUse": [] } }));
        assert!(!remove_antigravity_hooks(dir.path()), "nothing left to remove");

        std::fs::remove_file(&path).unwrap();
        write_antigravity_hooks(dir.path(), "185-x", Path::new("/x/maiestro")).unwrap();
        assert!(remove_antigravity_hooks(dir.path()));
        assert!(!path.exists());
    }

    /// `.agents/hooks.json` is git-excluded only for Antigravity worktrees.
    #[test]
    fn antigravity_hook_file_is_excluded_only_for_antigravity() {
        assert!(generated_file_patterns(Agent::Antigravity).contains(&".agents/hooks.json"));
        assert!(!generated_file_patterns(Agent::Claude).contains(&".agents/hooks.json"));
        assert!(!generated_file_patterns(Agent::Codex).contains(&".agents/hooks.json"));
    }
}

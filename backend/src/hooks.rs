//! Agent hooks (Claude Code or Codex) that give mAIestro Code live per-session status.
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
/// they call is refreshed. Either way the generated files are excluded from git.
pub async fn write_session_hooks(work_dir: &Path, ws_id: &str, agent: Agent) -> Result<(), String> {
    if agent == Agent::Codex {
        ensure_hook_wrapper()?;
        exclude_generated_files(work_dir).await;
        return Ok(());
    }
    let bin = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;

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

    exclude_generated_files(work_dir).await;
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
///
/// Returns true when it rewrote something.
pub fn reconcile_session_hooks(work_dir: &Path, ws_id: &str, agent: Agent) -> bool {
    if agent == Agent::Codex {
        return ensure_hook_wrapper().unwrap_or(false);
    }
    let Ok(bin) = std::env::current_exe() else {
        return false;
    };
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

/// At startup, heal stale hook binary paths across every tracked Claude session
/// (see `reconcile_session_hooks`), and point the Codex wrapper at the running
/// binary (once, however many Codex sessions exist). Logs what it rewrote.
pub fn reconcile_all_session_hooks() {
    let wrapper = matches!(ensure_hook_wrapper(), Ok(true));
    let fixed = crate::sessions::load_all()
        .into_iter()
        .filter(|s| s.agent == Agent::Claude)
        .filter(|s| reconcile_session_hooks(Path::new(&s.work_dir), &s.id, s.agent))
        .count();
    if fixed > 0 || wrapper {
        tracing::info!(sessions = fixed, codex_wrapper = wrapper, "reconciled stale status-hook paths");
    }
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

/// Append mAIestro Code's generated files to the worktree's shared git exclude file so
/// they don't show up as untracked changes (which would trip teardown's
/// `git status --porcelain` dirty check before the agent has run / in repos that
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
}

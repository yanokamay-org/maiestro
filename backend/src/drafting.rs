//! AI drafting: turn a free-text idea into a GitHub issue (title + body + short
//! label), and compress an existing issue into a short session label — all via
//! headless calls to the repo's agent ([`agent_text`]: `claude -p` or
//! `codex exec`).
//!
//! These calls run the agent in the repo for context but with no tools (no
//! file/shell/edit/fetch access), so prompt injection from the input or repo files
//! is contained to, at worst, a bad title the user reviews — never code execution.
//! `AgentActivity` correlates a run with the UI action that started it so its
//! busy glow can switch to the rainbow (AI) variant. Extracted from `spawn.rs`
//! (issue #99).

use std::path::Path;

use crate::agent::Agent;
use crate::naming::{default_short_title, trim_to_word};
use crate::plugins::GitHub;
use crate::repo_context::{repo_context, validated_cloned_repo};
use crate::tools::snippet;

/// Pull the inner `{title, body, short_title}` out of claude's reply text (which
/// may wrap it in code fences or prose) and extract a non-empty title, body, and
/// a short branch-friendly label. Falls back to the title for `short_title` when
/// the model omits it.
pub fn parse_issue_draft(text: &str) -> Result<(String, String, String), String> {
    let braces = text.find('{').zip(text.rfind('}')).filter(|(s, e)| e > s);
    let Some((start, end)) = braces else {
        // No JSON object: claude replied conversationally (e.g. asking the user
        // to clarify a vague idea). Surface that reply verbatim so the caller
        // can show it and let the user decide.
        return Err(text.trim().chars().take(400).collect());
    };
    let v: serde_json::Value = serde_json::from_str(&text[start..=end])
        .map_err(|e| format!("could not parse the agent's reply as JSON ({e}): {}", snippet(text)))?;
    let title = v["title"].as_str().unwrap_or("").trim().to_string();
    let body = v["body"].as_str().unwrap_or("").trim().to_string();
    if title.is_empty() {
        return Err("the agent returned an empty title".into());
    }
    let short_title = v["short_title"].as_str().unwrap_or("").trim().to_string();
    let short_title = if short_title.is_empty() { default_short_title(&title) } else { short_title };
    Ok((title, body, short_title))
}

/// Correlates an agent run with the UI action that initiated it. The frontend
/// generates a `request_id` per command invocation and listens for the
/// `agent-activity` event: while a run is in flight for that id, the action's
/// busy glow switches to the rainbow (AI) variant, and back to the monochrome
/// one when it ends — so a mixed script/AI action changes color mid-flight.
pub struct AgentActivity {
    app: tauri::AppHandle,
    request_id: String,
}

impl AgentActivity {
    pub fn new(app: tauri::AppHandle, request_id: String) -> Self {
        Self { app, request_id }
    }

    /// Emit `active: true` now and `active: false` when the returned guard
    /// drops — every exit path (success, error, cancellation) ends the
    /// activity, so a failed draft never leaves a button stuck rainbow.
    fn begin(&self) -> AgentActivityGuard<'_> {
        self.emit(true);
        AgentActivityGuard(self)
    }

    fn emit(&self, active: bool) {
        use tauri::Emitter;
        let _ = self.app.emit(
            "agent-activity",
            serde_json::json!({ "request_id": self.request_id, "active": active }),
        );
    }
}

struct AgentActivityGuard<'a>(&'a AgentActivity);

impl Drop for AgentActivityGuard<'_> {
    fn drop(&mut self) {
        self.0.emit(false);
    }
}

/// How long a headless drafting run may take before it's killed.
const AGENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Run the repo's `agent` headlessly with `prompt`, in `dir` for repo context,
/// and return its reply text. `model` is the effective drafting model for that
/// agent (`None` = the agent's own default; see `prompts::model`). Both backends
/// run with **no tools**, so prompt injection from the input text or repo files
/// is contained to, at worst, a bad title/body the user reviews — never code
/// execution or exfiltration — and return the same `Result`, so callers only pass
/// the agent through.
pub async fn agent_text(
    agent: Agent,
    dir: &Path,
    prompt: &str,
    model: Option<&str>,
    what: &str,
    activity: Option<&AgentActivity>,
) -> Result<String, String> {
    let _active = activity.map(|a| a.begin());
    tracing::debug!(agent = %agent, model = model.unwrap_or("<default>"), "headless agent call: {what}");
    match agent {
        Agent::Claude => claude_text(dir, prompt, model.unwrap_or_default(), what).await,
        Agent::Codex => codex_text(dir, prompt, model, what).await,
    }
}

/// Claude backend. Uses `--output-format json` so we parse a stable envelope
/// rather than guessing at raw text, and surfaces stdout/stderr in errors when
/// something goes wrong.
///
/// `--tools ""` disables ALL tools: these calls only need to generate text, so
/// the model must not be able to read arbitrary files, run Bash, edit, or fetch
/// URLs — even though it runs in the real cloned repo/worktree with the user's
/// ambient permissions. That contains prompt injection from the input text (or
/// from repo files like CLAUDE.md, which is still loaded as context) to, at
/// worst, a bad title/body the user reviews — not code execution or exfiltration.
async fn claude_text(dir: &Path, prompt: &str, model: &str, what: &str) -> Result<String, String> {
    let run = crate::tools::tokio_command("claude")
        .current_dir(dir)
        .args(["-p", prompt, "--model", model, "--output-format", "json", "--tools", ""])
        // Kill the probe if the timeout below fires — a dropped future must not
        // leave a headless claude burning quota in the background.
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(AGENT_TIMEOUT, run)
        .await
        .map_err(|_| format!("claude timed out while {what}"))?
        .map_err(|e| format!("could not run claude (is it installed and on PATH?): {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!("claude exited with an error: {}", snippet(&stderr)));
    }

    // `--output-format json` wraps the reply in a result envelope.
    let envelope: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|e| {
        format!("could not parse claude output ({e}); stdout: {}; stderr: {}", snippet(&stdout), snippet(&stderr))
    })?;
    if envelope["is_error"].as_bool().unwrap_or(false) {
        return Err(format!("claude reported an error: {}", snippet(envelope["result"].as_str().unwrap_or(""))));
    }
    Ok(envelope["result"].as_str().unwrap_or("").to_string())
}

/// The `codex exec` arguments for a one-shot, tool-less text reply whose final
/// message lands in `out_file`. The prompt itself is read from stdin (`-`), so a
/// large PR diff never hits argv limits. Pure so the flag set is unit-tested.
///
/// - `--sandbox read-only` + `--disable shell_tool`/`unified_exec` and the other
///   tool features: Codex gets no shell, no apps/plugins/browser, no web search —
///   the same containment `--tools ""` gives the Claude backend.
/// - `--disable hooks`: a drafting run is not a session, so no hook the user or
///   the repo has configured (ours ride only on the session's launch command)
///   should fire for it — e.g. report the PR draft as the worktree's status.
/// - `--ephemeral`: don't litter the user's Codex session history with drafts.
/// - `--skip-git-repo-check`: the probe may run outside a checkout.
pub(crate) fn codex_exec_args(model: Option<&str>, out_file: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        "exec",
        "--skip-git-repo-check",
        "--ephemeral",
        "--sandbox",
        "read-only",
        "--color",
        "never",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for feature in [
        "shell_tool",
        "unified_exec",
        "apps",
        "plugins",
        "browser_use",
        "computer_use",
        "image_generation",
        "view_image",
        "hooks",
    ] {
        args.push("--disable".into());
        args.push(feature.into());
    }
    args.push("-c".into());
    args.push("web_search=\"disabled\"".into());
    if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
        args.push("--model".into());
        args.push(m.trim().to_string());
    }
    args.push("--output-last-message".into());
    args.push(out_file.to_string_lossy().into_owned());
    args.push("-".into());
    args
}

/// The most useful line of a failed `codex exec`'s stderr: the last `ERROR:`
/// line (Codex prints the API error there, sometimes as a JSON object whose
/// `error.message` is the readable part), else the whole stderr snippet.
pub(crate) fn codex_error_message(stderr: &str) -> String {
    let Some(line) = stderr.lines().rev().find_map(|l| l.trim().strip_prefix("ERROR:")) else {
        return snippet(stderr);
    };
    let line = line.trim();
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| snippet(line))
}

/// A unique scratch path for `--output-last-message`, removed after the run.
fn codex_out_file() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("maiestro-codex-{}-{n}.txt", std::process::id()))
}

/// Run `codex exec` with `args` in `dir`, feeding `prompt` on stdin. Shared by
/// the drafting backend and the health probe. Returns the process output.
pub(crate) async fn run_codex_exec(
    dir: Option<&Path>,
    args: &[String],
    prompt: &str,
    timeout: std::time::Duration,
) -> Result<Result<std::process::Output, std::io::Error>, tokio::time::error::Elapsed> {
    use tokio::io::AsyncWriteExt;
    let mut cmd = crate::tools::tokio_command("codex");
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let prompt = prompt.to_string();
    tokio::time::timeout(timeout, async move {
        let mut child = cmd.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes()).await?;
            // Dropping stdin closes it, so Codex sees EOF and starts the turn.
        }
        child.wait_with_output().await
    })
    .await
}

/// Codex backend: `codex exec` in read-only, tool-less, one-shot mode (see
/// [`codex_exec_args`]). The reply is read from `--output-last-message`, with
/// stdout (which carries only the final message when not a TTY) as a fallback.
async fn codex_text(dir: &Path, prompt: &str, model: Option<&str>, what: &str) -> Result<String, String> {
    let out_file = codex_out_file();
    let args = codex_exec_args(model, &out_file);
    let result = run_codex_exec(Some(dir), &args, prompt, AGENT_TIMEOUT).await;
    let reply = std::fs::read_to_string(&out_file).ok();
    let _ = std::fs::remove_file(&out_file);

    let output = result
        .map_err(|_| format!("codex timed out while {what}"))?
        .map_err(|e| format!("could not run codex (is it installed and on PATH?): {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("codex exited with an error: {}", codex_error_message(&stderr)));
    }
    let reply = reply
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| String::from_utf8_lossy(&output.stdout).to_string());
    if reply.trim().is_empty() {
        return Err(format!("codex returned an empty reply while {what}"));
    }
    Ok(reply.trim().to_string())
}

/// Ask Claude, running in the cloned repo for context, to turn the user's
/// free-text idea into an issue title + markdown body + a short label. The
/// `short_title` is produced in the *same* call (no extra Claude run): it's a
/// punchy branch/session label, distinct from the full issue title.
async fn draft_issue(
    cloned_repo: &Path,
    idea: &str,
    instruction: &str,
    agent: Agent,
    model: Option<&str>,
    activity: &AgentActivity,
) -> Result<(String, String, String), String> {
    // Instruction (default or per-repo override) first; the idea is appended
    // here so an override can't drop it. See prompts.rs.
    let prompt = format!("{instruction}\n\nIdea: {idea}");
    let reply = agent_text(agent, cloned_repo, &prompt, model, "drafting the issue", Some(activity)).await?;
    parse_issue_draft(&reply)
}

/// Pull a usable short label out of Claude's reply to `suggest_short_label`.
/// The prompt asks for the bare label, but the model may still wrap it in
/// quotes, backticks, or a code fence — strip those. A multi-line or over-long
/// reply means it rambled instead of labeling: error, the caller keeps the
/// heuristic label.
fn parse_short_label(text: &str) -> Result<String, String> {
    let mut lines = text.trim().lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("```"));
    let Some(line) = lines.next() else {
        return Err("the agent returned an empty label".into());
    };
    if lines.next().is_some() {
        return Err(format!("the agent replied with prose, not a label: {}", snippet(text)));
    }
    let label = line.trim_matches(|c| matches!(c, '"' | '\'' | '`')).trim();
    let label = label.trim_end_matches(|c: char| !c.is_alphanumeric()).trim();
    if label.is_empty() {
        return Err("the agent returned an empty label".into());
    }
    if label.chars().count() > 60 {
        return Err(format!("the agent's label is too long: {}", snippet(label)));
    }
    Ok(label.to_string())
}

/// Ask Claude, running in the cloned repo for context, to compress an existing
/// issue's title + body into a short session label. The spawn preview opens
/// immediately with the heuristic label and swaps this in when it arrives; any
/// error here just leaves the heuristic in place.
async fn suggest_short_label(
    cloned_repo: &Path,
    title: &str,
    body: &str,
    instruction: &str,
    agent: Agent,
    model: Option<&str>,
) -> Result<String, String> {
    // Issue bodies can be arbitrarily long; the label only needs the gist.
    let body: String = body.chars().take(4000).collect();
    // Instruction (default or per-repo override) first; the issue text is
    // appended here so an override can't drop it. See prompts.rs.
    let prompt = format!("{instruction}\n\nIssue title: {title}\n\nIssue body:\n{body}");
    // No activity signal: this is a fire-and-forget label swap with no busy
    // element in the UI to color.
    let reply = agent_text(agent, cloned_repo, &prompt, model, "summarizing the issue", None).await?;
    parse_short_label(&reply)
}

/// Suggest an AI short label for an existing issue (title + body via Claude).
/// Called fire-and-forget by the spawn preview after it opens with the
/// heuristic label; the frontend swallows errors, so failures here are benign.
#[tauri::command]
pub async fn suggest_short_title(repo: String, issue_number: u64) -> Result<String, String> {
    crate::log_invoke!("suggest_short_title", repo = %repo, issue = issue_number);
    let (settings, gh) = repo_context(&repo).await?;
    let cloned_repo = validated_cloned_repo(&settings)?;
    let (issue_title, _issue_url, issue_body) = crate::spawn::issue_facts(&gh, &repo, issue_number).await?;
    let instruction = crate::prompts::short_label(&settings.prompts);
    let agent = crate::repo_settings::effective_agent(&settings);
    let model = crate::prompts::model(&settings.prompt_models, agent);
    suggest_short_label(&cloned_repo, &issue_title, &issue_body, &instruction, agent, model.as_deref()).await
}

/// A drafted issue ready to create, or a signal that Claude couldn't produce a
/// clear draft and the user must confirm creating from raw text.
pub enum DraftStep {
    Ready {
        title: String,
        body: String,
        /// Punchy branch/session label produced in the same draft call.
        short_title: String,
    },
    NeedsConfirmation { message: String },
}

/// Resolve the repo's identity + cloned repo, then turn the idea into an issue
/// draft — via Claude, or (when `use_raw_fallback`) straight from the raw text.
/// Returns the authenticated GitHub client alongside the draft so callers can
/// create the issue. Used by `draft_spawn_preview`.
pub async fn resolve_draft(
    repo: &str,
    idea: &str,
    use_raw_fallback: bool,
    activity: &AgentActivity,
) -> Result<(GitHub, DraftStep), String> {
    let idea = idea.trim();
    if idea.is_empty() {
        return Err("Describe what you want to work on first.".into());
    }

    let (settings, gh) = repo_context(repo).await?;
    let cloned_repo = validated_cloned_repo(&settings)?;

    let step = if use_raw_fallback {
        let title = trim_to_word(idea, 70);
        let short_title = default_short_title(&title);
        DraftStep::Ready { title, body: idea.to_string(), short_title }
    } else {
        let instruction = crate::prompts::draft_issue(&settings.prompts);
        let agent = crate::repo_settings::effective_agent(&settings);
        let model = crate::prompts::model(&settings.prompt_models, agent);
        match draft_issue(&cloned_repo, idea, &instruction, agent, model.as_deref(), activity).await {
            Ok((title, body, short_title)) => DraftStep::Ready { title, body, short_title },
            // Couldn't draft: let the user confirm before creating anything.
            Err(message) => DraftStep::NeedsConfirmation { message },
        }
    };
    Ok((gh, step))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Codex drafting call is one-shot, read-only and tool-less, reads the
    /// prompt from stdin, and passes `--model` only when one is set.
    #[test]
    fn codex_exec_args_are_tool_less_and_model_optional() {
        let out = Path::new("/tmp/out.txt");
        let args = codex_exec_args(None, out);
        let joined = args.join(" ");
        assert_eq!(args[0], "exec");
        assert!(joined.contains("--sandbox read-only"), "{joined}");
        assert!(joined.contains("--disable shell_tool") && joined.contains("--disable unified_exec"), "{joined}");
        assert!(joined.contains("--disable hooks"), "{joined}");
        assert!(joined.contains("--output-last-message /tmp/out.txt"), "{joined}");
        assert_eq!(args.last().map(String::as_str), Some("-"), "prompt comes from stdin");
        assert!(!args.iter().any(|a| a == "--model"), "no model → Codex's own default");

        let args = codex_exec_args(Some("gpt-5-codex"), out);
        let i = args.iter().position(|a| a == "--model").expect("--model passed");
        assert_eq!(args[i + 1], "gpt-5-codex");
        assert!(!codex_exec_args(Some("  "), out).iter().any(|a| a == "--model"));
    }

    /// The readable part of a failed `codex exec`: a JSON API error's message,
    /// a plain `ERROR:` line, or the stderr snippet when there's neither.
    #[test]
    fn codex_error_message_extracts_the_api_error() {
        let bad_model = "user\nhi\nwarning: Model metadata not found\n\
            ERROR: {\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"invalid_request_error\",\"message\":\"The 'x' model is not supported.\"}}\n";
        assert_eq!(codex_error_message(bad_model), "The 'x' model is not supported.");
        let logged_out = "ERROR: Reconnecting... 5/5\nERROR: unexpected status 401 Unauthorized: Missing bearer\n";
        assert_eq!(codex_error_message(logged_out), "unexpected status 401 Unauthorized: Missing bearer");
        assert_eq!(codex_error_message("boom"), "boom");
        assert_eq!(codex_error_message(""), "<empty>");
    }

    /// A bare label parses; quote/backtick/fence wrapping and trailing
    /// punctuation are stripped.
    #[test]
    fn short_label_parses_and_unwraps() {
        assert_eq!(parse_short_label("Auth token refresh").unwrap(), "Auth token refresh");
        assert_eq!(parse_short_label("  \"Auth token refresh.\"  ").unwrap(), "Auth token refresh");
        assert_eq!(parse_short_label("`Resizable popover`").unwrap(), "Resizable popover");
        assert_eq!(parse_short_label("```\nAI workspace labels\n```").unwrap(), "AI workspace labels");
    }

    /// Empty, multi-line (prose), and over-long replies are rejected — the
    /// caller falls back to the heuristic label.
    #[test]
    fn short_label_rejects_prose() {
        assert!(parse_short_label("").is_err());
        assert!(parse_short_label("\"\"").is_err());
        assert!(parse_short_label("Here are some options:\n- Auth refresh\n- Token renewal").is_err());
        assert!(parse_short_label(&"long ".repeat(20)).is_err());
    }

    /// A JSON draft (bare or fenced) yields title/body/short_title; a missing
    /// short_title falls back to a heuristic from the title; no-JSON prose and an
    /// empty title are rejected.
    #[test]
    fn parse_issue_draft_extracts_or_falls_back() {
        let (t, b, s) = parse_issue_draft(r#"{"title":"Add foo","body":"Do it","short_title":"foo"}"#).unwrap();
        assert_eq!((t.as_str(), b.as_str(), s.as_str()), ("Add foo", "Do it", "foo"));

        // Fenced + prose around it, and a missing short_title → derived from title.
        let (t, _b, s) = parse_issue_draft("Sure!\n```json\n{\"title\":\"Fix the bug!\",\"body\":\"x\"}\n```").unwrap();
        assert_eq!(t, "Fix the bug!");
        assert_eq!(s, "Fix the bug");

        // No JSON at all: the conversational reply is surfaced as the error.
        assert!(parse_issue_draft("Can you clarify what you mean?").is_err());
        // JSON present but empty title.
        assert!(parse_issue_draft(r#"{"title":"","body":"x"}"#).is_err());
    }
}

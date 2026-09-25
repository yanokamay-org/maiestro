//! Per-repo prerequisite health check (issue #93).
//!
//! `repo_health_check` runs a set of informational diagnostics for one tracked
//! repo — cloned checkout, the CLIs mAIestro Code invokes (`git`, the repo's agent
//! — `claude` or `codex`, never both — and `code`),
//! the GitHub token *and the permissions it grants*, the configured env files,
//! the worktree terminal font, and (trailing, issue #146) how old each directly
//! invoked CLI is against a hardcoded minimum — and returns a `HealthReport` the
//! Settings window renders in a modal.
//!
//! The checks never mutate anything and never block spawning; they surface
//! likely problems early instead of letting them fail mid-spawn. The GitHub
//! permission check is the notable one: rather than performing a throwaway write
//! to prove write access, it *derives* the verdict from the token's granted
//! OAuth scopes (classic PATs, via `X-OAuth-Scopes`) or the repo's `permissions`
//! object (fine-grained tokens) — see the module's `github` sub-check.

use crate::agent::Agent;
use crate::paths::expand_tilde;
use crate::plugins::GitHub;
use crate::repo_settings;
use crate::tools::snippet;

#[derive(serde::Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Pass,
    Fail,
    Warn,
    /// Not a problem — something worth knowing. Used where a check found a
    /// perfectly workable setup that the user might still want to change (the
    /// terminal font falling back to a stock face, say). Distinct from `Warn`,
    /// which means "this will probably bite you", so an informational row never
    /// makes the report look broken.
    Info,
    Skipped,
}

#[derive(serde::Serialize)]
pub struct HealthCheck {
    /// Stable id (e.g. "cloned_repo", "github_token") for the frontend.
    pub id: String,
    pub label: String,
    pub status: HealthStatus,
    /// Human-readable context: a resolved path, a login, or an error message.
    pub detail: String,
    /// Nested checks, used by the GitHub check to break out token validity,
    /// repo read access, and write-permission verdict.
    pub sub: Vec<HealthCheck>,
    /// A suggested shell command to remediate a failure (e.g. a `git clone` when
    /// the repo isn't checked out). Rendered as a copyable monospace line; absent
    /// for checks with no obvious one-liner fix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl HealthCheck {
    fn new(id: &str, label: &str, status: HealthStatus, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status,
            detail: detail.into(),
            sub: Vec::new(),
            command: None,
        }
    }

    /// Attach a suggested remediation command (see `command`).
    fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }
}

#[derive(serde::Serialize)]
pub struct HealthReport {
    pub repo: String,
    pub checks: Vec<HealthCheck>,
}

/// Run all prerequisite checks for `repo` ("owner/name") and return the report.
/// Only fails as a whole if the repo's settings file can't be loaded — every
/// individual prerequisite is reported as a check row, never an early error.
#[tauri::command]
pub async fn repo_health_check(window: tauri::Window, repo: String) -> Result<HealthReport, String> {
    crate::log_invoke!("repo_health_check", repo = %repo);
    let settings = repo_settings::repo_settings_get(repo.clone())?;

    // Only the repo's agent is checked (login, model, CLI version): a Codex repo
    // never probes, lists, or version-checks `claude`, and vice versa, so a
    // single-agent machine gets a clean report.
    let agent = repo_settings::effective_agent(&settings);
    // The model mAIestro Code's own drafting calls would use — the agent probe
    // runs against it so it doubles as a "is this model available?" check.
    let model = crate::prompts::model(&settings.prompt_models, agent);
    // Run the probe inside the cloned repo when it exists (agent auth is global,
    // so cwd only needs to be a real directory); otherwise let it inherit ours.
    let agent_cwd = settings
        .cloned_repo_dir
        .as_deref()
        .map(expand_tilde)
        .filter(|p| p.is_dir());

    // Run the checks one at a time and emit each result the moment it lands, so
    // the popover streams rows (and shows a live spinner for the running one)
    // instead of waiting for the whole batch. `total` lets the UI stop spinning.
    let mut checks: Vec<HealthCheck> = Vec::new();
    let total = 8;
    // Each `step!` announces the check's title (so the spinner can name what's
    // running) *before* running it, then emits the result. The title here must
    // match the label the check function produces — the result event carries the
    // authoritative label, so any drift only shows for the in-flight moment.
    macro_rules! step {
        ($label:expr, $e:expr) => {{
            emit_running(&window, &repo, checks.len(), total, $label);
            let check = $e;
            emit_check(&window, &repo, checks.len(), total, &check);
            checks.push(check);
        }};
    }
    step!("Cloned repo exists", check_cloned_repo(&repo, settings.cloned_repo_dir.as_deref()).await);
    step!("Git available", check_cli("git", "Git available"));
    // The agent probe returns one row ("Claude logged in" / "Codex logged in")
    // with the model check nested as a sub — logged-in and model-available are
    // distinct facts, and the parent stays green (login) even when the model
    // sub fails.
    let login_label = format!("{} logged in", agent.display_name());
    match agent {
        Agent::Claude => step!(
            login_label,
            check_claude(&repo, model.as_deref().unwrap_or_default(), agent_cwd.as_deref()).await
        ),
        Agent::Codex => step!(login_label, check_codex(&repo, model.as_deref(), agent_cwd.as_deref()).await),
    }
    step!("GitHub token & permissions", check_github(&repo, settings.identity_id.as_deref()).await);
    step!("Session editor available", check_editor());
    step!("Configured env files exist", check_env_files(settings.cloned_repo_dir.as_deref(), &settings.env_files));
    step!("Terminal font installed", check_terminal_font());
    step!("Tool versions", check_tool_versions(&repo, agent).await);

    Ok(HealthReport { repo, checks })
}

/// Announce the check about to run as a `health-check-running` event, so the
/// spinner can name what's executing. `index` is how many have already finished.
fn emit_running(window: &tauri::Window, repo: &str, index: usize, total: usize, label: impl AsRef<str>) {
    use tauri::Emitter;
    let _ = window.emit(
        "health-check-running",
        serde_json::json!({ "repo": repo, "index": index, "total": total, "label": label.as_ref() }),
    );
}

/// Log an external command the health check is about to run, at `info` so it's
/// visible in the default log (health checks are manual and infrequent, so this
/// doesn't spam). Never logs secrets — the commands here carry none.
fn log_command(repo: &str, command: &str) {
    tracing::info!(target: "health", repo = %repo, command = %command, "health-check command");
}

/// Emit one completed check to the popover as a `health-check` event. The UI
/// keys on `repo` (so a stale run's events are ignored) and uses `index`/`total`
/// to know when to stop the spinner. Best-effort — a failed emit never aborts the
/// remaining checks (the command's return value is the source of truth).
fn emit_check(window: &tauri::Window, repo: &str, index: usize, total: usize, check: &HealthCheck) {
    use tauri::Emitter;
    let _ = window.emit(
        "health-check",
        serde_json::json!({ "repo": repo, "index": index, "total": total, "check": check }),
    );
}

/// `cloned_repo_dir` is set, exists, and is a valid git repository whose `origin`
/// remote points at `repo` ("owner/name"). The remote-match is a sub-check, so a
/// checkout that exists but points at a *different* project fails distinctly from
/// a missing one.
async fn check_cloned_repo(repo: &str, cloned_repo_dir: Option<&str>) -> HealthCheck {
    let id = "cloned_repo";
    let label = "Cloned repo exists";
    let Some(dir) = cloned_repo_dir.filter(|d| !d.trim().is_empty()) else {
        // No target dir configured, so suggest cloning into the conventional
        // location (`~/src/<name>`, mAIestro Code's default cloned_repo_dir).
        let name = repo.rsplit('/').next().unwrap_or(repo);
        return HealthCheck::new(id, label, HealthStatus::Fail, "No cloned_repo_dir configured")
            .with_command(clone_command(repo, &format!("~/src/{name}")));
    };
    let path = expand_tilde(dir);
    if !path.is_dir() {
        // Not checked out yet — show how to clone it into the configured dir.
        return HealthCheck::new(id, label, HealthStatus::Fail, format!("Not found: {}", path.display()))
            .with_command(clone_command(repo, dir));
    }
    // A directory alone isn't enough — confirm it's actually a git checkout.
    log_command(repo, &format!("git -C {} rev-parse --git-dir", path.display()));
    let is_git = crate::tools::tokio_command("git")
        .arg("-C")
        .arg(&path)
        .args(["rev-parse", "--git-dir"])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !is_git {
        return HealthCheck::new(id, label, HealthStatus::Fail, format!("Not a git repository: {}", path.display()))
            .with_command(clone_command(repo, dir));
    }
    // Valid checkout: also confirm its `origin` points at this repo, so a
    // `cloned_repo_dir` aimed at the wrong project is caught here.
    let mut check = HealthCheck::new(id, label, HealthStatus::Pass, path.display().to_string());
    check.sub.push(check_remote_matches(repo, &path).await);
    check.status = rollup(&check.sub);
    check
}

/// The checkout's `origin` remote URL resolves to `repo` ("owner/name").
async fn check_remote_matches(repo: &str, path: &std::path::Path) -> HealthCheck {
    let id = "cloned_repo_remote";
    let label = "Remote matches this repo";
    log_command(repo, &format!("git -C {} remote get-url origin", path.display()));
    let out = crate::tools::tokio_command("git")
        .arg("-C")
        .arg(path)
        .args(["remote", "get-url", "origin"])
        .output()
        .await;
    let url = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        // Non-zero exit is git's "No such remote 'origin'".
        Ok(_) => return HealthCheck::new(id, label, HealthStatus::Fail, "No `origin` remote configured"),
        Err(e) => return HealthCheck::new(id, label, HealthStatus::Warn, format!("Couldn't read origin: {e}")),
    };
    match remote_owner_name(&url) {
        Some(on) if on.eq_ignore_ascii_case(repo) => {
            HealthCheck::new(id, label, HealthStatus::Pass, format!("origin → {url}"))
        }
        Some(on) => HealthCheck::new(
            id,
            label,
            HealthStatus::Fail,
            format!("origin is {on}, expected {repo} ({url})"),
        ),
        None => HealthCheck::new(id, label, HealthStatus::Warn, format!("Unrecognized origin URL: {url}")),
    }
}

/// Extract "owner/name" from a git remote URL, covering the HTTPS
/// (`https://github.com/owner/name.git`), SSH (`ssh://git@github.com/owner/name`),
/// and scp-style (`git@github.com:owner/name.git`) forms by taking the last two
/// path segments. Returns `None` if fewer than two segments are present.
fn remote_owner_name(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let segments: Vec<&str> = trimmed
        .split(['/', ':'])
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() < 2 {
        return None;
    }
    let name = segments[segments.len() - 1];
    let owner = segments[segments.len() - 2];
    Some(format!("{owner}/{name}"))
}

/// A sample `git clone` command for `repo` ("owner/name") into `dir` (kept as the
/// user typed it, `~` and all, so it's a copy-paste-ready shell command). Uses the
/// HTTPS remote: it needs no credentials at all for a public repo, and falls back
/// to whatever credential helper git already has for a private one. The SSH form
/// this used to emit silently assumed a GitHub SSH key, so on a machine that only
/// ever authenticates over HTTPS the hint was a command that could never work.
fn clone_command(repo: &str, dir: &str) -> String {
    format!("git clone https://github.com/{repo}.git {dir}")
}

/// A directly-invoked CLI (`git`, `claude`) resolves to a real file. Because a
/// configured `tool_paths` override is authoritative (see `tools::find_tool`), a
/// pin that doesn't exist is a hard **fail** naming the missing path — never a
/// silent fall-through to a different binary on PATH.
fn check_cli(tool: &str, label: &str) -> HealthCheck {
    let id = tool;
    match crate::tools::find_tool(tool) {
        Some(p) => HealthCheck::new(id, label, HealthStatus::Pass, p.display().to_string()),
        None => HealthCheck::new(id, label, HealthStatus::Fail, tool_not_found_detail(tool)),
    }
}

/// Explain a "not found" for a resolved tool: a configured-but-missing override
/// says so (and names the path); otherwise it's a plain not-on-PATH.
fn tool_not_found_detail(tool: &str) -> String {
    match crate::tools::stale_override(tool) {
        Some(stale) => format!("Configured path not found: {stale}"),
        None => format!("`{tool}` not found on PATH"),
    }
}

/// Probe Claude once and return the **logged in** (auth) check with a nested
/// **model available** sub-check, so the two failure modes are distinguishable
/// while staying visually grouped. Both come from a single `claude -p … --model
/// <model>` run:
/// - non-zero exit → auth failed (not logged in); model check is Skipped.
/// - `is_error` with a `401`/`403` status → auth failed; model Skipped.
/// - `is_error` with a *null* status (no HTTP request completed — claude bailed
///   out locally, e.g. "Not logged in · Please run /login") → auth failed; model
///   Skipped. This is the logged-out case: it exits 0 with an error envelope but
///   *no* status, so it must not be mistaken for a model problem.
/// - `is_error` with any other status (e.g. `404`) → auth *worked* (the API
///   answered), so login Passes and the **model** check Fails with the message.
/// - clean reply → both Pass.
///
/// A 404 authenticating-but-unknown-model is exactly the case worth separating:
/// the user is logged in fine, they just picked a `prompt_model` that isn't there.
///
/// If a `tool_paths.claude` override is configured but missing, resolution fails
/// outright (the override is authoritative — no PATH fallback), so this reports a
/// hard login **Fail** naming the pinned path rather than probing some other claude.
async fn check_claude(repo: &str, model: &str, cwd: Option<&std::path::Path>) -> HealthCheck {
    let login_id = "claude_login";
    let login_label = "Claude logged in";
    let model_id = "claude_model";
    let model_label = format!("Model `{model}` available");

    let model_skipped = |detail: &str| {
        HealthCheck::new(model_id, &model_label, HealthStatus::Skipped, detail)
    };
    // The model check hangs off the login check as a sub. The parent keeps the
    // *login* status (not a roll-up), so "Claude logged in" stays green even when
    // the model sub fails — you are logged in; you just picked a bad model.
    let nest = |mut login: HealthCheck, model: HealthCheck| {
        login.sub.push(model);
        login
    };

    // Resolve first so "not installed" (or a missing pinned override) is a
    // distinct, fast failure that names the offending path.
    let Some(bin) = crate::tools::find_tool("claude") else {
        return nest(
            HealthCheck::new(login_id, login_label, HealthStatus::Fail, tool_not_found_detail("claude")),
            model_skipped("claude not found"),
        );
    };
    let login_command = format!("{} login", bin.display());

    let mut cmd = crate::tools::tokio_command("claude");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    // Keep it cheap and deterministic: no tools, JSON envelope (so we can read
    // `is_error`/`api_error_status`), a one-word reply.
    cmd.args([
        "-p",
        "Reply with exactly: ok",
        "--model",
        model,
        "--output-format",
        "json",
        "--tools",
        "",
    ]);
    log_command(repo, &format!("claude -p 'Reply with exactly: ok' --model {model} --output-format json --tools ''"));
    // Kill the probe if the timeout fires so it doesn't linger in the background.
    cmd.kill_on_drop(true);
    let run = tokio::time::timeout(std::time::Duration::from_secs(60), cmd.output()).await;

    let output = match run {
        Err(_) => {
            return nest(
                HealthCheck::new(login_id, login_label, HealthStatus::Warn, "claude timed out after 60s"),
                model_skipped("claude timed out"),
            );
        }
        Ok(Err(e)) => {
            return nest(
                HealthCheck::new(login_id, login_label, HealthStatus::Fail, format!("Couldn't run claude: {e}")),
                model_skipped("claude couldn't run"),
            );
        }
        Ok(Ok(o)) => o,
    };

    // The JSON envelope is authoritative, so parse it *before* looking at the
    // exit code: claude exits non-zero on an API error too (a bad model is exit 1
    // with `is_error`/`api_error_status: 404`), so a non-zero exit alone does NOT
    // mean "not logged in". Only a *missing* envelope falls back to the exit code.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Ok(env) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        // No envelope to interpret. A non-zero exit here is the genuine
        // couldn't-run / not-logged-in case; a 0 exit means it ran but we can't
        // judge the model.
        if output.status.success() {
            return nest(
                HealthCheck::new(login_id, login_label, HealthStatus::Pass, "Logged in"),
                model_skipped("Couldn't parse claude output"),
            );
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = if stderr.trim().is_empty() { snippet(&stdout) } else { snippet(&stderr) };
        return nest(
            HealthCheck::new(login_id, login_label, HealthStatus::Fail, format!("Likely not logged in: {detail}"))
                .with_command(login_command),
            model_skipped("Claude not logged in"),
        );
    };

    classify_claude_envelope(&env, model, &login_command)
}

/// Turn a *parsed* claude result envelope into the nested login/model check.
/// Pure (no I/O) so the auth-vs-model decision — the part that actually had the
/// logged-out bug — is unit-tested directly. See `check_claude` for the mapping.
fn classify_claude_envelope(env: &serde_json::Value, model: &str, login_command: &str) -> HealthCheck {
    let login_id = "claude_login";
    let login_label = "Claude logged in";
    let model_id = "claude_model";
    let model_label = format!("Model `{model}` available");
    let model_skipped =
        |detail: &str| HealthCheck::new(model_id, &model_label, HealthStatus::Skipped, detail);
    let nest = |mut login: HealthCheck, model: HealthCheck| {
        login.sub.push(model);
        login
    };
    let login_fail = |detail: String| {
        nest(
            HealthCheck::new(login_id, login_label, HealthStatus::Fail, detail)
                .with_command(login_command.to_string()),
            model_skipped("Claude not logged in"),
        )
    };

    if !env["is_error"].as_bool().unwrap_or(false) {
        return nest(
            HealthCheck::new(login_id, login_label, HealthStatus::Pass, "Logged in"),
            HealthCheck::new(model_id, &model_label, HealthStatus::Pass, format!("`{model}` responded")),
        );
    }

    // An error envelope. `api_error_status` is the HTTP status of an *actual* API
    // response, so it tells auth from model — but only when it's present:
    // - 401/403: credentials were rejected → login Fails, model Skipped.
    // - a *null* status: no HTTP request ever completed; claude bailed out
    //   locally *before* authenticating (e.g. "Not logged in · Please run
    //   /login", or an offline/network error). We can't conclude auth worked, so
    //   this is a login Fail, not a model problem. This is the logged-out case.
    // - anything else (notably 404): the request authenticated but the model was
    //   the problem → login Passes, model Fails with the message.
    let status = env["api_error_status"].as_u64();
    let message = snippet(env["result"].as_str().unwrap_or(""));
    match status {
        Some(401) | Some(403) => login_fail(format!("Not authorized: {message}")),
        None => login_fail(format!("Likely not logged in: {message}")),
        Some(_) => nest(
            HealthCheck::new(login_id, login_label, HealthStatus::Pass, "Logged in"),
            HealthCheck::new(model_id, &model_label, HealthStatus::Fail, message),
        ),
    }
}

/// Probe Codex and return the **Codex logged in** check with a nested **model
/// available** sub-check, mirroring [`check_claude`]'s shape:
/// - `codex` doesn't resolve (or a pinned `tool_paths.codex` is missing) → login
///   Fail naming the path; model Skipped.
/// - `codex login status` exits non-zero → login Fail with `codex login` as the
///   fix; model Skipped. This runs *first* because a logged-out `codex exec`
///   retries for a while before failing.
/// - Logged in, no drafting model configured (`prompt_models.codex` is `null`) →
///   the model row is Info: drafting uses Codex's own configured model, which
///   the login probe already vouches for.
/// - Logged in with a model → a tiny tool-less `codex exec --model <m>` round
///   trip; a failure there is a model Fail (you're logged in; the model is the
///   problem), with Codex's API error as the detail.
async fn check_codex(repo: &str, model: Option<&str>, cwd: Option<&std::path::Path>) -> HealthCheck {
    let login_id = "codex_login";
    let login_label = "Codex logged in";
    let model_id = "codex_model";
    let model_label = match model {
        Some(m) => format!("Model `{m}` available"),
        None => "Drafting model".to_string(),
    };
    let nest = |mut login: HealthCheck, model: HealthCheck| {
        login.sub.push(model);
        login
    };
    let model_row = |status: HealthStatus, detail: &str| HealthCheck::new(model_id, &model_label, status, detail);

    let Some(bin) = crate::tools::find_tool("codex") else {
        return nest(
            HealthCheck::new(login_id, login_label, HealthStatus::Fail, tool_not_found_detail("codex")),
            model_row(HealthStatus::Skipped, "codex not found"),
        );
    };
    let login_command = format!("{} login", bin.display());

    log_command(repo, "codex login status");
    let mut cmd = crate::tools::tokio_command("codex");
    cmd.args(["login", "status"]).kill_on_drop(true);
    let status = match tokio::time::timeout(std::time::Duration::from_secs(20), cmd.output()).await {
        Err(_) => {
            return nest(
                HealthCheck::new(login_id, login_label, HealthStatus::Warn, "`codex login status` timed out after 20s"),
                model_row(HealthStatus::Skipped, "codex timed out"),
            );
        }
        Ok(Err(e)) => {
            return nest(
                HealthCheck::new(login_id, login_label, HealthStatus::Fail, format!("Couldn't run codex: {e}")),
                model_row(HealthStatus::Skipped, "codex couldn't run"),
            );
        }
        Ok(Ok(o)) => o,
    };
    let status_text = {
        let out = String::from_utf8_lossy(&status.stdout);
        let err = String::from_utf8_lossy(&status.stderr);
        if out.trim().is_empty() { snippet(&err) } else { snippet(&out) }
    };
    if !status.status.success() {
        return nest(
            HealthCheck::new(login_id, login_label, HealthStatus::Fail, status_text).with_command(login_command),
            model_row(HealthStatus::Skipped, "Codex not logged in"),
        );
    }
    let login = HealthCheck::new(login_id, login_label, HealthStatus::Pass, status_text);

    let Some(model) = model else {
        return nest(
            login,
            model_row(HealthStatus::Info, "No drafting model set — Codex uses the model configured in Codex itself"),
        );
    };

    let out_file = std::env::temp_dir().join(format!("maiestro-codex-health-{}.txt", std::process::id()));
    let args = crate::drafting::codex_exec_args(Some(model), &out_file);
    log_command(repo, &format!("codex exec --model {model} … 'Reply with exactly: ok'"));
    let run = crate::drafting::run_codex_exec(cwd, &args, "Reply with exactly: ok", std::time::Duration::from_secs(60)).await;
    let _ = std::fs::remove_file(&out_file);
    let model_check = match run {
        Err(_) => model_row(HealthStatus::Warn, "codex timed out after 60s"),
        Ok(Err(e)) => model_row(HealthStatus::Warn, &format!("Couldn't run codex: {e}")),
        Ok(Ok(o)) if o.status.success() => model_row(HealthStatus::Pass, &format!("`{model}` responded")),
        Ok(Ok(o)) => model_row(
            HealthStatus::Fail,
            &crate::drafting::codex_error_message(&String::from_utf8_lossy(&o.stderr)),
        ),
    };
    nest(login, model_check)
}

/// The session editor. Today mAIestro Code always launches VS Code (`open_vscode`),
/// preferring the `code` CLI and falling back to the app bundle — so mirror that:
/// pass if the `code` CLI resolves, warn (with the fallback still viable) if not.
fn check_editor() -> HealthCheck {
    let id = "editor";
    let label = "Session editor available";
    match crate::tools::find_tool("code") {
        Some(p) => HealthCheck::new(id, label, HealthStatus::Pass, p.display().to_string()),
        // A configured-but-missing `code` pin is authoritative — fail naming it,
        // rather than falling back to launching VS Code.app via `open -a`.
        None if crate::tools::stale_override("code").is_some() => {
            HealthCheck::new(id, label, HealthStatus::Fail, tool_not_found_detail("code"))
        }
        None if std::path::Path::new("/Applications/Visual Studio Code.app").is_dir() => HealthCheck::new(
            id,
            label,
            HealthStatus::Warn,
            "`code` CLI not found; will fall back to launching Visual Studio Code.app",
        ),
        None => HealthCheck::new(id, label, HealthStatus::Fail, "Visual Studio Code not found"),
    }
}

/// Every configured env file (resolved relative to `cloned_repo_dir`, as the
/// spawn path copies them) exists. Pass with none configured; warn listing any
/// missing.
fn check_env_files(cloned_repo_dir: Option<&str>, env_files: &[String]) -> HealthCheck {
    let id = "env_files";
    let label = "Configured env files exist";
    if env_files.is_empty() {
        return HealthCheck::new(id, label, HealthStatus::Pass, "None configured");
    }
    let base_dir = cloned_repo_dir.map(expand_tilde);
    // An entry that isn't a contained relative path (absolute, or `..`-escaping)
    // is silently skipped by the spawn copy (`is_contained_relpath`), so flag it
    // here with the same predicate rather than resolving it and reporting it as
    // "present" — `base.join(<absolute>)` would otherwise mask the problem.
    let mut invalid: Vec<&String> = Vec::new();
    let mut missing: Vec<&String> = Vec::new();
    for rel in env_files {
        if !crate::paths::is_contained_relpath(rel) {
            invalid.push(rel);
            continue;
        }
        let path = match &base_dir {
            Some(base) => base.join(rel.as_str()),
            None => expand_tilde(rel),
        };
        if !path.exists() {
            missing.push(rel);
        }
    }
    let join = |v: &[&String]| v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
    if !invalid.is_empty() {
        HealthCheck::new(
            id,
            label,
            HealthStatus::Warn,
            format!("Not a contained relative path (skipped at spawn): {}", join(&invalid)),
        )
    } else if !missing.is_empty() {
        HealthCheck::new(id, label, HealthStatus::Warn, format!("Missing: {}", join(&missing)))
    } else {
        HealthCheck::new(id, label, HealthStatus::Pass, format!("{} present", env_files.len()))
    }
}

// ── Terminal font ───────────────────────────────────────────────────────────
//
// The `terminal_font_family` preference is written into every spawned worktree's
// `.vscode/settings.json`. Claude Code's TUI draws box-drawing and powerline
// glyphs, which only a patched (Nerd) font renders — but an unpatched machine
// still gets a perfectly usable terminal from the stack's stock fallback. So a
// missing Nerd Font is reported as `Info`, never a failure: nothing is broken,
// the user just might want to install it.
//
// Detection is a filename scan of the macOS font directories rather than
// CoreText or `system_profiler` (seconds slow) — no new dependency, and fast
// enough to sit in a health run.

/// Directories macOS loads fonts from, in search order: the user's own, the
/// machine's, and the system's (whose `Supplemental` subdirectory holds many of
/// the stock faces, so each directory is scanned one level deep).
fn font_dirs() -> Vec<std::path::PathBuf> {
    vec![
        crate::paths::home().join("Library/Fonts"),
        std::path::PathBuf::from("/Library/Fonts"),
        std::path::PathBuf::from("/System/Library/Fonts"),
    ]
}

/// Normalized form for comparing a font family to a font *file* name: lowercase,
/// with everything but letters and digits dropped. Collapses the many ways the
/// same family is written — `JetBrainsMono Nerd Font` vs.
/// `JetBrainsMonoNerdFont-Regular.ttf` — into one comparable token.
fn normalize_font_name(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

/// Generic CSS font families — aliases the browser/editor resolves itself, not
/// installable faces, so they're excluded from "is it installed?".
const GENERIC_FONT_FAMILIES: &[&str] = &["monospace", "serif", "sansserif", "cursive", "fantasy", "systemui"];

/// The concrete families named by a CSS `font-family` stack, in order, unquoted
/// and with the generic aliases dropped.
fn font_families(stack: &str) -> Vec<String> {
    stack
        .split(',')
        .map(|f| f.trim().trim_matches(['\'', '"']).trim().to_string())
        .filter(|f| !f.is_empty() && !GENERIC_FONT_FAMILIES.contains(&normalize_font_name(f).as_str()))
        .collect()
}

/// Normalized names of every font file installed in [`font_dirs`]. Unreadable
/// directories are skipped — a missing `~/Library/Fonts` just means no user fonts.
fn installed_font_names() -> Vec<String> {
    let mut out = Vec::new();
    let push_dir = |dir: &std::path::Path, out: &mut Vec<String>, sub: &mut Vec<std::path::PathBuf>| {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                sub.push(path);
            } else if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.push(normalize_font_name(stem));
            }
        }
    };
    for dir in font_dirs() {
        let mut sub = Vec::new();
        push_dir(&dir, &mut out, &mut sub);
        // One level deeper (e.g. /System/Library/Fonts/Supplemental).
        for d in sub {
            push_dir(&d, &mut out, &mut Vec::new());
        }
    }
    out
}

/// Whether `family` is among `installed`. A font file is named for the family
/// *plus* its style (`JetBrainsMonoNerdFont-Regular`), so the family matches when
/// it's a prefix of the file name.
fn font_installed(family: &str, installed: &[String]) -> bool {
    let want = normalize_font_name(family);
    !want.is_empty() && installed.iter().any(|f| f.starts_with(&want))
}

/// The Homebrew cask that installs `family`, for the remediation one-liner. A
/// cask name can't be derived from a family name (`JetBrainsMono Nerd Font` ships
/// as `font-jetbrains-mono-nerd-font`), so this is a small verified table rather
/// than a guess — an unknown family gets no command, which beats a wrong one.
fn font_install_command(family: &str) -> Option<String> {
    let cask = match normalize_font_name(family).as_str() {
        "jetbrainsmononerdfont" => "font-jetbrains-mono-nerd-font",
        "hacknerdfont" => "font-hack-nerd-font",
        "firacodenerdfont" => "font-fira-code-nerd-font",
        "meslolgnerdfont" | "meslolgsnf" => "font-meslo-lg-nerd-font",
        "caskaydiacovenerdfont" => "font-caskaydia-cove-nerd-font",
        "saucecodepronerdfont" => "font-sauce-code-pro-nerd-font",
        "symbolsnerdfont" | "symbolsnerdfontmono" => "font-symbols-only-nerd-font",
        "cascadiacode" => "font-cascadia-code",
        _ => return None,
    };
    Some(format!("brew install --cask {cask}"))
}

/// Is the preferred terminal font actually installed? Informational: the font
/// stack always degrades to a stock face, so a missing Nerd Font costs glyphs,
/// not a working session. `Warn` is reserved for the one genuinely broken case —
/// *no* family in the configured stack is installed, which means the user typed
/// something that resolves to nothing.
fn check_terminal_font() -> HealthCheck {
    terminal_font_check(&crate::app_settings::terminal_font_family(), &installed_font_names())
}

/// The verdict itself, split from the filesystem scan so every branch is
/// unit-testable against a fixed set of installed fonts.
fn terminal_font_check(stack: &str, installed: &[String]) -> HealthCheck {
    let id = "terminal_font";
    let label = "Terminal font installed";
    let families = font_families(stack);
    let Some(preferred) = families.first() else {
        return HealthCheck::new(id, label, HealthStatus::Warn, format!("No font family configured: {stack}"));
    };

    if font_installed(preferred, installed) {
        return HealthCheck::new(id, label, HealthStatus::Pass, preferred.clone());
    }

    let fallback = families.iter().skip(1).find(|f| font_installed(f, installed));
    let detail = match fallback {
        Some(f) => format!(
            "{preferred} isn't installed — the terminal falls back to {f}. \
             Claude Code's box and powerline glyphs may not render."
        ),
        None => format!("None of the configured fonts are installed: {stack}"),
    };
    let status = if fallback.is_some() { HealthStatus::Info } else { HealthStatus::Warn };
    let check = HealthCheck::new(id, label, status, detail);
    match font_install_command(preferred) {
        Some(cmd) => check.with_command(cmd),
        None => check,
    }
}

/// The GitHub token check: validity + read access + a derived write-permission
/// verdict, as three sub-checks under one parent. Uses the repo's assigned
/// identity token; skipped entirely if no identity is assigned. Never mutates
/// the repo — write access is inferred, not exercised (see #93).
async fn check_github(repo: &str, identity_id: Option<&str>) -> HealthCheck {
    let id = "github_token";
    let label = "GitHub token & permissions";

    let Some(identity) = identity_id.filter(|i| !i.trim().is_empty()) else {
        return HealthCheck::new(id, label, HealthStatus::Skipped, "No identity assigned to this repo");
    };
    let gh = match GitHub::for_identity(identity).await {
        Ok(gh) => gh,
        Err(e) => return HealthCheck::new(id, label, HealthStatus::Fail, e),
    };

    // 1. Token validity + scopes.
    let token_info = match gh.check_token().await {
        Ok(info) => info,
        Err(e) => {
            let mut parent = HealthCheck::new(id, label, HealthStatus::Fail, "Token check failed");
            parent.sub.push(HealthCheck::new("token_valid", "Token valid", HealthStatus::Fail, e));
            return parent;
        }
    };
    let mut sub = vec![HealthCheck::new(
        "token_valid",
        "Token valid",
        HealthStatus::Pass,
        format!("Authenticated as {}", token_info.login),
    )];

    // 2. Repo read access — also the source of the `permissions` object used to
    //    judge write access for fine-grained tokens.
    let repo_json = gh.repo(repo).await;
    let (read_ok, repo_json) = match repo_json {
        Ok(v) => {
            sub.push(HealthCheck::new(
                "repo_read",
                "Repo readable",
                HealthStatus::Pass,
                format!("Read access to {repo}"),
            ));
            (true, Some(v))
        }
        Err(e) => {
            sub.push(HealthCheck::new("repo_read", "Repo readable", HealthStatus::Fail, e));
            (false, None)
        }
    };

    // 3. Write-permission verdict — derived, never exercised.
    sub.push(write_permission_check(&token_info.scopes, repo_json.as_ref(), read_ok));

    let status = rollup(&sub);
    let detail = format!("Identity: {identity}");
    HealthCheck { id: id.into(), label: label.into(), status, detail, sub, command: None }
}

/// Derive a write-permission verdict without mutating the repo:
/// - Classic PAT (non-empty scopes): needs `repo` (private) or at least
///   `public_repo` (public) — mAIestro Code creates issues/PRs and merges.
/// - Fine-grained PAT (empty scopes): read the repo's `permissions.push` flag.
fn write_permission_check(
    scopes: &[String],
    repo_json: Option<&serde_json::Value>,
    read_ok: bool,
) -> HealthCheck {
    let id = "repo_write";
    let label = "Write access (issues, PRs)";
    let has = |s: &str| scopes.iter().any(|x| x == s);

    if !scopes.is_empty() {
        // Classic PAT: the scope list is authoritative.
        if has("repo") {
            return HealthCheck::new(id, label, HealthStatus::Pass, "Classic token has `repo` scope");
        }
        let is_public = repo_json
            .and_then(|v| v["private"].as_bool())
            .map(|private| !private)
            .unwrap_or(false);
        if is_public && has("public_repo") {
            return HealthCheck::new(
                id,
                label,
                HealthStatus::Pass,
                "Classic token has `public_repo` scope (public repo)",
            );
        }
        return HealthCheck::new(
            id,
            label,
            HealthStatus::Fail,
            format!("Token missing `repo` scope (has: {})", scopes.join(", ")),
        );
    }

    // Fine-grained token (or GitHub App): infer from the repo `permissions`.
    let Some(v) = repo_json else {
        let detail = if read_ok {
            "Could not read repo permissions"
        } else {
            "Repo not readable, so write access can't be determined"
        };
        return HealthCheck::new(id, label, HealthStatus::Warn, detail);
    };
    match v["permissions"]["push"].as_bool() {
        Some(true) => HealthCheck::new(id, label, HealthStatus::Pass, "Fine-grained token can push to this repo"),
        Some(false) => HealthCheck::new(
            id,
            label,
            HealthStatus::Fail,
            "Token can read but cannot push to this repo (needs Contents/Issues/Pull requests: write)",
        ),
        None => HealthCheck::new(id, label, HealthStatus::Warn, "Repo permissions not reported by GitHub"),
    }
}

// ── Tool versions (issue #146) ──────────────────────────────────────────────
//
// The earlier checks confirm each directly-invoked tool *resolves*
// (`check_cli`, `check_claude`/`check_codex`, `check_editor`); this trailing
// group asks how old it is. mAIestro Code leans on features only newer releases
// have — Claude Code's `--remote-control`/`--name` launch flags, the
// `PostToolUseFailure` hook, `/color`; Codex's hooks and `exec` flags; `git
// worktree`; VS Code's `--disable-workspace-trust` — so a
// stale binary can fail mid-spawn or degrade silently. A version below the
// floor is a **`Warn`**, never a `Fail`: an old tool might still work, and this
// report is informational like the rest of it. Floors are hardcoded constants,
// not a setting — bumping one is a code change that ships with whatever
// feature needs it.

/// One entry in the minimum-version table: the `tools::find_tool` name, the
/// health-check label, the minimum version (dotted, parsed via
/// [`parse_version`]), and why the floor exists (shown in the warn detail).
struct ToolMinimum {
    tool: &'static str,
    label: &'static str,
    min: &'static str,
    reason: &'static str,
}

/// Hardcoded floors, in the order their sub-checks appear in the report.
const MIN_TOOL_VERSIONS: &[ToolMinimum] = &[
    ToolMinimum {
        tool: "claude",
        label: "Claude Code",
        min: "2.0.0",
        reason: "needed for --remote-control, --name, the PostToolUseFailure hook, and /color",
    },
    ToolMinimum {
        tool: "codex",
        label: "Codex CLI",
        min: "0.133.0",
        reason: "needed for stable hooks (PermissionRequest, SessionEnd) and the `codex exec` flags drafting uses",
    },
    ToolMinimum {
        tool: "git",
        label: "Git",
        min: "2.22.0",
        reason: "needed for reliable `git worktree` and `for-each-ref` support",
    },
    ToolMinimum {
        tool: "code",
        label: "VS Code",
        min: "1.80.0",
        reason: "needed for --disable-workspace-trust",
    },
];

/// Extract the first run of `digits(.digits)*` from the first non-empty line of
/// `output` — every tool prints its version as the first thing on the first
/// line (`2.1.270 (Claude Code)`, `git version 2.54.0`, or `1.137.0` followed by
/// a commit hash and arch on later lines). `None` when no such run is found, so
/// callers fall back to `Info` rather than guessing.
fn parse_version(output: &str) -> Option<Vec<u64>> {
    let line = output.lines().find(|l| !l.trim().is_empty())?;
    let chars: Vec<char> = line.chars().collect();
    let start = chars.iter().position(|c| c.is_ascii_digit())?;
    let mut end = start;
    while end < chars.len() && (chars[end].is_ascii_digit() || chars[end] == '.') {
        end += 1;
    }
    let mut token: String = chars[start..end].iter().collect();
    while token.ends_with('.') {
        token.pop();
    }
    if token.is_empty() {
        return None;
    }
    token.split('.').map(|p| p.parse().ok()).collect()
}

/// Component-wise `found >= min`, treating a missing trailing component as `0`
/// on either side (so `2.0` is at least `2.0.0`, and `2` is not at least
/// `2.0.1`).
fn version_at_least(found: &[u64], min: &[u64]) -> bool {
    for i in 0..min.len().max(found.len()) {
        let f = found.get(i).copied().unwrap_or(0);
        let m = min.get(i).copied().unwrap_or(0);
        if f != m {
            return f > m;
        }
    }
    true
}

/// The result of probing `<tool> --version`, kept separate from the resolve
/// step so [`tool_version_check`] is a pure classifier every branch of which is
/// unit-tested without spawning anything.
enum VersionOutcome {
    /// The command exited with output to parse.
    Output(String),
    /// The probe was killed after the timeout.
    TimedOut,
    /// The command couldn't be run or exited with nothing usable on stdout.
    Error(String),
}

/// A resolved-but-unusable-`brew` path never gets an upgrade command guessed —
/// only these tools have an unambiguous one. `claude update` and `codex update`
/// self-update regardless of install method; `git` only offers `brew upgrade git` when the
/// resolved binary actually lives under a Homebrew prefix (Apple's Xcode-stub
/// git and a system git can't be upgraded that way). VS Code updates itself
/// from its own menu, so `code` never gets a command.
fn version_upgrade_command(tool: &str, path: &std::path::Path) -> Option<String> {
    match tool {
        "claude" => Some("claude update".to_string()),
        "codex" => Some("codex update".to_string()),
        "git" => {
            let p = path.to_string_lossy();
            (p.starts_with("/opt/homebrew/") || p.starts_with("/usr/local/")).then(|| "brew upgrade git".to_string())
        }
        _ => None,
    }
}

/// Turn a `--version` probe into the version sub-check. `resolved` is the
/// binary path; `None` means `find_tool` couldn't resolve it at all — the
/// corresponding earlier check (e.g. "Git available") already reported that at
/// the right severity, so this just points back at it rather than repeating the
/// failure. Pure so every branch is unit-tested directly.
fn tool_version_check(min: &ToolMinimum, resolved: Option<&std::path::Path>, outcome: VersionOutcome) -> HealthCheck {
    let id = format!("{}_version", min.tool);
    let label = format!("{} ≥ {}", min.label, min.min);
    let Some(path) = resolved else {
        return HealthCheck::new(&id, &label, HealthStatus::Skipped, "Not found — see above");
    };
    match outcome {
        VersionOutcome::TimedOut => {
            HealthCheck::new(&id, &label, HealthStatus::Warn, format!("`{} --version` timed out", min.tool))
        }
        VersionOutcome::Error(e) => {
            HealthCheck::new(&id, &label, HealthStatus::Warn, format!("Couldn't run `{} --version`: {}", min.tool, snippet(&e)))
        }
        VersionOutcome::Output(stdout) => {
            let Some(found) = parse_version(&stdout) else {
                return HealthCheck::new(
                    &id,
                    &label,
                    HealthStatus::Info,
                    format!("Couldn't parse version: {}", snippet(&stdout)),
                );
            };
            // The floor is a compile-time constant validated by
            // `every_tool_minimum_parses`, so this always succeeds in practice.
            let min_parts = parse_version(min.min).unwrap_or_default();
            let found_str = found.iter().map(u64::to_string).collect::<Vec<_>>().join(".");
            if version_at_least(&found, &min_parts) {
                HealthCheck::new(&id, &label, HealthStatus::Pass, format!("{found_str} · {}", path.display()))
            } else {
                let check = HealthCheck::new(
                    &id,
                    &label,
                    HealthStatus::Warn,
                    format!("{found_str} is older than {} — {}", min.min, min.reason),
                );
                match version_upgrade_command(min.tool, path) {
                    Some(cmd) => check.with_command(cmd),
                    None => check,
                }
            }
        }
    }
}

/// Resolve and probe one tool's `--version`, then classify via
/// [`tool_version_check`]. A 10s timeout is generous — every one of these
/// prints its version in well under a second — but keeps a hung binary from
/// stalling the health run.
async fn check_one_tool_version(repo: &str, min: &ToolMinimum) -> HealthCheck {
    let Some(path) = crate::tools::find_tool(min.tool) else {
        return tool_version_check(min, None, VersionOutcome::Error(String::new()));
    };
    log_command(repo, &format!("{} --version", path.display()));
    let mut cmd = crate::tools::tokio_command(min.tool);
    cmd.arg("--version");
    cmd.kill_on_drop(true);
    let run = tokio::time::timeout(std::time::Duration::from_secs(10), cmd.output()).await;
    let outcome = match run {
        Err(_) => VersionOutcome::TimedOut,
        Ok(Err(e)) => VersionOutcome::Error(e.to_string()),
        Ok(Ok(o)) => {
            let stdout = String::from_utf8_lossy(&o.stdout).to_string();
            if o.status.success() || !stdout.trim().is_empty() {
                VersionOutcome::Output(stdout)
            } else {
                VersionOutcome::Error(String::from_utf8_lossy(&o.stderr).to_string())
            }
        }
    };
    tool_version_check(min, Some(path.as_path()), outcome)
}

/// Whether a [`MIN_TOOL_VERSIONS`] entry applies to a repo on `agent`: `git` and
/// `code` always do; an agent CLI only when it is the repo's agent.
fn tool_applies(tool: &str, agent: Agent) -> bool {
    match tool {
        "claude" | "codex" => tool == agent.tool(),
        _ => true,
    }
}

/// Run `<tool> --version` for every entry in [`MIN_TOOL_VERSIONS`] that applies
/// to the repo's agent and return the parent "Tool versions" row with one
/// sub-check per tool, rolled up the same way every other multi-sub group is
/// (see [`rollup`]).
async fn check_tool_versions(repo: &str, agent: Agent) -> HealthCheck {
    let id = "tool_versions";
    let label = "Tool versions";
    let mut sub = Vec::with_capacity(MIN_TOOL_VERSIONS.len());
    for min in MIN_TOOL_VERSIONS.iter().filter(|m| tool_applies(m.tool, agent)) {
        sub.push(check_one_tool_version(repo, min).await);
    }
    let status = rollup(&sub);
    HealthCheck { id: id.into(), label: label.into(), status, detail: String::new(), sub, command: None }
}

/// Worst-case roll-up of a group's sub-checks into the parent status:
/// any Fail → Fail; else any Warn → Warn; else any Skipped → Skipped; else Pass.
/// `Info` is deliberately *not* ranked — it reports no problem, so a group whose
/// only non-pass sub is informational still rolls up to Pass.
fn rollup(sub: &[HealthCheck]) -> HealthStatus {
    if sub.iter().any(|c| c.status == HealthStatus::Fail) {
        HealthStatus::Fail
    } else if sub.iter().any(|c| c.status == HealthStatus::Warn) {
        HealthStatus::Warn
    } else if sub.iter().any(|c| c.status == HealthStatus::Skipped) {
        HealthStatus::Skipped
    } else {
        HealthStatus::Pass
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_families_unquotes_and_drops_generics() {
        assert_eq!(
            font_families("'JetBrainsMono Nerd Font', 'Cascadia Code', Menlo, monospace"),
            vec!["JetBrainsMono Nerd Font", "Cascadia Code", "Menlo"]
        );
        assert_eq!(font_families("\"Fira Code\" , sans-serif"), vec!["Fira Code"]);
        // A stack of nothing but generics leaves no installable family.
        assert!(font_families("monospace").is_empty());
        assert!(font_families("").is_empty());
    }

    /// A font file is named for the family plus its style, and spellings differ in
    /// spacing/case — so matching is on the normalized name as a prefix.
    #[test]
    fn font_installed_matches_family_against_file_names() {
        let installed = vec![
            normalize_font_name("JetBrainsMonoNerdFont-Regular"),
            normalize_font_name("Menlo"),
        ];
        assert!(font_installed("JetBrainsMono Nerd Font", &installed));
        assert!(font_installed("Menlo", &installed));
        assert!(!font_installed("Cascadia Code", &installed));
        // An empty family never matches everything.
        assert!(!font_installed("", &installed));
    }

    /// The schema default's preferred family must map to a real cask, or the
    /// health check would offer no way to fix what it reports.
    #[test]
    fn default_font_has_an_install_command() {
        let stack = crate::app_settings::terminal_font_family();
        let preferred = font_families(&stack).first().cloned().expect("a family");
        assert_eq!(
            font_install_command(&preferred).as_deref(),
            Some("brew install --cask font-jetbrains-mono-nerd-font"),
            "no install command for the preferred font {preferred}"
        );
        assert_eq!(font_install_command("Some Unknown Face"), None);
    }

    /// A missing Nerd Font is Info, not a failure — the stack still resolves to a
    /// stock face — and carries the one-liner that installs it.
    #[test]
    fn missing_preferred_font_is_info_with_an_install_command() {
        let installed = vec![normalize_font_name("Menlo")];
        let check = terminal_font_check("'JetBrainsMono Nerd Font', Menlo, monospace", &installed);
        assert_eq!(check.status, HealthStatus::Info);
        assert!(check.detail.contains("falls back to Menlo"), "detail: {}", check.detail);
        assert_eq!(check.command.as_deref(), Some("brew install --cask font-jetbrains-mono-nerd-font"));
    }

    /// Nothing in the configured stack installed is a real misconfiguration, so
    /// that one warns rather than merely informing.
    #[test]
    fn stack_with_nothing_installed_warns() {
        let check = terminal_font_check("'Not A Font', monospace", &[normalize_font_name("Menlo")]);
        assert_eq!(check.status, HealthStatus::Warn);
        // No cask is known for it, so no command is offered rather than a wrong one.
        assert_eq!(check.command, None);
    }

    /// An informational row reports no problem, so it must not drag a group's
    /// roll-up below Pass.
    #[test]
    fn rollup_ignores_info_rows() {
        let info = HealthCheck::new("i", "i", HealthStatus::Info, "");
        let pass = HealthCheck::new("p", "p", HealthStatus::Pass, "");
        assert_eq!(rollup(&[info, pass]), HealthStatus::Pass);
        let info = HealthCheck::new("i", "i", HealthStatus::Info, "");
        let warn = HealthCheck::new("w", "w", HealthStatus::Warn, "");
        assert_eq!(rollup(&[info, warn]), HealthStatus::Warn);
    }

    #[test]
    fn remote_owner_name_parses_all_url_forms() {
        let cases = [
            "https://github.com/owner/name.git",
            "https://github.com/owner/name",
            "git@github.com:owner/name.git",
            "git@github.com:owner/name",
            "ssh://git@github.com/owner/name.git",
            "https://github.com/owner/name/",
        ];
        for url in cases {
            assert_eq!(remote_owner_name(url).as_deref(), Some("owner/name"), "url: {url}");
        }
    }

    #[test]
    fn clone_command_uses_https_remote_and_keeps_tilde_dir() {
        assert_eq!(
            clone_command("owner/name", "~/src/name"),
            "git clone https://github.com/owner/name.git ~/src/name"
        );
    }

    #[test]
    fn remote_owner_name_rejects_too_few_segments() {
        assert_eq!(remote_owner_name("name"), None);
        assert_eq!(remote_owner_name(""), None);
    }

    // Pull the nested model sub-check (there is always exactly one).
    fn model_sub(login: &HealthCheck) -> &HealthCheck {
        &login.sub[0]
    }

    #[test]
    fn classify_logged_out_envelope_fails_login_not_model() {
        // The real logged-out envelope: exits with an error but *no* HTTP status,
        // because claude bailed out before authenticating. Regression guard — this
        // used to be misread as "auth worked, model broken" and left login green.
        let env = serde_json::json!({
            "is_error": true,
            "api_error_status": null,
            "result": "Not logged in · Please run /login",
        });
        let login = classify_claude_envelope(&env, "haiku", "claude login");
        assert_eq!(login.status, HealthStatus::Fail);
        assert_eq!(login.command.as_deref(), Some("claude login"));
        assert_eq!(model_sub(&login).status, HealthStatus::Skipped);
    }

    #[test]
    fn classify_401_fails_login_and_skips_model() {
        let env = serde_json::json!({
            "is_error": true,
            "api_error_status": 401,
            "result": "Unauthorized",
        });
        let login = classify_claude_envelope(&env, "haiku", "claude login");
        assert_eq!(login.status, HealthStatus::Fail);
        assert_eq!(model_sub(&login).status, HealthStatus::Skipped);
    }

    #[test]
    fn classify_404_passes_login_but_fails_model() {
        // Authenticated fine, just an unknown model: login stays green, model red.
        let env = serde_json::json!({
            "is_error": true,
            "api_error_status": 404,
            "result": "model: nonesuch not found",
        });
        let login = classify_claude_envelope(&env, "nonesuch", "claude login");
        assert_eq!(login.status, HealthStatus::Pass);
        assert_eq!(model_sub(&login).status, HealthStatus::Fail);
    }

    #[test]
    fn classify_clean_reply_passes_both() {
        let env = serde_json::json!({
            "is_error": false,
            "api_error_status": null,
            "result": "ok",
        });
        let login = classify_claude_envelope(&env, "haiku", "claude login");
        assert_eq!(login.status, HealthStatus::Pass);
        assert_eq!(model_sub(&login).status, HealthStatus::Pass);
    }

    #[test]
    fn remote_owner_name_is_used_case_insensitively() {
        // The comparison at the call site is case-insensitive; the parser itself
        // preserves case, so callers must use eq_ignore_ascii_case.
        let parsed = remote_owner_name("https://github.com/Owner/Name.git").unwrap();
        assert!(parsed.eq_ignore_ascii_case("owner/name"));
    }

    // ── Tool versions ────────────────────────────────────────────────────

    #[test]
    fn parse_version_reads_the_three_real_output_shapes() {
        assert_eq!(parse_version("2.1.270 (Claude Code)\n"), Some(vec![2, 1, 270]));
        assert_eq!(parse_version("git version 2.54.0\n"), Some(vec![2, 54, 0]));
        // Apple's build appends a suffix after the version; the parser must stop
        // at the first non-digit/dot character rather than swallowing it.
        assert_eq!(parse_version("git version 2.39.3 (Apple Git-145)\n"), Some(vec![2, 39, 3]));
        // `code --version` is three lines: version, commit hash, arch. Only the
        // first line matters.
        assert_eq!(
            parse_version("1.137.0\n645f29cc3176500b4b5762ba887cf2a7f0ffdf2c\narm64\n"),
            Some(vec![1, 137, 0])
        );
    }

    #[test]
    fn parse_version_rejects_unparseable_output() {
        assert_eq!(parse_version("command not found"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("\n\n"), None);
    }

    #[test]
    fn version_at_least_compares_component_wise_with_missing_as_zero() {
        assert!(version_at_least(&[2, 1, 270], &[2, 0, 0]));
        assert!(!version_at_least(&[1, 9], &[2, 0]));
        // A missing trailing component is 0 on either side.
        assert!(version_at_least(&[2, 0], &[2, 0, 0]));
        assert!(!version_at_least(&[2], &[2, 0, 1]));
        assert!(version_at_least(&[2, 0, 0], &[2, 0, 0]));
    }

    /// A typo'd floor would silently disable the check for that tool, so every
    /// entry in the table must actually parse.
    #[test]
    fn every_tool_minimum_parses() {
        for m in MIN_TOOL_VERSIONS {
            assert!(parse_version(m.min).is_some(), "unparsable floor for {}: {}", m.tool, m.min);
        }
    }

    /// Only the repo's agent is version-checked; git and code always are.
    #[test]
    fn tool_versions_are_filtered_by_agent() {
        let for_agent = |a: Agent| -> Vec<&str> {
            MIN_TOOL_VERSIONS.iter().map(|m| m.tool).filter(|t| tool_applies(t, a)).collect()
        };
        assert_eq!(for_agent(Agent::Claude), vec!["claude", "git", "code"]);
        assert_eq!(for_agent(Agent::Codex), vec!["codex", "git", "code"]);
    }

    #[test]
    fn codex_updates_itself() {
        assert_eq!(
            version_upgrade_command("codex", std::path::Path::new("/opt/homebrew/bin/codex")),
            Some("codex update".to_string())
        );
    }

    fn claude_min() -> ToolMinimum {
        ToolMinimum { tool: "claude", label: "Claude Code", min: "2.0.0", reason: "needed for testing" }
    }

    #[test]
    fn tool_version_check_passes_when_at_or_above_floor() {
        let min = claude_min();
        let path = std::path::PathBuf::from("/opt/homebrew/bin/claude");
        let check = tool_version_check(&min, Some(&path), VersionOutcome::Output("2.1.270 (Claude Code)\n".into()));
        assert_eq!(check.status, HealthStatus::Pass);
        assert!(check.detail.contains("2.1.270"), "detail: {}", check.detail);
        assert!(check.detail.contains("/opt/homebrew/bin/claude"), "detail: {}", check.detail);
        assert_eq!(check.command, None);
    }

    #[test]
    fn tool_version_check_warns_below_floor_with_reason_and_command() {
        let min = claude_min();
        let path = std::path::PathBuf::from("/usr/local/bin/claude");
        let check = tool_version_check(&min, Some(&path), VersionOutcome::Output("1.9.0\n".into()));
        assert_eq!(check.status, HealthStatus::Warn);
        assert!(check.detail.contains("older than 2.0.0"), "detail: {}", check.detail);
        assert!(check.detail.contains("needed for testing"), "detail: {}", check.detail);
        assert_eq!(check.command.as_deref(), Some("claude update"));
    }

    #[test]
    fn tool_version_check_skips_when_unresolved() {
        let min = claude_min();
        let check = tool_version_check(&min, None, VersionOutcome::Error(String::new()));
        assert_eq!(check.status, HealthStatus::Skipped);
    }

    #[test]
    fn tool_version_check_is_info_when_unparseable() {
        let min = claude_min();
        let path = std::path::PathBuf::from("/usr/local/bin/claude");
        let check = tool_version_check(&min, Some(&path), VersionOutcome::Output("weird wrapper output".into()));
        assert_eq!(check.status, HealthStatus::Info);
        assert!(check.detail.contains("weird wrapper output"), "detail: {}", check.detail);
    }

    #[test]
    fn tool_version_check_warns_on_timeout_and_error() {
        let min = claude_min();
        let path = std::path::PathBuf::from("/usr/local/bin/claude");
        let timed_out = tool_version_check(&min, Some(&path), VersionOutcome::TimedOut);
        assert_eq!(timed_out.status, HealthStatus::Warn);
        let errored = tool_version_check(&min, Some(&path), VersionOutcome::Error("boom".into()));
        assert_eq!(errored.status, HealthStatus::Warn);
        assert!(errored.detail.contains("boom"), "detail: {}", errored.detail);
    }

    #[test]
    fn version_upgrade_command_only_offers_brew_for_a_homebrew_git() {
        assert_eq!(
            version_upgrade_command("git", std::path::Path::new("/opt/homebrew/bin/git")),
            Some("brew upgrade git".to_string())
        );
        assert_eq!(
            version_upgrade_command("git", std::path::Path::new("/usr/local/bin/git")),
            Some("brew upgrade git".to_string())
        );
        // Apple's Xcode-stub git and any other system git can't be upgraded that
        // way, so no command is offered rather than a wrong one.
        assert_eq!(version_upgrade_command("git", std::path::Path::new("/usr/bin/git")), None);
    }

    #[test]
    fn version_upgrade_command_claude_updates_regardless_of_path_code_never_does() {
        assert_eq!(
            version_upgrade_command("claude", std::path::Path::new("/usr/bin/claude")),
            Some("claude update".to_string())
        );
        assert_eq!(version_upgrade_command("code", std::path::Path::new("/opt/homebrew/bin/code")), None);
    }
}

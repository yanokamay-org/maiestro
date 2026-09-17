//! Spawn core + teardown: create (or reopen) a git worktree + VS Code workspace
//! for a GitHub issue, and later tear it down.
//!
//! GitHub is reached via the REST API under the repo's identity (no `gh`), env
//! files come from the repo's settings, and the editor is launched via `open -a`
//! (no constructed env). The pieces this orchestrates live in focused modules
//! (issue #99): `theming`, `hooks`, `editor`, `drafting`, `pr`, plus the shared
//! `gitops` / `naming` / `repo_context` helpers.

use std::path::{Path, PathBuf};

use crate::drafting::{resolve_draft, ClaudeActivity, DraftStep};
use crate::editor::{
    close_editor_window, open_vscode, probe_editor_window, window_marker, worktree_in_use,
    write_vscode_files, WinProbe,
};
use crate::gitops::{git, git_net, local_branch_exists};
use crate::hooks::{reconcile_session_hooks, write_claude_hooks};
use crate::naming::{default_short_title, slugify};
use crate::paths::expand_tilde;
use crate::plugins::GitHub;
use crate::repo_context::{repo_context, validated_cloned_repo};
use crate::theming::pick_theme;
use crate::tools::snippet;

// ── Command ─────────────────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct SpawnResult {
    /// The workspace/session id (= `Session.id`), so the frontend can match the
    /// row this spawn created and track its `creating` status.
    pub session_id: String,
    pub work_dir: String,
    pub branch: String,
    pub issue_url: String,
    /// True when an existing workspace was reused rather than created.
    pub reused: bool,
    /// Non-fatal warnings raised during the *synchronous* phase. On a fresh
    /// spawn the heavy work now runs in the background, so its warnings are
    /// logged there rather than returned here.
    pub warnings: Vec<String>,
}

/// Result of `create_issue_direct`: the issue was opened from the reviewed
/// title/body. Tagged (`status: "created"`) for a stable wire shape the frontend
/// switches on.
#[derive(serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CreateIssueOutcome {
    Created {
        number: u64,
        issue_url: String,
        warnings: Vec<String>,
    },
}

/// The decisions a spawn needs once the issue is known: the (possibly edited)
/// short label that drives the slug, and the chosen theming. Slugs and the
/// session title are derived from these so the preview and the spawn agree.
struct SpawnDecision<'a> {
    repo: &'a str,
    issue_number: u64,
    issue_url: &'a str,
    default_branch: &'a str,
    short_label: &'a str,
    color: &'a str,
    emoji: &'a str,
    force_new: bool,
}

/// The naming/placement decision for a spawn — reuse an existing worktree or
/// create a fresh (possibly suffixed) one. Computed with no side effects by
/// [`resolve_workspace`] from the issue facts, settings, and two existence
/// predicates, so the bug-prone reuse-vs-create branch and `-2`/`-3` suffixing
/// are unit-testable without git, GitHub, or the filesystem. `do_spawn` is then a
/// thin walk over this plan.
#[derive(Debug, PartialEq, Eq)]
enum WorkspacePlan {
    /// An existing worktree at `work_dir` is reopened rather than rebuilt.
    Reuse { workspace: String, branch: String, work_dir: PathBuf },
    /// A fresh worktree/session is created. `session_label` is the emoji-less
    /// label (`#n — <label>`, with a ` (k)` suffix when the base name collided).
    Create { workspace: String, branch: String, work_dir: PathBuf, session_label: String },
}

/// Resolve a spawn's workspace naming/placement — pure, no side effects.
///
/// `dir_exists` reports whether a candidate worktree directory is already on
/// disk; `branch_taken` whether a candidate local branch already exists. Reuse
/// wins only when not forcing new *and* the base worktree dir exists. Otherwise
/// the first free `(dir, branch)` pair is found by suffixing `-2`, `-3`, … . The
/// worktree path is `<worktree_prefix><workspace>/<repo_name>` (a string concat —
/// the trailing `work-` is part of the dir name), tilde-expanded.
fn resolve_workspace(
    issue_number: u64,
    short_label: &str,
    repo_name: &str,
    worktree_prefix: &str,
    force_new: bool,
    dir_exists: impl Fn(&Path) -> bool,
    branch_taken: impl Fn(&str) -> bool,
) -> WorkspacePlan {
    let worktree_dir =
        |workspace: &str| expand_tilde(&format!("{worktree_prefix}{workspace}")).join(repo_name);
    let label_prefix = format!("#{issue_number} — ");
    let base_workspace = format!("{issue_number}-{}", slugify(short_label, 25));
    let base_branch = format!("feature/{base_workspace}");
    let base_dir = worktree_dir(&base_workspace);

    // Reuse an existing workspace by default; force_new skips straight to create.
    if !force_new && dir_exists(&base_dir) {
        return WorkspacePlan::Reuse {
            workspace: base_workspace,
            branch: base_branch,
            work_dir: base_dir,
        };
    }

    // Resolve to the first free (path, branch) pair, suffixing -2, -3, … .
    let mut workspace = base_workspace.clone();
    let mut branch = base_branch.clone();
    let mut work_dir = base_dir;
    let mut session_label = format!("{label_prefix}{short_label}");
    let mut n = 2;
    while dir_exists(&work_dir) || branch_taken(&branch) {
        workspace = format!("{base_workspace}-{n}");
        branch = format!("{base_branch}-{n}");
        work_dir = worktree_dir(&workspace);
        session_label = format!("{label_prefix}{short_label} ({n})");
        n += 1;
    }
    WorkspacePlan::Create { workspace, branch, work_dir, session_label }
}

/// The name of the worktree's parent directory (e.g. `work-127-add-session-color`),
/// which the generated `window.title` marker — and teardown's window lookup — are
/// keyed on. Empty when the path has no usable parent.
fn work_parent_of(work_dir: &Path) -> String {
    work_dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Regenerate a reused worktree's `.vscode` files from its recorded session, so
/// reopening picks up changes to what we generate (the session color, the window
/// marker, the startup task) without needing a fresh spawn.
///
/// Deliberately sourced from the session record rather than the caller's freshly
/// picked theme: reopening a worktree must not re-theme it. Best-effort — a
/// worktree with no session record is left untouched, and a write failure only
/// warns, because the reopen itself must still succeed.
fn refresh_vscode_files(work_dir: &Path, workspace: &str) {
    let Some(session) = crate::sessions::get(workspace) else {
        return;
    };
    if let Err(e) = write_vscode_files(
        work_dir,
        &work_parent_of(work_dir),
        &session.color,
        &session.session_title,
    ) {
        tracing::warn!(error = %e, "could not refresh .vscode files on reuse");
    }
}

/// Everything the background phase of a fresh spawn needs, owned so it can move
/// into the `tokio::spawn`ed task that builds the worktree after `do_spawn` has
/// already returned. See `finish_spawn`.
struct SpawnBg {
    gh: GitHub,
    cloned_repo: PathBuf,
    work_dir: PathBuf,
    work_parent: String,
    branch: String,
    workspace: String,
    session_title: String,
    color: String,
    default_branch: String,
    repo: String,
    issue_number: u64,
    env_files: Vec<String>,
    post_spawn_commands: Vec<String>,
    /// Whether to record the workspace in a comment on the issue. Resolved from
    /// the repo's `comment_on_spawn` setting; does not affect assignment.
    comment_on_spawn: bool,
}

/// Core worktree + session creation, shared by every spawn path. Resolves the
/// repo's settings/identity/cloned repo itself; the caller supplies the issue facts
/// and the (reviewed) label/theming. The slug is `<n>-<slug(short_label)>`.
///
/// Two-phase for a fresh spawn: this fast synchronous phase resolves the final
/// workspace id, records the `Session` row, and marks it `creating` (so the
/// popover shows the row with a "Creating…" pill immediately), then hands the
/// slow work — worktree add, env copy, GitHub assign/comment, post-spawn
/// commands, editor launch — to a background task and returns at once. Reopening
/// an existing worktree stays fully synchronous (it's near-instant).
#[tracing::instrument(skip_all, fields(session = tracing::field::Empty))]
async fn do_spawn(d: SpawnDecision<'_>) -> Result<SpawnResult, String> {
    let SpawnDecision { repo, issue_number, issue_url, default_branch, short_label, color, emoji, force_new } = d;

    let (settings, gh) = repo_context(repo).await?;
    let cloned_repo = validated_cloned_repo(&settings)?;
    let repo_name = repo.split('/').next_back().unwrap_or(repo).to_string();

    let short_label = {
        let t = short_label.trim();
        if t.is_empty() { default_short_title("") } else { t.to_string() }
    };

    // Worktree location prefix: the full path is `<prefix><workspace>/<repo>`
    // (string concat — the trailing `work-` is part of the dir name). Unset
    // falls back to the schema default.
    let worktree_prefix = effective_worktree_prefix(settings.worktree_prefix.as_deref());

    // Pure resolution of the reuse-vs-create decision and the workspace id/branch/
    // path (see `resolve_workspace`). Branch existence is snapshotted once so the
    // resolver's suffix loop stays synchronous; on-disk collisions use `is_dir`.
    let branches = crate::gitops::local_branches(&cloned_repo).await;
    let plan = resolve_workspace(
        issue_number,
        &short_label,
        &repo_name,
        &worktree_prefix,
        force_new,
        |p| p.is_dir(),
        |b| branches.contains(b),
    );

    let (workspace, branch, work_dir, session_label) = match plan {
        WorkspacePlan::Reuse { workspace, branch, work_dir } => {
            // Tag this span (and every log line it emits) with the session id.
            tracing::Span::current().record("session", workspace.as_str());
            // Reopening doesn't rewrite hooks, so heal a stale binary path here
            // too (without waiting for the next startup reconcile).
            reconcile_session_hooks(&work_dir, &workspace);
            // Same for the generated `.vscode` files, so a worktree spawned
            // before a change to them (e.g. the session color) picks it up.
            refresh_vscode_files(&work_dir, &workspace);
            open_vscode(&work_dir)?;
            tracing::info!(repo = %repo, issue = issue_number, branch = %branch, reused = true, "spawned workspace");
            return Ok(SpawnResult {
                session_id: workspace,
                work_dir: work_dir.display().to_string(),
                branch,
                issue_url: issue_url.to_string(),
                reused: true,
                warnings: Vec::new(),
            });
        }
        WorkspacePlan::Create { workspace, branch, work_dir, session_label } => {
            (workspace, branch, work_dir, session_label)
        }
    };
    // Tag the span with the final (possibly suffixed) workspace id.
    tracing::Span::current().record("session", workspace.as_str());

    let session_title = format!("{emoji} {session_label}");
    let work_parent = work_parent_of(&work_dir);

    // Record the session up front so the dashboard shows the row immediately
    // (and so its color counts as taken for the next spawn) — the worktree it
    // points at is built by the background task below. If we can't even record
    // it, fail synchronously: the row would never appear.
    let session = crate::sessions::Session {
        id: workspace.clone(),
        repo: repo.to_string(),
        issue_number,
        issue_url: issue_url.to_string(),
        branch: branch.clone(),
        default_branch: default_branch.to_string(),
        work_dir: work_dir.display().to_string(),
        cloned_repo_dir: cloned_repo.display().to_string(),
        session_title: session_title.clone(),
        color: color.to_string(),
        emoji: emoji.to_string(),
        hidden: None,
    };
    crate::sessions::save(&session).map_err(|e| format!("could not record session: {e}"))?;

    // Mark it `creating` so the popover shows a "Creating…" pill while the
    // background task builds the worktree.
    crate::status::write_creating(&workspace);

    // Hand the slow work off to a background task and return at once.
    let bg = SpawnBg {
        gh,
        cloned_repo,
        work_dir: work_dir.clone(),
        work_parent,
        branch: branch.clone(),
        workspace: workspace.clone(),
        session_title,
        color: color.to_string(),
        default_branch: default_branch.to_string(),
        repo: repo.to_string(),
        issue_number,
        env_files: settings.env_files.clone(),
        post_spawn_commands: settings.post_spawn_commands.clone(),
        comment_on_spawn: crate::repo_settings::bool_or_default(
            settings.comment_on_spawn,
            "/properties/comment_on_spawn/default",
        ),
    };
    tokio::spawn(finish_spawn(bg));

    tracing::info!(repo = %repo, issue = issue_number, branch = %branch, reused = false, "spawn started (building worktree in background)");
    Ok(SpawnResult {
        session_id: workspace,
        work_dir: work_dir.display().to_string(),
        branch,
        issue_url: issue_url.to_string(),
        reused: false,
        warnings: Vec::new(),
    })
}

/// Background phase of a fresh spawn: build the worktree and launch the editor
/// after `do_spawn` has already returned. On success the `creating` marker is
/// cleared (handing the status over to Claude's hooks); on a fatal failure it's
/// replaced with a surfaced error the popover shows on the row. Carries the
/// `session=` span so its log lines join the rest of the spawn's story.
async fn finish_spawn(bg: SpawnBg) {
    use tracing::Instrument;
    let span = tracing::info_span!("finish_spawn", session = %bg.workspace);
    async move {
        match do_finish_spawn(&bg).await {
            Ok(warnings) => {
                for w in &warnings {
                    tracing::warn!(warning = %w, "spawn warning");
                }
                tracing::info!(branch = %bg.branch, "spawn finished");
            }
            Err(e) => {
                tracing::error!(error = %e, "spawn failed");
                crate::status::write_spawn_error(&bg.workspace, &e);
            }
        }
    }
    .instrument(span)
    .await;
}

/// The actual worktree build, factored out so `finish_spawn` can map its result
/// to the status record. Returns non-fatal warnings on success; an `Err` is a
/// fatal failure (e.g. `git worktree add`) that leaves no usable worktree.
async fn do_finish_spawn(bg: &SpawnBg) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();

    // Create the worktree from the repo's default branch.
    std::fs::create_dir_all(bg.work_dir.parent().unwrap()).map_err(|e| e.to_string())?;
    if let Err(e) = git_net(&bg.cloned_repo, &["fetch", "origin", "--quiet"]).await {
        tracing::warn!(error = %e, "git fetch before spawn failed (continuing)");
    }
    git(&bg.cloned_repo, &["worktree", "add", &bg.work_dir.to_string_lossy(), "-b", &bg.branch, &format!("origin/{}", bg.default_branch)]).await?;
    if let Err(e) = git(&bg.work_dir, &["branch", "--unset-upstream"]).await {
        tracing::warn!(error = %e, "git branch --unset-upstream failed (continuing)");
    }

    // Copy configured env files (relative to the cloned repo) into the worktree.
    for rel in &bg.env_files {
        // Reject absolute or `..`-escaping entries so the copy can't read outside
        // the cloned repo or write outside the worktree.
        if !crate::paths::is_contained_relpath(rel) {
            warnings.push(format!("env file path not contained in the cloned repo, skipped: {rel}"));
            continue;
        }
        let src = bg.cloned_repo.join(rel);
        if !src.is_file() {
            warnings.push(format!("env file not found, skipped: {rel}"));
            continue;
        }
        let dst = bg.work_dir.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        if let Err(e) = std::fs::copy(&src, &dst) {
            warnings.push(format!("could not copy env file {rel}: {e}"));
        }
    }

    // Assign the issue to the token's user and — unless the repo turned it off —
    // record the workspace in a comment. Non-fatal: the worktree already exists,
    // so failures only warn.
    match bg.gh.authenticated_login().await {
        Ok(login) => {
            if let Err(e) = bg.gh.add_assignees(&bg.repo, bg.issue_number, &[login]).await {
                warnings.push(format!("could not assign issue #{}: {e}", bg.issue_number));
            }
            if bg.comment_on_spawn {
                let body = format!(
                    "🤖 Spawned a local workspace for this issue.\n\n\
                     - **GitHub Branch:** `{}`\n\
                     - **Local Directory:** `{}`\n\
                     - **Claude Session:** `{}`\n",
                    bg.branch,
                    bg.work_dir.display(),
                    bg.session_title,
                );
                if let Err(e) = bg.gh.create_comment(&bg.repo, bg.issue_number, &body).await {
                    warnings.push(format!("could not comment on issue #{}: {e}", bg.issue_number));
                }
            } else {
                tracing::debug!(issue = bg.issue_number, "comment_on_spawn is off; not commenting on the issue");
            }
        }
        Err(e) => warnings.push(format!("could not resolve token user for assignment: {e}")),
    }

    write_vscode_files(&bg.work_dir, &bg.work_parent, &bg.color, &bg.session_title)?;
    write_claude_hooks(&bg.work_dir, &bg.workspace).await?;

    // Run the repo's post-spawn commands (e.g. `pnpm install`) in the new
    // worktree before opening the editor, so the session starts ready.
    warnings.extend(run_post_spawn_commands(&bg.work_dir, &bg.post_spawn_commands).await);

    // Worktree is ready: clear the `creating` marker before opening the editor,
    // so Claude's SessionStart hook (fired only once VS Code launches it) owns
    // the status from here without us racing to clobber it.
    crate::status::clear_creating(&bg.workspace);

    open_vscode(&bg.work_dir)?;
    Ok(warnings)
}

/// Fetch an issue's facts (title, url, body) and — via the caller — the repo's
/// default branch: the shared first step of preparing or running a spawn for an
/// existing issue. Also used by the drafting path's short-label suggestion.
pub(crate) async fn issue_facts(gh: &GitHub, repo: &str, issue_number: u64) -> Result<(String, String, String), String> {
    let issue = gh.issue(repo, issue_number).await?;
    let issue_title = issue["title"].as_str().unwrap_or("").to_string();
    if issue_title.is_empty() {
        return Err(format!("issue #{issue_number} not found"));
    }
    let issue_url = issue["html_url"].as_str().unwrap_or("").to_string();
    let issue_body = issue["body"].as_str().unwrap_or("").to_string();
    Ok((issue_title, issue_url, issue_body))
}

/// The effective worktree-path prefix: the configured value, or the schema
/// default (`/properties/worktree_prefix/default`) when unset or empty. The
/// default lives in the JSON schema only — no hardcoded fallback here. Takes the
/// field rather than the whole settings so callers that have already moved other
/// fields out can still use it.
fn effective_worktree_prefix(configured: Option<&str>) -> String {
    configured
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| crate::repo_settings::schema_default("/properties/worktree_prefix/default"))
}

/// Run a repo's post-spawn commands in the freshly-created worktree, in order.
/// Each runs via the user's login shell (`$SHELL -lc`) so PATH and tool managers
/// (nvm, pnpm, asdf, …) are available — mAIestro Code's own environment is minimal and
/// not sourced from a profile. Stops at the first command that fails or times
/// out; returns a warning per problem (the worktree is left in place either way,
/// never torn down). A blank command is skipped.
async fn run_post_spawn_commands(work_dir: &Path, commands: &[String]) -> Vec<String> {
    let mut warnings = Vec::new();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    for cmd in commands {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            continue;
        }
        tracing::info!(command = %cmd, "running post-spawn command");
        let run = tokio::process::Command::new(&shell)
            .args(["-l", "-c", cmd])
            .current_dir(work_dir)
            // Without this, a command that hits the timeout below is orphaned
            // and keeps mutating the worktree under the live session.
            .kill_on_drop(true)
            .output();
        // 10 minutes is generous for installs but still bounds a hung command so
        // it can't freeze the spawn forever.
        let output = tokio::time::timeout(std::time::Duration::from_secs(600), run).await;
        match output {
            Ok(Ok(out)) if out.status.success() => {
                tracing::info!(command = %cmd, "post-spawn command succeeded");
            }
            Ok(Ok(out)) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let code = out.status.code().unwrap_or(-1);
                tracing::error!(command = %cmd, code, stderr = %snippet(&stderr), "post-spawn command failed");
                warnings.push(format!("post-spawn command failed (exit {code}): {cmd}"));
                break;
            }
            Ok(Err(e)) => {
                tracing::error!(command = %cmd, error = %e, "could not run post-spawn command");
                warnings.push(format!("could not run post-spawn command '{cmd}': {e}"));
                break;
            }
            Err(_) => {
                tracing::error!(command = %cmd, "post-spawn command timed out");
                warnings.push(format!("post-spawn command timed out after 10m: {cmd}"));
                break;
            }
        }
    }
    warnings
}

// ── Create issue ────────────────────────────────────────────────────────────────

/// Open an issue from an explicit, already-reviewed title and body (no drafting).
/// Used by the create-issue preview's confirm button.
#[tauri::command]
pub async fn create_issue_direct(repo: String, title: String, body: String) -> Result<CreateIssueOutcome, String> {
    crate::log_invoke!("create_issue_direct", repo = %repo);
    let title = title.trim();
    if title.is_empty() {
        return Err("Issue title can't be empty.".into());
    }
    let (_settings, gh) = repo_context(&repo).await?;
    let number = gh.create_issue(&repo, title, &body).await?;
    Ok(CreateIssueOutcome::Created {
        number,
        issue_url: format!("https://github.com/{repo}/issues/{number}"),
        warnings: Vec::new(),
    })
}


// ── Preview-then-spawn ────────────────────────────────────────────────────────

/// Everything the spawn preview shows for an issue: the editable issue fields
/// and short label, plus the chosen theming and the repo's local dir name (so
/// the UI can render the worktree path). `issue_number` is None on the
/// create-and-spawn path, where the issue isn't opened until the user confirms.
#[derive(serde::Serialize)]
pub struct SpawnPlan {
    pub repo: String,
    pub issue_number: Option<u64>,
    pub issue_title: String,
    pub issue_body: String,
    pub short_title: String,
    pub color: String,
    pub emoji: String,
    pub repo_name: String,
    /// Effective (un-expanded) worktree prefix, so the preview can show where
    /// the worktree will actually land instead of hardcoding the default.
    pub worktree_prefix: String,
}

/// Prepare a preview for spawning an existing issue: fetch its title/body and
/// pick theming, without touching the worktree or GitHub.
#[tauri::command]
pub async fn prepare_spawn(repo: String, issue_number: u64) -> Result<SpawnPlan, String> {
    crate::log_invoke!("prepare_spawn", repo = %repo, issue = issue_number);
    let (settings, gh) = repo_context(&repo).await?;
    let worktree_prefix = effective_worktree_prefix(settings.worktree_prefix.as_deref());
    let (issue_title, _issue_url, issue_body) = issue_facts(&gh, &repo, issue_number).await?;
    let short_title = default_short_title(&issue_title);
    let seed = format!("{issue_number}-{}", slugify(&short_title, 25));
    let (color, emoji) = pick_theme(&seed);
    let repo_name = repo.split('/').next_back().unwrap_or(&repo).to_string();
    Ok(SpawnPlan {
        repo,
        issue_number: Some(issue_number),
        issue_title,
        issue_body,
        short_title,
        color: color.to_string(),
        emoji: emoji.to_string(),
        repo_name,
        worktree_prefix,
    })
}

/// Result of drafting a spawn preview from a free-text idea: a ready preview, or
/// a needs-confirmation prompt (Claude couldn't draft a clear issue).
#[derive(serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DraftPreviewOutcome {
    Drafted(SpawnPlan),
    NeedsConfirmation { message: String },
}

/// Draft an issue from the user's idea (one Claude call, which also yields the
/// short label) and return a preview — WITHOUT creating the issue. The issue is
/// only opened when the user confirms via `confirm_spawn`.
#[tauri::command]
pub async fn draft_spawn_preview(
    app: tauri::AppHandle,
    repo: String,
    idea: String,
    use_raw_fallback: bool,
    request_id: String,
) -> Result<DraftPreviewOutcome, String> {
    crate::log_invoke!("draft_spawn_preview", repo = %repo, use_raw_fallback);
    let activity = ClaudeActivity::new(app, request_id);
    let (_gh, step) = resolve_draft(&repo, &idea, use_raw_fallback, &activity).await?;
    match step {
        DraftStep::Ready { title, body, short_title, .. } => {
            let seed = format!("new-{}", slugify(&short_title, 25));
            let (color, emoji) = pick_theme(&seed);
            let repo_name = repo.split('/').next_back().unwrap_or(&repo).to_string();
            let settings = crate::repo_settings::repo_settings_get(repo.clone())?;
            let worktree_prefix = effective_worktree_prefix(settings.worktree_prefix.as_deref());
            Ok(DraftPreviewOutcome::Drafted(SpawnPlan {
                repo,
                issue_number: None,
                issue_title: title,
                issue_body: body,
                short_title,
                color: color.to_string(),
                emoji: emoji.to_string(),
                repo_name,
                worktree_prefix,
            }))
        }
        DraftStep::NeedsConfirmation { message } => Ok(DraftPreviewOutcome::NeedsConfirmation { message }),
    }
}

/// The reviewed (possibly edited) preview the user confirmed. `issue_number` is
/// Some for an existing issue (PATCHed when `update_issue`), None to create one.
#[derive(serde::Deserialize)]
pub struct SpawnEdits {
    pub issue_number: Option<u64>,
    pub issue_title: String,
    pub issue_body: String,
    pub short_title: String,
    pub color: String,
    pub emoji: String,
    /// For an existing issue: whether the title/body were changed and should be
    /// written back to GitHub. Ignored on the create path (always created).
    pub update_issue: bool,
}

/// Confirm a previewed spawn: create or update the GitHub issue as needed, then
/// build the worktree/session using the reviewed label and theming.
#[tauri::command]
pub async fn confirm_spawn(repo: String, edits: SpawnEdits, force_new: bool) -> Result<SpawnResult, String> {
    crate::log_invoke!("confirm_spawn", repo = %repo, force_new);
    let (_settings, gh) = repo_context(&repo).await?;

    let number = match edits.issue_number {
        Some(n) => {
            if edits.update_issue {
                gh.update_issue(&repo, n, &edits.issue_title, &edits.issue_body).await?;
            }
            n
        }
        None => gh.create_issue(&repo, &edits.issue_title, &edits.issue_body).await?,
    };

    let default_branch = gh.repo(&repo).await?["default_branch"].as_str().unwrap_or("main").to_string();
    let issue_url = format!("https://github.com/{repo}/issues/{number}");

    do_spawn(SpawnDecision {
        repo: &repo,
        issue_number: number,
        issue_url: &issue_url,
        default_branch: &default_branch,
        short_label: &edits.short_title,
        color: &edits.color,
        emoji: &edits.emoji,
        force_new,
    })
    .await
}

// ── Teardown ────────────────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TeardownOutcome {
    Done,
    /// Checks found unresolved work; `warnings` describes it so the UI can ask
    /// the user to confirm before destroying the worktree.
    NeedsConfirmation { warnings: Vec<String> },
    /// VS Code still has the worktree open and we couldn't close it (no
    /// Accessibility grant, or the close didn't take). `message` explains the
    /// situation; `accessibility` is true when granting Accessibility would let
    /// mAIestro Code close the window itself, so the UI can offer that shortcut.
    BlockedByEditor { message: String, accessibility: bool },
}

/// Why teardown must leave the session's branch on GitHub alone, or `None` when
/// deleting it is safe. Pure, so the policy is unit-testable without a network.
///
/// This is a **whitelist**: the branch goes only when its work is already safe —
/// the PR merged, or the branch carries nothing beyond the base branch. It is
/// deliberately stricter than teardown's own confirmation prompt, which asks
/// about destroying the *worktree*; that is not consent to destroy pushed
/// commits which, once the worktree is gone, exist nowhere else. There is no
/// override — a branch still holding unlanded work is deleted on GitHub by hand.
fn remote_delete_skip_reason(
    enabled: bool,
    has_client: bool,
    pr_open: bool,
    pr_merged: bool,
    ahead: u32,
) -> Option<&'static str> {
    if !enabled {
        return Some("delete_remote_on_teardown is off for this repo");
    }
    if !has_client {
        // Also the "GitHub was unreachable during the checks" case: without a
        // client we know nothing about the PR, so we assume the worst.
        return Some("no GitHub identity is configured for this repo");
    }
    if pr_open {
        return Some("a pull request is still open against this branch");
    }
    if !pr_merged && ahead > 0 {
        return Some("the branch has commits that were never merged");
    }
    None
}

/// Tear down a spawned session's worktree. Inspects the branch first
/// (uncommitted changes, PR state, unmerged commits); confirmation is required
/// in every case except when the PR is merged and nothing new remains. On
/// teardown the VS Code window is closed *first* (open windows have caused
/// removal failures), then the worktree, local branch, directory, and session
/// record are removed.
#[tauri::command]
#[tracing::instrument(skip_all, fields(session = %session_id))]
pub async fn teardown(session_id: String, confirmed: bool, force: bool) -> Result<TeardownOutcome, String> {
    crate::log_invoke!("teardown", confirmed, force);
    let session = crate::sessions::get(&session_id)
        .ok_or_else(|| format!("session not found: {session_id}"))?;
    let work_dir = PathBuf::from(&session.work_dir);
    let cloned_repo = expand_tilde(&session.cloned_repo_dir);
    let branch = session.branch.clone();
    let base = session.default_branch.clone();

    // ── Checks ──────────────────────────────────────────────────────────────
    let mut warnings = Vec::new();

    let dirty = git(&work_dir, &["status", "--porcelain"])
        .await
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if dirty {
        warnings.push("Worktree has uncommitted changes".to_string());
    }

    // PR state via the REST API (best-effort: needs an identity + token). The
    // client and the facts it yields outlive this block: the execute phase needs
    // them to decide whether the branch on GitHub can be safely deleted.
    let mut pr_merged = false;
    let mut pr_open = false;
    let mut github: Option<GitHub> = None;
    let settings = crate::repo_settings::repo_settings_get(session.repo.clone())?;
    if let Some(identity_id) = settings.identity_id.as_deref() {
        if let Ok(gh) = GitHub::for_identity(identity_id).await {
            if let Ok(prs) = gh.pulls_for_branch(&session.repo, &branch).await {
                pr_merged = prs.iter().any(|p| p["merged_at"].is_string());
                if let Some(open) = prs.iter().find(|p| p["state"].as_str() == Some("open")) {
                    pr_open = true;
                    warnings.push(format!("PR #{} is still open", open["number"].as_u64().unwrap_or(0)));
                }
            }
            // Fallback: once a PR merges, GitHub deletes its head branch by
            // default, after which the head-ref filter above returns nothing and
            // we'd wrongly conclude "no work on this branch". Resolve by the
            // branch's tip commit instead, which still points at the merged PR.
            if !pr_merged {
                if let Ok(sha) = git(&work_dir, &["rev-parse", "HEAD"]).await {
                    if let Ok(prs) = gh.pulls_for_commit(&session.repo, &sha).await {
                        pr_merged = prs.iter().any(|p| p["merged_at"].is_string());
                    }
                }
            }
            github = Some(gh);
        }
    }

    // Commits on the branch not yet on the base, when no merged PR accounts for them.
    let ahead = git(&work_dir, &["rev-list", "--count", &format!("origin/{base}..{branch}")])
        .await
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    if !pr_merged && ahead > 0 {
        warnings.push(format!("Branch has {ahead} commit(s) not merged"));
    }
    if !pr_merged && ahead == 0 && !dirty {
        warnings.push("Nothing has been done on this branch".to_string());
    }

    // Skip confirmation only when the PR is merged and nothing new remains.
    let safe = pr_merged && !dirty;
    if !safe && !confirmed {
        return Ok(TeardownOutcome::NeedsConfirmation { warnings });
    }

    // ── Execute ─────────────────────────────────────────────────────────────
    // 1. Close VS Code FIRST and CONFIRM the worktree is free before touching the
    //    files. Removing it out from under a live VS Code crashes the editor, so
    //    we only proceed once we can show the window is gone — never on a guess.
    //    `force` skips this entirely: the user chose "Delete anyway" knowing the
    //    open window may crash.
    if !force {
        if let Some(marker) = window_marker(&work_dir) {
            close_editor_window(&marker).await;
            let mut waited = 0u64;
            loop {
                match probe_editor_window(&marker).await {
                    // Window confirmed gone — safe to delete.
                    WinProbe::Absent => break,
                    // No Accessibility grant: we can neither close nor see the
                    // window. Fall back to the permission-free check — if nothing
                    // is using the worktree, proceed; otherwise stop and let the
                    // user close the window, grant Accessibility, or force it.
                    WinProbe::Denied => {
                        if worktree_in_use(&work_dir).await {
                            return Ok(TeardownOutcome::BlockedByEditor {
                                message:
                                    "I couldn't tear down because the Visual Studio Code window \
                                     is still open.\n\nYou have two options: close the window \
                                     yourself, or enable Accessibility for mAIestro Code so it can \
                                     close the window for you."
                                        .to_string(),
                                accessibility: true,
                            });
                        }
                        break;
                    }
                    // Window still open with Accessibility granted: the close is in
                    // flight (or the user may close it). Wait a bit, then give up.
                    WinProbe::Open => {
                        if waited >= 4000 {
                            return Ok(TeardownOutcome::BlockedByEditor {
                                message:
                                    "I couldn't tear down because the Visual Studio Code window \
                                     is still open. Close its window, then try Tear Down again."
                                        .to_string(),
                                accessibility: false,
                            });
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        waited += 300;
                    }
                }
            }
        }
    }

    // 2. Remove the worktree (force: the user confirmed discarding any changes).
    //    A spawn that failed in the background (issue #77) can leave a Session
    //    record whose worktree was never created — tolerate a missing dir so the
    //    broken row can still be torn down, just pruning any dangling admin entry.
    if work_dir.exists() {
        git(&cloned_repo, &["worktree", "remove", "--force", &work_dir.to_string_lossy()]).await?;
    } else {
        let _ = git(&cloned_repo, &["worktree", "prune"]).await;
    }

    // 3. Delete the local branch (-D: spawn unset the upstream and -d checks the
    //    wrong base, so it would refuse even for merged branches).
    if local_branch_exists(&cloned_repo, &branch).await {
        if let Err(e) = git(&cloned_repo, &["branch", "-D", &branch]).await {
            tracing::warn!(branch = %branch, error = %e, "could not delete local branch during teardown");
        }
    }

    // 4. Delete the branch on GitHub, then drop the now-stale remote-tracking
    //    ref. Guarded by `remote_delete_skip_reason` and strictly non-fatal: a
    //    teardown must never fail because GitHub was unreachable. The skip is
    //    logged with its reason so the behavior is explainable from the log.
    let delete_remote = crate::repo_settings::bool_or_default(
        settings.delete_remote_on_teardown,
        "/properties/delete_remote_on_teardown/default",
    );
    match remote_delete_skip_reason(delete_remote, github.is_some(), pr_open, pr_merged, ahead) {
        Some(reason) => {
            tracing::info!(branch = %branch, reason, "keeping the remote branch");
        }
        None => {
            // `remote_delete_skip_reason` returns `None` only when `has_client`.
            let gh = github.as_ref().expect("guard passes only with a client");
            match gh.delete_ref(&session.repo, &branch).await {
                Ok(()) => tracing::info!(branch = %branch, "deleted remote branch"),
                Err(e) => tracing::warn!(branch = %branch, error = %e, "could not delete remote branch during teardown"),
            }
        }
    }
    // The remote-tracking ref is stale either way — we just deleted the branch,
    // or GitHub deleted it on merge and nothing pruned it locally. Local-only, so
    // it costs no network and runs even when the delete was skipped.
    let _ = git(&cloned_repo, &["update-ref", "-d", &format!("refs/remotes/origin/{branch}")]).await;

    // 5. Remove the leftover wrapper dir (`<prefix><workspace>`), gating removal
    //    on the path actually starting with the repo's configured worktree
    //    prefix so we never remove_dir_all something outside it. A spawn-generated
    //    path never contains `..`; reject any that does before the string-prefix
    //    check, so a tampered session record can't tunnel out of the prefix (e.g.
    //    `.../work-x/../../../etc`) while still matching the prefix literally.
    if let Some(parent) = work_dir.parent() {
        let prefix = effective_worktree_prefix(settings.worktree_prefix.as_deref());
        let expanded = expand_tilde(&prefix);
        let has_dotdot = parent
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
        let under_prefix = parent.to_string_lossy().starts_with(&*expanded.to_string_lossy());
        // Only remove the wrapper once it's empty: the wrapper is keyed by
        // `<issue>-<slug>` alone, so two repos with an identically-slugged issue
        // share it — removing it while the sibling's worktree is inside would
        // destroy that repo's work.
        let empty = std::fs::read_dir(parent).map(|mut d| d.next().is_none()).unwrap_or(false);
        if !has_dotdot && under_prefix && parent.exists() {
            if !empty {
                tracing::info!(dir = %parent.display(), "wrapper dir not empty after teardown; leaving it in place");
            } else if let Err(e) = std::fs::remove_dir_all(parent) {
                tracing::warn!(dir = %parent.display(), error = %e, "could not remove worktree wrapper dir during teardown");
            }
        }
    }

    // 6. Drop the session record and its live status file.
    if let Err(e) = crate::sessions::delete(&session_id) {
        tracing::warn!(error = %e, "could not delete session record during teardown");
    }
    crate::status::remove(&session_id);

    tracing::info!(branch = %branch, "tore down workspace");
    Ok(TeardownOutcome::Done)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // No collisions: a plain create at the base workspace/branch/path, with the
    // emoji-less `#n — label` session label. Also proves the slug shape and that
    // the worktree path is `<prefix><workspace>/<repo>`.
    #[test]
    fn create_when_nothing_exists() {
        let plan = resolve_workspace(
            42,
            "Add Foo Bar",
            "widget",
            "/src/work-",
            false,
            |_| false,
            |_| false,
        );
        assert_eq!(
            plan,
            WorkspacePlan::Create {
                workspace: "42-add-foo-bar".into(),
                branch: "feature/42-add-foo-bar".into(),
                work_dir: PathBuf::from("/src/work-42-add-foo-bar/widget"),
                session_label: "#42 — Add Foo Bar".into(),
            }
        );
    }

    // When the base worktree dir already exists and we're not forcing new, reuse
    // it — no suffixing, no session_label (the reuse path doesn't record one).
    #[test]
    fn reuse_when_base_dir_exists() {
        let plan = resolve_workspace(
            42,
            "Add Foo",
            "widget",
            "/src/work-",
            false,
            |p| p == Path::new("/src/work-42-add-foo/widget"),
            |_| false,
        );
        assert_eq!(
            plan,
            WorkspacePlan::Reuse {
                workspace: "42-add-foo".into(),
                branch: "feature/42-add-foo".into(),
                work_dir: PathBuf::from("/src/work-42-add-foo/widget"),
            }
        );
    }

    // force_new never reuses even when the base dir exists — it suffixes past it.
    #[test]
    fn force_new_skips_reuse_and_suffixes() {
        let plan = resolve_workspace(
            42,
            "Add Foo",
            "widget",
            "/src/work-",
            true, // force_new
            |p| p == Path::new("/src/work-42-add-foo/widget"),
            |_| false,
        );
        assert_eq!(
            plan,
            WorkspacePlan::Create {
                workspace: "42-add-foo-2".into(),
                branch: "feature/42-add-foo-2".into(),
                work_dir: PathBuf::from("/src/work-42-add-foo-2/widget"),
                session_label: "#42 — Add Foo (2)".into(),
            }
        );
    }

    // A pre-existing *branch* (with no worktree dir) forces a suffix even without
    // force_new — the reuse check is dir-only, but the create loop avoids branch
    // collisions too.
    #[test]
    fn existing_branch_forces_suffix() {
        let taken: HashSet<String> = ["feature/42-add-foo".to_string()].into_iter().collect();
        let plan = resolve_workspace(
            42,
            "Add Foo",
            "widget",
            "/src/work-",
            false,
            |_| false, // no dirs exist, so no reuse
            move |b| taken.contains(b),
        );
        match plan {
            WorkspacePlan::Create { workspace, branch, session_label, .. } => {
                assert_eq!(workspace, "42-add-foo-2");
                assert_eq!(branch, "feature/42-add-foo-2");
                assert_eq!(session_label, "#42 — Add Foo (2)");
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    // Multiple consecutive collisions keep suffixing until a free pair is found.
    #[test]
    fn suffixes_past_multiple_collisions() {
        let dirs: HashSet<PathBuf> = [
            PathBuf::from("/src/work-7-x/widget"),
            PathBuf::from("/src/work-7-x-2/widget"),
            PathBuf::from("/src/work-7-x-3/widget"),
        ]
        .into_iter()
        .collect();
        let plan = resolve_workspace(
            7,
            "x",
            "widget",
            "/src/work-",
            true, // force_new so the existing base dir doesn't become a reuse
            move |p| dirs.contains(p),
            |_| false,
        );
        match plan {
            WorkspacePlan::Create { workspace, .. } => assert_eq!(workspace, "7-x-4"),
            other => panic!("expected Create, got {other:?}"),
        }
    }

    // The worktree prefix is a raw string concat (trailing text is part of the
    // dir name) and a leading `~` is expanded.
    #[test]
    fn prefix_is_concatenated_and_tilde_expanded() {
        let home = crate::paths::home();
        let plan = resolve_workspace(1, "y", "repo", "~/src/work-", false, |_| false, |_| false);
        match plan {
            WorkspacePlan::Create { work_dir, .. } => {
                assert_eq!(work_dir, home.join("src/work-1-y/repo"));
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    // ── Remote-branch delete guard ──────────────────────────────────────────

    /// The only two shapes that may delete: the work landed, or the branch never
    /// carried anything beyond base (nothing to lose either way).
    #[test]
    fn remote_delete_allowed_only_when_the_work_is_safe() {
        // Merged, even with commits the stale base ref hasn't caught up to.
        assert_eq!(remote_delete_skip_reason(true, true, false, true, 3), None);
        // Nothing on the branch beyond base.
        assert_eq!(remote_delete_skip_reason(true, true, false, false, 0), None);
    }

    /// Unmerged commits are never destroyed. Teardown's confirmation prompt is
    /// about the worktree; it is not consent to delete the only copy of pushed
    /// work, so there is deliberately no `confirmed`/`force` input here.
    #[test]
    fn remote_delete_refuses_unmerged_commits() {
        assert!(remote_delete_skip_reason(true, true, false, false, 1).is_some());
        // A PR closed without merging reads exactly like "never had a PR": not
        // merged, commits still on the branch.
        assert!(remote_delete_skip_reason(true, true, false, false, 7).is_some());
    }

    /// An open PR keeps its head branch: deleting it would close the PR.
    #[test]
    fn remote_delete_refuses_while_a_pr_is_open() {
        assert!(remote_delete_skip_reason(true, true, true, false, 0).is_some());
        // Even a merged PR plus another still open — the open one wins.
        assert!(remote_delete_skip_reason(true, true, true, true, 0).is_some());
    }

    /// Off by setting, or no client to ask (which is also the "GitHub was
    /// unreachable during the checks" case — we know nothing, so assume the worst).
    #[test]
    fn remote_delete_refuses_when_disabled_or_clientless() {
        assert!(remote_delete_skip_reason(false, true, false, true, 0).is_some());
        assert!(remote_delete_skip_reason(true, false, false, true, 0).is_some());
    }
}

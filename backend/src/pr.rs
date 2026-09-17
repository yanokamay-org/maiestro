//! Pull-request lifecycle for a spawned session's branch: surface the linked PR,
//! create one (Claude-drafted, pushing the branch first), report its checks/merge
//! readiness, and merge it after reconciling the local worktree.
//!
//! All GitHub access is via the repo's identity token (no `gh`); the polled
//! read-only commands (`session_pr`, `session_pr_checks`, `session_work_state`)
//! soft-fail to `Ok(None)` when the session is gone or the repo has no identity,
//! so the UI simply renders no pill. Extracted from `spawn.rs` (issue #99).

use std::path::{Path, PathBuf};

use crate::drafting::{claude_text, parse_issue_draft, ClaudeActivity};
use crate::gitops::{git, git_net};
use crate::plugins::GitHub;
use crate::repo_context::repo_context;

// ── Session PR link ───────────────────────────────────────────────────────────

/// A pull request associated with a session's branch, surfaced to the UI as a
/// clickable link in the session pill.
#[derive(serde::Serialize)]
pub struct PrLink {
    pub number: u64,
    pub html_url: String,
    pub title: String,
    /// One of "draft", "open", "merged", "closed".
    pub state: String,
}

/// Collapse GitHub's `state` / `draft` / `merged_at` fields into a single label.
fn pr_state(pr: &serde_json::Value) -> String {
    if pr["merged_at"].is_string() {
        "merged".to_string()
    } else if pr["state"].as_str() == Some("open") {
        if pr["draft"].as_bool().unwrap_or(false) {
            "draft".to_string()
        } else {
            "open".to_string()
        }
    } else {
        "closed".to_string()
    }
}

fn pr_link_from(pr: &serde_json::Value) -> PrLink {
    PrLink {
        number: pr["number"].as_u64().unwrap_or(0),
        html_url: pr["html_url"].as_str().unwrap_or("").to_string(),
        title: pr["title"].as_str().unwrap_or("").to_string(),
        state: pr_state(pr),
    }
}

/// The pull request to show for a session's branch, if any. Prefers the most
/// recently created open PR; otherwise the most recently created PR of any
/// state, so the link still resolves after the PR is merged or closed.
///
/// Returns `Ok(None)` — not an error — when the session is gone, the repo has
/// no identity configured, or the branch has no PRs. The UI treats all of these
/// the same: it simply renders no PR button. Only an actual API failure (e.g.
/// auth/network) surfaces as `Err`, which the frontend also degrades silently.
#[tauri::command]
#[tracing::instrument(skip_all, fields(session = %session_id))]
pub async fn session_pr(session_id: String) -> Result<Option<PrLink>, String> {
    crate::log_invoke_debug!("session_pr");
    let Some(session) = crate::sessions::get(&session_id) else {
        return Ok(None);
    };
    let settings = crate::repo_settings::repo_settings_get(session.repo.clone())?;
    let Some(identity_id) = settings.identity_id else {
        return Ok(None);
    };
    let gh = GitHub::for_identity(&identity_id).await?;
    let prs = gh.pulls_for_branch(&session.repo, &session.branch).await?;

    // `created_at` is ISO-8601, so lexicographic order is chronological.
    let created_at = |p: &&serde_json::Value| p["created_at"].as_str().unwrap_or("").to_string();
    let best = prs
        .iter()
        .filter(|p| p["state"].as_str() == Some("open"))
        .max_by_key(created_at)
        .or_else(|| prs.iter().max_by_key(created_at));

    Ok(best.map(pr_link_from))
}

// ── Create PR ───────────────────────────────────────────────────────────────────

/// Build a context blob describing the branch's changes for the PR drafter: the
/// commit log plus the diff against the base, capped so a huge diff falls back
/// to a file-level `--stat` rather than blowing past the prompt budget.
async fn change_summary(work_dir: &Path, base: &str) -> String {
    let range = format!("origin/{base}..HEAD");
    let log = git(work_dir, &["log", "--oneline", &range]).await.unwrap_or_default();
    let diff = git(work_dir, &["diff", &format!("origin/{base}...HEAD")]).await.unwrap_or_default();
    const MAX_DIFF: usize = 12_000;
    let diff_section = if diff.chars().count() > MAX_DIFF {
        let stat = git(work_dir, &["diff", "--stat", &format!("origin/{base}...HEAD")]).await.unwrap_or_default();
        format!("Diff too large to include in full; file-level summary:\n{stat}")
    } else {
        diff
    };
    format!("Commits:\n{log}\n\nDiff:\n{diff_section}")
}

/// Create a draft pull request for a session's branch, with a Claude-drafted
/// title and description. Pushes the branch to origin first (mAIestro Code's own
/// local-git op, like `git worktree add` — the launched session's own pushes are
/// separate), reuses an already-open PR instead of duplicating, and links the PR
/// to the originating issue with `Closes #N`.
#[tauri::command]
#[tracing::instrument(skip_all, fields(session = %session_id))]
pub async fn session_create_pr(
    app: tauri::AppHandle,
    session_id: String,
    request_id: String,
) -> Result<PrLink, String> {
    crate::log_invoke!("session_create_pr");
    let session = crate::sessions::get(&session_id)
        .ok_or_else(|| format!("session not found: {session_id}"))?;
    let work_dir = PathBuf::from(&session.work_dir);
    let base = session.default_branch.clone();
    let branch = session.branch.clone();

    // Guard: uncommitted changes wouldn't make it into the PR (it's built from the
    // pushed branch), so block and ask the user to commit them first.
    let dirty = git(&work_dir, &["status", "--porcelain"])
        .await
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if dirty {
        return Err("This worktree has uncommitted changes. Commit them first, then create the PR.".to_string());
    }

    let (settings, gh) = repo_context(&session.repo).await?;

    // Refresh the base ref so the ahead-count and diff compare against current origin.
    git_net(&work_dir, &["fetch", "origin", &base, "--quiet"]).await.ok();

    // Guard: nothing to open a PR for.
    let ahead = git(&work_dir, &["rev-list", "--count", &format!("origin/{base}..HEAD")])
        .await
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    if ahead == 0 {
        return Err(format!("No commits on this branch ahead of {base} to open a PR for."));
    }

    // Reuse an existing open/draft PR instead of creating a duplicate.
    if let Ok(prs) = gh.pulls_for_branch(&session.repo, &branch).await {
        if let Some(open) = prs.iter().find(|p| p["state"].as_str() == Some("open")) {
            return Ok(pr_link_from(open));
        }
    }

    // Seed the draft with the issue title/body for context (best-effort).
    let issue = gh.issue(&session.repo, session.issue_number).await.unwrap_or_default();
    let issue_title = issue["title"].as_str().unwrap_or("");
    let issue_body = issue["body"].as_str().unwrap_or("");

    let summary = change_summary(&work_dir, &base).await;
    // Instruction (default or per-repo override) first; the issue context and
    // diff are appended here so an override can't drop them. See prompts.rs.
    let instruction = crate::prompts::draft_pr(&settings.prompts);
    let prompt = format!(
        "{instruction}\n\nThis PR resolves issue #{number} (\"{issue_title}\").\
         \n\nOriginating issue body:\n{issue_body}\n\nChanges:\n{summary}",
        number = session.issue_number,
    );

    // Draft via Claude. If drafting fails (claude errored, or its reply had no
    // parseable {title, body}), log and propagate the error and abort *before*
    // pushing or opening the PR — we'd rather tell the user why than open a
    // garbage PR. The PR draft reuses the issue-draft parser but only needs
    // title + body.
    let activity = ClaudeActivity::new(app, request_id);
    let model = crate::prompts::model(&settings.prompt_model);
    let reply = claude_text(&work_dir, &prompt, &model, "drafting the PR", Some(&activity))
        .await
        .map_err(|e| { tracing::warn!(error = %e, "Claude PR draft failed"); e })?;
    let (title, body, _) = parse_issue_draft(&reply)
        .map_err(|e| { tracing::warn!(error = %e, "PR draft reply was unparseable"); e })?;

    let body = format!("{body}\n\nCloses #{}", session.issue_number);

    // Draft succeeded — now push the branch so GitHub can see the head ref. -u
    // sets upstream for the user's later pushes from the session.
    git_net(&work_dir, &["push", "-u", "origin", &branch])
        .await
        .map_err(|e| format!("could not push branch {branch}: {e}"))?;

    let pr = gh
        .create_pull(&session.repo, &title, &branch, &base, &body, true)
        .await?;
    let link = pr_link_from(&pr);
    tracing::info!(repo = %session.repo, branch = %branch, pr = link.number, "created pull request");
    Ok(link)
}

// ── PR checks & merge ─────────────────────────────────────────────────────────

/// Aggregate CI/merge state of a session's PR, surfaced to the UI to drive the
/// pill's check indicator and gate auto-merge.
#[derive(serde::Serialize)]
pub struct PrChecks {
    /// Pill indicator derived from `mergeable_state`: "passed" (mergeable),
    /// "failed" (conflicts), "pending" (behind/blocked/computing), or "none"
    /// (draft). See `merge_pill_state`.
    pub state: String,
    /// Whether mergeability is still being computed by GitHub (`mergeable_state`
    /// == "unknown") — drives the spinner.
    pub running: bool,
    /// GitHub's `mergeable_state == "clean"`: required checks and (where branch
    /// protection requires them) approvals are satisfied, so a merge will land.
    pub ready_to_merge: bool,
    /// Raw GitHub `mergeable_state` (clean / dirty / behind / blocked / unstable
    /// / draft / unknown). Lets the UI stop the auto-merge loop on states that
    /// won't self-resolve (behind, conflicts, required review) instead of waiting.
    pub mergeable_state: String,
}

/// Map GitHub's `mergeable_state` to the pill's indicator vocabulary
/// (`passed`/`failed`/`pending`/`none`) plus whether to show the computing
/// spinner. We can't read the Checks API with a fine-grained token, so
/// merge-readiness — which only needs "Pull requests: read" — is the signal the
/// pill reflects. `mergeable_state` still folds in required-check results
/// (`blocked`/`unstable`) without us touching the Checks API.
fn merge_pill_state(mergeable_state: &str) -> (String, bool) {
    match mergeable_state {
        // Mergeable: clean is fully green; unstable/has_hooks are mergeable too
        // (a non-required check may be red, but the merge will land).
        "clean" | "unstable" | "has_hooks" => ("passed".to_string(), false),
        "dirty" => ("failed".to_string(), false), // conflicts with base
        "behind" | "blocked" => ("pending".to_string(), false), // needs update / review
        "draft" => ("none".to_string(), false),
        // "unknown" / "" — GitHub is still computing mergeability; spin.
        _ => ("pending".to_string(), true),
    }
}

/// Check status for a session's open PR, if any. Mirrors `session_pr`'s soft-fail
/// contract: `Ok(None)` when the session is gone, the repo has no identity, or
/// the branch has no open PR. Only an actual API failure surfaces as `Err`, which
/// the frontend also degrades silently (no indicator).
#[tauri::command]
pub async fn session_pr_checks(session_id: String) -> Result<Option<PrChecks>, String> {
    crate::log_invoke_debug!("session_pr_checks", session_id = %session_id);
    let Some(session) = crate::sessions::get(&session_id) else {
        return Ok(None);
    };
    let settings = crate::repo_settings::repo_settings_get(session.repo.clone())?;
    let Some(identity_id) = settings.identity_id else {
        return Ok(None);
    };
    let gh = GitHub::for_identity(&identity_id).await?;
    let prs = gh.pulls_for_branch(&session.repo, &session.branch).await?;

    let created_at = |p: &&serde_json::Value| p["created_at"].as_str().unwrap_or("").to_string();
    let Some(pr) = prs
        .iter()
        .filter(|p| p["state"].as_str() == Some("open"))
        .max_by_key(created_at)
    else {
        return Ok(None);
    };
    let Some(number) = pr["number"].as_u64() else {
        return Ok(None);
    };

    // `mergeable_state` is only populated on the single-PR endpoint, not the
    // list. It needs just "Pull requests: read" — no Checks API (which a
    // fine-grained PAT can't access) — and it already reflects required-check
    // results via `blocked`/`unstable`.
    let full = gh.pull(&session.repo, number).await?;
    let mergeable_state = full["mergeable_state"].as_str().unwrap_or("unknown").to_string();
    let ready_to_merge = mergeable_state == "clean";
    let (state, running) = merge_pill_state(&mergeable_state);

    Ok(Some(PrChecks { state, running, ready_to_merge, mergeable_state }))
}

// ── Work lifecycle (local git facts) ────────────────────────────────────────────

/// Local git facts about a session's worktree, used by the UI to derive the
/// work-lifecycle phase (Planning → Implementing → Merged). The "Merged" phase
/// comes from the PR state the row already has, so this stays purely local —
/// no GitHub round-trip on a polled command.
#[derive(serde::Serialize)]
pub struct WorkState {
    /// Commits in `origin/<default_branch>..HEAD` (measured against the local
    /// remote-tracking ref — no fetch, so it can lag origin slightly).
    pub ahead: u32,
    /// Whether the worktree has uncommitted changes (`git status --porcelain`).
    pub dirty: bool,
}

/// Local git state of a session's worktree. Soft-fails to `Ok(None)` when the
/// session record or its worktree is gone (e.g. torn down mid-poll), matching
/// `session_pr` / `session_pr_checks`; the UI then renders no lifecycle pill.
/// Deliberately local-only (no `git fetch`) to stay cheap on the poll path.
#[tauri::command]
pub async fn session_work_state(session_id: String) -> Result<Option<WorkState>, String> {
    crate::log_invoke_debug!("session_work_state");
    let Some(session) = crate::sessions::get(&session_id) else {
        return Ok(None);
    };
    let work_dir = PathBuf::from(&session.work_dir);
    if !work_dir.exists() {
        return Ok(None);
    }
    let base = session.default_branch;

    let dirty = git(&work_dir, &["status", "--porcelain"])
        .await
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let ahead = git(&work_dir, &["rev-list", "--count", &format!("origin/{base}..HEAD")])
        .await
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);

    Ok(Some(WorkState { ahead, dirty }))
}

/// Merge a session's PR, creating it first if needed. Before touching GitHub it
/// reconciles the **local worktree** so the merge can't land a stale remote: it
/// blocks on uncommitted changes and pushes any committed-but-unpushed local
/// commits (so the PR head reflects local HEAD). Then it reuses/creates the PR,
/// marks a draft ready for review (a draft can't be merged and its checks don't
/// gate), and merges — but only once GitHub reports the PR `clean`.
///
/// States that won't self-resolve surface as `Err` so the UI can stop waiting:
/// `dirty` (conflicts) and `behind` (out of date with base). Transient states
/// (checks still running, GitHub recomputing) return the PR unmerged so the
/// frontend poll retries.
#[tauri::command]
#[tracing::instrument(name = "session_merge_pr", skip_all, fields(session = %session_id))]
pub async fn session_merge_pr(
    app: tauri::AppHandle,
    session_id: String,
    request_id: String,
) -> Result<PrLink, String> {
    crate::log_invoke!("session_merge_pr", session_id = %session_id, request_id = %request_id);
    let session = crate::sessions::get(&session_id)
        .ok_or_else(|| format!("session not found: {session_id}"))?;
    let work_dir = PathBuf::from(&session.work_dir);
    let branch = session.branch.clone();
    let (_settings, gh) = repo_context(&session.repo).await?;

    // Guard: uncommitted work would be silently excluded — the merge lands the
    // pushed branch, not the worktree. Block and ask the user to commit first.
    let dirty = git(&work_dir, &["status", "--porcelain"])
        .await
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if dirty {
        return Err("This worktree has uncommitted changes. Commit them first, then merge.".to_string());
    }

    // Reconcile committed-but-unpushed local commits before merging: refresh the
    // remote ref, and if local HEAD is ahead, push so the PR head includes them.
    git_net(&work_dir, &["fetch", "origin", &branch, "--quiet"]).await.ok();
    let ahead = git(&work_dir, &["rev-list", "--count", &format!("origin/{branch}..HEAD")])
        .await
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    if ahead > 0 {
        git_net(&work_dir, &["push", "origin", &branch])
            .await
            .map_err(|e| format!("could not push local commits before merging: {e}"))?;
    }

    // Ensure a PR exists: reuse an open one, else create (which drafts + pushes).
    let prs = gh.pulls_for_branch(&session.repo, &branch).await?;
    let number = match prs.iter().find(|p| p["state"].as_str() == Some("open")) {
        Some(p) => p["number"].as_u64().unwrap_or(0),
        None => session_create_pr(app, session_id.clone(), request_id).await?.number,
    };

    // Re-fetch so `draft` / `mergeable_state` reflect the (possibly just-created)
    // PR and any commits we just pushed.
    let mut pr = gh.pull(&session.repo, number).await?;

    // A draft can't be merged and its checks don't gate; promote it first.
    if pr["draft"].as_bool().unwrap_or(false) {
        let node_id = pr["node_id"].as_str().unwrap_or("").to_string();
        if node_id.is_empty() {
            return Err("could not resolve PR node id to mark it ready for review".to_string());
        }
        gh.mark_ready(&node_id).await?;
        pr = gh.pull(&session.repo, number).await?;
    }

    match pr["mergeable_state"].as_str().unwrap_or("") {
        "clean" => {
            gh.merge_pull(&session.repo, number, "merge").await?;
            let merged = gh.pull(&session.repo, number).await?;
            Ok(pr_link_from(&merged))
        }
        "dirty" => Err(format!(
            "PR #{number} has merge conflicts with {base}. Resolve them in the worktree and push, then merge.",
            base = session.default_branch,
        )),
        "behind" => Err(format!(
            "PR #{number} is behind {base}. Update the branch (merge or rebase {base}) and push, then merge.",
            base = session.default_branch,
        )),
        // blocked / unstable / unknown (null while GitHub recomputes): transient
        // or gated on checks/review — return the PR as-is so the poll keeps
        // waiting; the frontend decides when a blocker is terminal.
        _ => Ok(pr_link_from(&pr)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_state_collapses_github_fields() {
        assert_eq!(pr_state(&serde_json::json!({ "merged_at": "2026-01-01T00:00:00Z", "state": "closed" })), "merged");
        assert_eq!(pr_state(&serde_json::json!({ "state": "open", "draft": true })), "draft");
        assert_eq!(pr_state(&serde_json::json!({ "state": "open", "draft": false })), "open");
        assert_eq!(pr_state(&serde_json::json!({ "state": "open" })), "open");
        assert_eq!(pr_state(&serde_json::json!({ "state": "closed" })), "closed");
    }

    #[test]
    fn merge_pill_state_maps_mergeable_states() {
        assert_eq!(merge_pill_state("clean"), ("passed".to_string(), false));
        assert_eq!(merge_pill_state("unstable"), ("passed".to_string(), false));
        assert_eq!(merge_pill_state("has_hooks"), ("passed".to_string(), false));
        assert_eq!(merge_pill_state("dirty"), ("failed".to_string(), false));
        assert_eq!(merge_pill_state("behind"), ("pending".to_string(), false));
        assert_eq!(merge_pill_state("blocked"), ("pending".to_string(), false));
        assert_eq!(merge_pill_state("draft"), ("none".to_string(), false));
        // Unknown / still-computing states spin.
        assert_eq!(merge_pill_state("unknown"), ("pending".to_string(), true));
        assert_eq!(merge_pill_state(""), ("pending".to_string(), true));
    }
}

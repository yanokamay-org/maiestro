use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use crate::credentials::{CredentialScope, CredentialStore};
use crate::plugin::{CredentialTypeInfo, Plugin};

const TYPES: &[CredentialTypeInfo] = &[CredentialTypeInfo {
    type_id: "github_token",
    display_name: "GitHub Token",
    description: "Personal access token for GitHub API calls and git operations over HTTPS.",
}];

pub struct GitHubPlugin;

impl Plugin for GitHubPlugin {
    fn credential_types(&self) -> &[CredentialTypeInfo] { TYPES }
}

/// Result of a token health probe (`GitHub::check_token`): the authenticated
/// login and the classic-PAT OAuth scopes (empty for fine-grained tokens).
pub struct TokenInfo {
    pub login: String,
    pub scopes: Vec<String>,
}

// ── Reusable REST client ────────────────────────────────────────────────────────

/// Authenticated GitHub REST client, scoped to a single identity's token.
/// Holds the token and a shared `reqwest::Client`; methods are thin wrappers
/// over the v3 API so callers (issue listing, spawning, …) don't re-implement
/// auth, headers, and error decoding.
pub struct GitHub {
    client: reqwest::Client,
    token: String,
    /// The identity this client authenticates as — used only to scope the ETag
    /// cache so two identities never share a cached body for the same URL.
    identity_id: String,
    /// API root, e.g. `https://api.github.com`. A field (not a hardcoded literal
    /// at each call site) so tests can point the client at a local `wiremock`
    /// server. Every request URL is built through [`GitHub::api`].
    base_url: String,
}

/// The real GitHub REST API root. Every production client uses this; only tests
/// override it (via [`GitHub::for_test`]).
const DEFAULT_BASE_URL: &str = "https://api.github.com";

/// Process-wide ETag cache for conditional GETs: key → (etag, raw JSON body).
/// Keyed by identity + URL so tokens don't share cached bodies. Serving a `304
/// Not Modified` from here does NOT count against GitHub's REST rate limit —
/// which is the whole point for the polled PR/checks calls.
fn etag_cache() -> &'static Mutex<HashMap<String, (String, String)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (String, String)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

impl GitHub {
    pub async fn for_identity(identity_id: &str) -> Result<Self, String> {
        // The Keychain read blocks — in dev builds it can block for the whole
        // duration of a macOS permission prompt. Run it off the async runtime so a
        // slow/prompting Keychain can't pin a tokio worker (issue #101).
        let id = identity_id.to_string();
        let token = tokio::task::spawn_blocking(move || {
            let scope = CredentialScope::Identity { identity_id: id };
            CredentialStore::get("github_token", &scope)
        })
        .await
        .map_err(|e| format!("keychain read task failed: {e}"))?
        .map_err(|_| "No GitHub token found for this identity. Set one in Settings.".to_string())?;
        let client = reqwest::Client::builder()
            .user_agent("maiestro/0.1")
            // Bound every GitHub call so a black-holed connection fails with an
            // error the UI can degrade on instead of stalling a command forever.
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            token,
            identity_id: identity_id.to_string(),
            base_url: DEFAULT_BASE_URL.to_string(),
        })
    }

    /// A test client pointed at `base_url` (a local `wiremock` server) with a
    /// dummy token and identity. Lets `wiremock` integration tests exercise the
    /// real request/response/ETag/error logic with canned responses — no network,
    /// no Keychain. The `identity_id` is randomized per call so the process-wide
    /// ETag cache never bleeds between tests.
    #[cfg(test)]
    pub fn for_test(base_url: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            token: "test-token".to_string(),
            identity_id: format!("test-{}", uuid::Uuid::new_v4()),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Build a full request URL from an API `path` (which must start with `/`),
    /// rooted at this client's [`base_url`](Self::base_url).
    fn api(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// A request builder pre-loaded with auth and the standard GitHub headers.
    pub fn req(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            // The token goes only into this Authorization header. It must never
            // be logged — `send` below logs the method + URL but not headers.
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    /// Send a built request, logging method + URL + resulting status at `info`.
    /// The single choke point for outbound GitHub calls — credentials live in the
    /// Authorization header, which is never logged (only the URL, which GitHub
    /// never puts the token in).
    pub async fn send(&self, rb: reqwest::RequestBuilder) -> reqwest::Result<reqwest::Response> {
        // Clone to read method + URL for the log line without consuming the
        // builder. Bodies are in-memory JSON, so the clone always succeeds.
        let (method, url) = rb
            .try_clone()
            .and_then(|c| c.build().ok())
            .map(|r| (r.method().to_string(), r.url().to_string()))
            .unwrap_or_else(|| ("?".to_string(), "?".to_string()));
        match rb.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                // GETs are read-only "get status" calls (and the UI polls them), so
                // log them at debug; mutations (POST/PATCH/DELETE) stay at info. A
                // non-success response is surfaced (with its URL) by `error_message`.
                if method == "GET" {
                    tracing::debug!(target: "github", %method, %url, status, "github api call");
                } else {
                    tracing::info!(target: "github", %method, %url, status, "github api call");
                }
                Ok(resp)
            }
            Err(e) => {
                tracing::error!(target: "github", %method, %url, error = %e, "github api call failed");
                Err(e)
            }
        }
    }

    /// GET a URL and parse the JSON body, mapping non-2xx to a GitHub error
    /// message. Conditional: if we've seen this URL before, the request carries
    /// `If-None-Match` with the stored ETag; a `304 Not Modified` serves the
    /// cached body for free (no rate-limit charge). Otherwise the fresh body and
    /// its ETag are cached for next time.
    pub async fn get_json(&self, url: &str) -> Result<serde_json::Value, String> {
        let key = format!("{}\u{1}{}", self.identity_id, url);
        let cached = etag_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned();

        let mut rb = self.req(reqwest::Method::GET, url);
        if let Some((etag, _)) = &cached {
            rb = rb.header(reqwest::header::IF_NONE_MATCH, etag.clone());
        }
        let resp = self.send(rb).await.map_err(|e| e.to_string())?;

        // 304: the resource is unchanged — return the cached body. We only sent
        // the validator when `cached` was Some, so it's present here.
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            if let Some((_, body)) = cached {
                return serde_json::from_str(&body).map_err(|e| e.to_string());
            }
            return Ok(serde_json::Value::Null); // unreachable in practice
        }
        if !resp.status().is_success() {
            return Err(error_message(resp).await);
        }

        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = resp.text().await.map_err(|e| e.to_string())?;
        if let Some(etag) = etag {
            etag_cache()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, (etag, body.clone()));
        }
        serde_json::from_str(&body).map_err(|e| e.to_string())
    }

    /// Login of the token's owner (`GET /user`).
    pub async fn authenticated_login(&self) -> Result<String, String> {
        let v = self.get_json(&self.api("/user")).await?;
        v["login"].as_str().map(str::to_string).ok_or_else(|| "could not resolve token user".to_string())
    }

    /// Health-check probe of the token itself: a raw (non-ETag-cached) `GET /user`
    /// that returns the authenticated login plus the granted OAuth scopes read
    /// from the `X-OAuth-Scopes` response header. The scopes list is present for
    /// classic PATs and empty for fine-grained tokens / GitHub Apps (which don't
    /// report scopes this way — their access is inspected via a repo's
    /// `permissions` object instead). See #93.
    pub async fn check_token(&self) -> Result<TokenInfo, String> {
        let resp = self
            .send(self.req(reqwest::Method::GET, &self.api("/user")))
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(error_message(resp).await);
        }
        // Header is a comma-separated scope list, e.g. "repo, read:org". Absent
        // (or empty) for fine-grained tokens.
        let scopes: Vec<String> = resp
            .headers()
            .get("x-oauth-scopes")
            .and_then(|v| v.to_str().ok())
            .map(|s| {
                s.split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        let login = v["login"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "could not resolve token user".to_string())?;
        Ok(TokenInfo { login, scopes })
    }

    /// Repository metadata, including `default_branch`. `repo` is "owner/name".
    pub async fn repo(&self, repo: &str) -> Result<serde_json::Value, String> {
        self.get_json(&self.api(&format!("/repos/{repo}"))).await
    }

    /// A single issue. `repo` is "owner/name".
    pub async fn issue(&self, repo: &str, number: u64) -> Result<serde_json::Value, String> {
        self.get_json(&self.api(&format!("/repos/{repo}/issues/{number}"))).await
    }

    pub async fn add_assignees(&self, repo: &str, number: u64, assignees: &[String]) -> Result<(), String> {
        let url = self.api(&format!("/repos/{repo}/issues/{number}/assignees"));
        let resp = self.send(self.req(reqwest::Method::POST, &url)
            .json(&serde_json::json!({ "assignees": assignees })))
            .await.map_err(|e| e.to_string())?;
        if resp.status().is_success() { Ok(()) } else { Err(error_message(resp).await) }
    }

    /// All pull requests (any state) whose head is `branch` on `repo` ("owner/name").
    pub async fn pulls_for_branch(&self, repo: &str, branch: &str) -> Result<Vec<serde_json::Value>, String> {
        let owner = repo.split('/').next().unwrap_or("");
        let url = self.api(&format!(
            "/repos/{repo}/pulls?head={owner}:{branch}&state=all&per_page=100"
        ));
        let v = self.get_json(&url).await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// All pull requests (any state) that contain commit `sha` on `repo`
    /// ("owner/name"). Unlike `pulls_for_branch`, this resolves by commit rather
    /// than head ref, so it still finds a merged PR after its head branch has
    /// been deleted (GitHub's default on merge) — making it the reliable signal
    /// for "this work already landed via a PR".
    pub async fn pulls_for_commit(&self, repo: &str, sha: &str) -> Result<Vec<serde_json::Value>, String> {
        let url = self.api(&format!(
            "/repos/{repo}/commits/{sha}/pulls?per_page=100"
        ));
        let v = self.get_json(&url).await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// Open a new issue and return its number. `repo` is "owner/name".
    pub async fn create_issue(&self, repo: &str, title: &str, body: &str) -> Result<u64, String> {
        let url = self.api(&format!("/repos/{repo}/issues"));
        let resp = self.send(self.req(reqwest::Method::POST, &url)
            .json(&serde_json::json!({ "title": title, "body": body })))
            .await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(error_message(resp).await);
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        let number = v["number"].as_u64().ok_or_else(|| "issue created but no number returned".to_string())?;
        tracing::info!(target: "github", %repo, issue = number, "created issue");
        Ok(number)
    }

    /// Update an existing issue's title and body. `repo` is "owner/name".
    pub async fn update_issue(&self, repo: &str, number: u64, title: &str, body: &str) -> Result<(), String> {
        let url = self.api(&format!("/repos/{repo}/issues/{number}"));
        let resp = self.send(self.req(reqwest::Method::PATCH, &url)
            .json(&serde_json::json!({ "title": title, "body": body })))
            .await.map_err(|e| e.to_string())?;
        if resp.status().is_success() { Ok(()) } else { Err(error_message(resp).await) }
    }

    /// Delete a branch on `repo` ("owner/name") — `DELETE /git/refs/heads/<branch>`.
    ///
    /// "Already gone" counts as success. GitHub deletes a PR's head branch itself
    /// on merge when the repo has that option enabled, so by the time teardown
    /// runs the ref is usually absent; it answers `422 Reference does not exist`
    /// (or `404`), neither of which is a failure of what the caller asked for.
    /// This makes the call idempotent and safe to repeat.
    pub async fn delete_ref(&self, repo: &str, branch: &str) -> Result<(), String> {
        let url = self.api(&format!("/repos/{repo}/git/refs/heads/{branch}"));
        let resp = self
            .send(self.req(reqwest::Method::DELETE, &url))
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if status.is_success()
            || status == reqwest::StatusCode::NOT_FOUND
            || status == reqwest::StatusCode::UNPROCESSABLE_ENTITY
        {
            return Ok(());
        }
        Err(error_message(resp).await)
    }

    pub async fn create_comment(&self, repo: &str, number: u64, body: &str) -> Result<(), String> {
        let url = self.api(&format!("/repos/{repo}/issues/{number}/comments"));
        let resp = self.send(self.req(reqwest::Method::POST, &url)
            .json(&serde_json::json!({ "body": body })))
            .await.map_err(|e| e.to_string())?;
        if resp.status().is_success() { Ok(()) } else { Err(error_message(resp).await) }
    }

    /// Open a pull request and return the created PR object. `repo` is
    /// "owner/name"; `head` and `base` are branch names on that repo.
    pub async fn create_pull(
        &self,
        repo: &str,
        title: &str,
        head: &str,
        base: &str,
        body: &str,
        draft: bool,
    ) -> Result<serde_json::Value, String> {
        let url = self.api(&format!("/repos/{repo}/pulls"));
        let resp = self.send(self.req(reqwest::Method::POST, &url)
            .json(&serde_json::json!({
                "title": title,
                "head": head,
                "base": base,
                "body": body,
                "draft": draft,
            })))
            .await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(error_message(resp).await);
        }
        resp.json().await.map_err(|e| e.to_string())
    }

    /// A single pull request. `repo` is "owner/name". The response carries the
    /// fields the merge flow needs: `mergeable_state`, the head `sha`, `draft`,
    /// and the GraphQL `node_id` used to mark a draft ready for review.
    pub async fn pull(&self, repo: &str, number: u64) -> Result<serde_json::Value, String> {
        self.get_json(&self.api(&format!("/repos/{repo}/pulls/{number}"))).await
    }

    /// Mark a draft pull request ready for review. REST has no endpoint for this,
    /// so it goes through the GraphQL `markPullRequestReadyForReview` mutation.
    /// `node_id` is the PR's GraphQL id (the `node_id` field on the REST PR).
    pub async fn mark_ready(&self, node_id: &str) -> Result<(), String> {
        let query = "mutation($id: ID!) { \
            markPullRequestReadyForReview(input: { pullRequestId: $id }) { \
                pullRequest { isDraft } } }";
        let rb = self.req(reqwest::Method::POST, &self.api("/graphql"))
            .json(&serde_json::json!({ "query": query, "variables": { "id": node_id } }));
        let resp = self.send(rb).await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(error_message(resp).await);
        }
        // GraphQL returns 200 even on logical errors; surface them from `errors`.
        let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        if let Some(err) = v["errors"][0]["message"].as_str() {
            return Err(err.to_string());
        }
        Ok(())
    }

    /// Merge a pull request. `repo` is "owner/name"; `method` is one of
    /// "merge" / "squash" / "rebase".
    pub async fn merge_pull(&self, repo: &str, number: u64, method: &str) -> Result<(), String> {
        let url = self.api(&format!("/repos/{repo}/pulls/{number}/merge"));
        let rb = self.req(reqwest::Method::PUT, &url)
            .json(&serde_json::json!({ "merge_method": method }));
        let resp = self.send(rb).await.map_err(|e| e.to_string())?;
        if resp.status().is_success() { Ok(()) } else { Err(error_message(resp).await) }
    }
}

// ── Tauri commands ─────────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct RepoItem {
    pub full_name: String,
    pub private: bool,
    pub description: Option<String>,
}

/// Map a repo object from the GitHub API to the picker's `RepoItem`.
fn repo_item(v: &serde_json::Value) -> RepoItem {
    RepoItem {
        full_name: v["full_name"].as_str().unwrap_or("").to_string(),
        private: v["private"].as_bool().unwrap_or(false),
        description: v["description"].as_str().map(|s| s.to_string()),
    }
}

#[tauri::command]
pub async fn github_list_repos(identity_id: String) -> Result<Vec<RepoItem>, String> {
    crate::log_invoke_debug!("github_list_repos", identity = %identity_id);
    let gh = GitHub::for_identity(&identity_id).await?;

    let mut repos = Vec::new();
    let mut page: u32 = 1;

    loop {
        let resp = gh
            .send(gh.req(reqwest::Method::GET, &gh.api("/user/repos"))
                .query(&[
                    ("per_page", "100"),
                    ("page", &page.to_string()),
                    ("sort", "pushed"),
                    ("affiliation", "owner,collaborator,organization_member"),
                ]))
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            return Err(error_message(resp).await);
        }

        let page_data: Vec<serde_json::Value> = resp.json().await.map_err(|e| e.to_string())?;
        let count = page_data.len();

        for repo in page_data {
            repos.push(repo_item(&repo));
        }

        if count < 100 || page >= 10 {
            break;
        }
        page += 1;
    }

    Ok(repos)
}

/// Look up one repo by "owner/name", for tracking a repo the identity has no
/// affiliation with (so it never appears in `github_list_repos`) — see #124.
/// The returned `full_name` is GitHub's canonical spelling, which normalizes
/// the case of what the user typed.
#[tauri::command]
pub async fn github_get_repo(identity_id: String, repo: String) -> Result<RepoItem, String> {
    crate::log_invoke_debug!("github_get_repo", identity = %identity_id, repo = %repo);
    let gh = GitHub::for_identity(&identity_id).await?;
    let v = gh.repo(repo.trim()).await?;
    Ok(repo_item(&v))
}

/// An open issue plus its open sub-issues, nested recursively.
#[derive(serde::Serialize)]
pub struct IssueNode {
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub children: Vec<IssueNode>,
}

struct IssueMeta {
    title: String,
    html_url: String,
    updated_at: String, // ISO-8601; lexicographic order == chronological order
}

/// List a repo's open issues as a tree, nesting GitHub's native sub-issues under
/// their parent. `repo` is "owner/name". Pull requests are excluded, and issues
/// are ordered most-recently-modified first at every level.
#[tauri::command]
pub async fn github_list_issues(identity_id: String, repo: String) -> Result<Vec<IssueNode>, String> {
    crate::log_invoke_debug!("github_list_issues", identity = %identity_id, repo = %repo);
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| format!("invalid repo (expected owner/name): {repo}"))?;

    let gh = GitHub::for_identity(&identity_id).await?;

    // 1. Collect every open issue. The issues endpoint also returns PRs, so skip
    //    anything carrying a `pull_request` field. Remember which ones have
    //    sub-issues so we only make follow-up calls where needed.
    let mut meta: HashMap<u64, IssueMeta> = HashMap::new();
    let mut parents: Vec<u64> = Vec::new();
    let mut page: u32 = 1;

    loop {
        let resp = gh
            .send(gh.req(reqwest::Method::GET, &gh.api(&format!("/repos/{owner}/{name}/issues")))
                .query(&[
                    ("state", "open"),
                    ("sort", "updated"),
                    ("direction", "desc"),
                    ("per_page", "100"),
                    ("page", &page.to_string()),
                ]))
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            return Err(error_message(resp).await);
        }

        let data: Vec<serde_json::Value> = resp.json().await.map_err(|e| e.to_string())?;
        let count = data.len();

        for issue in data {
            if issue.get("pull_request").is_some() {
                continue;
            }
            let Some(number) = issue["number"].as_u64() else { continue };
            meta.insert(number, issue_meta(&issue));
            if issue["sub_issues_summary"]["total"].as_u64().unwrap_or(0) > 0 {
                parents.push(number);
            }
        }

        if count < 100 || page >= 10 {
            break;
        }
        page += 1;
    }

    // 2. For each issue that has sub-issues, fetch them to learn the hierarchy.
    let mut children_of: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut is_child: HashSet<u64> = HashSet::new();

    for parent in parents {
        let resp = gh
            .send(gh.req(reqwest::Method::GET, &gh.api(&format!("/repos/{owner}/{name}/issues/{parent}/sub_issues")))
                .query(&[("per_page", "100")]))
            .await
            .map_err(|e| e.to_string())?;

        // A repo without the sub-issues feature returns an error here; treat that
        // as "no children" rather than failing the whole list.
        if !resp.status().is_success() {
            continue;
        }

        let subs: Vec<serde_json::Value> = resp.json().await.map_err(|e| e.to_string())?;
        let mut kids = Vec::new();
        for sub in subs {
            let Some(n) = sub["number"].as_u64() else { continue };
            if sub["state"].as_str() != Some("open") {
                continue;
            }
            meta.entry(n).or_insert_with(|| issue_meta(&sub));
            kids.push(n);
            is_child.insert(n);
        }
        children_of.insert(parent, kids);
    }

    // 3. Order every sibling group most-recently-modified first.
    let by_recency = |a: &u64, b: &u64| {
        let ca = meta.get(a).map(|m| m.updated_at.as_str()).unwrap_or("");
        let cb = meta.get(b).map(|m| m.updated_at.as_str()).unwrap_or("");
        cb.cmp(ca).then(b.cmp(a))
    };
    for kids in children_of.values_mut() {
        kids.sort_by(by_recency);
    }

    // Roots are open issues that aren't anyone's sub-issue; build down from each.
    let mut roots: Vec<u64> = meta.keys().copied().filter(|n| !is_child.contains(n)).collect();
    roots.sort_by(by_recency);

    let mut visited = HashSet::new();
    let tree = roots
        .iter()
        .filter_map(|n| build_issue_node(*n, &meta, &children_of, &mut visited))
        .collect();

    Ok(tree)
}

fn issue_meta(issue: &serde_json::Value) -> IssueMeta {
    IssueMeta {
        title: issue["title"].as_str().unwrap_or("").to_string(),
        html_url: issue["html_url"].as_str().unwrap_or("").to_string(),
        updated_at: issue["updated_at"].as_str().unwrap_or("").to_string(),
    }
}

fn build_issue_node(
    number: u64,
    meta: &HashMap<u64, IssueMeta>,
    children_of: &HashMap<u64, Vec<u64>>,
    visited: &mut HashSet<u64>,
) -> Option<IssueNode> {
    if !visited.insert(number) {
        return None; // guard against unexpected cycles
    }
    let m = meta.get(&number)?;
    let children = children_of
        .get(&number)
        .map(|kids| {
            kids.iter()
                .filter_map(|c| build_issue_node(*c, meta, children_of, visited))
                .collect()
        })
        .unwrap_or_default();
    Some(IssueNode {
        number,
        title: m.title.clone(),
        html_url: m.html_url.clone(),
        children,
    })
}

async fn error_message(resp: reqwest::Response) -> String {
    let status = resp.status();
    // Capture the URL before `text()` consumes the response, so the error line
    // says *which* call failed (reqwest::Response doesn't expose the method).
    let url = resp.url().to_string();
    let body = resp.text().await.unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v["message"].as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
    tracing::error!(target: "github", status = status.as_u16(), %url, message = %message, "github api error");
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_meta_extracts_and_defaults() {
        let m = issue_meta(&serde_json::json!({
            "title": "Add foo",
            "html_url": "https://github.com/o/r/issues/1",
            "updated_at": "2026-01-02T00:00:00Z"
        }));
        assert_eq!(m.title, "Add foo");
        assert_eq!(m.html_url, "https://github.com/o/r/issues/1");
        assert_eq!(m.updated_at, "2026-01-02T00:00:00Z");
        // Missing fields default to empty rather than panicking.
        let empty = issue_meta(&serde_json::json!({}));
        assert_eq!(empty.title, "");
        assert_eq!(empty.html_url, "");
    }

    fn mk_meta(n: u64) -> IssueMeta {
        IssueMeta { title: format!("issue {n}"), html_url: format!("u{n}"), updated_at: String::new() }
    }

    #[test]
    fn build_issue_node_nests_children() {
        let meta = HashMap::from([(1, mk_meta(1)), (2, mk_meta(2)), (3, mk_meta(3))]);
        let children_of = HashMap::from([(1u64, vec![2u64, 3u64])]);
        let mut visited = HashSet::new();
        let node = build_issue_node(1, &meta, &children_of, &mut visited).unwrap();
        assert_eq!(node.number, 1);
        assert_eq!(node.title, "issue 1");
        let kids: Vec<u64> = node.children.iter().map(|c| c.number).collect();
        assert_eq!(kids, vec![2, 3]);
    }

    #[test]
    fn build_issue_node_guards_cycles_and_missing() {
        // A → B → A cycle: each node is emitted once, no infinite recursion.
        let meta = HashMap::from([(1, mk_meta(1)), (2, mk_meta(2))]);
        let children_of = HashMap::from([(1u64, vec![2u64]), (2u64, vec![1u64])]);
        let mut visited = HashSet::new();
        let node = build_issue_node(1, &meta, &children_of, &mut visited).unwrap();
        assert_eq!(node.children.len(), 1);
        assert_eq!(node.children[0].number, 2);
        assert!(node.children[0].children.is_empty()); // 1 already visited

        // A child with no meta entry is dropped, not rendered as a stub.
        let meta = HashMap::from([(1, mk_meta(1))]);
        let children_of = HashMap::from([(1u64, vec![99u64])]);
        let mut visited = HashSet::new();
        let node = build_issue_node(1, &meta, &children_of, &mut visited).unwrap();
        assert!(node.children.is_empty());
    }

    // ── wiremock integration tests ─────────────────────────────────────────────
    //
    // These drive the real reqwest client against a local mock server (base URL
    // injected via `GitHub::for_test`), covering the genuinely risky HTTP logic —
    // ETag conditional GETs, classic-vs-fine-grained PAT scope derivation, the
    // error-message path, and the PR lifecycle — with no network and no Keychain.

    use serde_json::json;
    use wiremock::matchers::{body_json, header, header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn get_json_serves_304_from_etag_cache() {
        let server = MockServer::start().await;
        // A conditional re-request (If-None-Match present) gets 304 with no body —
        // higher priority so it wins over the plain 200 mock when the header is set.
        Mock::given(method("GET"))
            .and(path("/repos/o/r"))
            .and(header_exists("if-none-match"))
            .respond_with(ResponseTemplate::new(304))
            .with_priority(1)
            .mount(&server)
            .await;
        // First (unconditional) request: 200 + body + ETag, which gets cached.
        Mock::given(method("GET"))
            .and(path("/repos/o/r"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"v1\"")
                    .set_body_json(json!({ "default_branch": "main" })),
            )
            .with_priority(2)
            .mount(&server)
            .await;

        let gh = GitHub::for_test(&server.uri());
        let first = gh.repo("o/r").await.unwrap();
        assert_eq!(first["default_branch"], "main");
        // Second call sends If-None-Match, receives 304, and returns the cached
        // body verbatim — identical to the first result.
        let second = gh.repo("o/r").await.unwrap();
        assert_eq!(second, first);
    }

    #[tokio::test]
    async fn repo_item_uses_githubs_canonical_full_name() {
        // The manual-add path (#124) types a repo by hand, so the API response —
        // not the typed string — is the source of truth for the tracked name.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            // GitHub resolves repo names case-insensitively, so the lookup is
            // made with what the user typed and answers with the canonical name.
            .and(path("/repos/octo/hello-world"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "full_name": "octo/Hello-World",
                "private": false,
                "description": "My first repository"
            })))
            .mount(&server)
            .await;

        // Typed in the wrong case; GitHub resolves it to the canonical spelling.
        let v = GitHub::for_test(&server.uri()).repo("octo/hello-world").await.unwrap();
        let item = repo_item(&v);
        assert_eq!(item.full_name, "octo/Hello-World");
        assert!(!item.private);
        assert_eq!(item.description.as_deref(), Some("My first repository"));
    }

    #[tokio::test]
    async fn repo_lookup_surfaces_not_found_message() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/octo/nope"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "message": "Not Found" })))
            .mount(&server)
            .await;

        let err = GitHub::for_test(&server.uri()).repo("octo/nope").await.unwrap_err();
        assert!(err.contains("Not Found"), "unexpected error: {err}");
    }

    #[test]
    fn repo_item_defaults_missing_fields() {
        let item = repo_item(&json!({}));
        assert_eq!(item.full_name, "");
        assert!(!item.private);
        assert!(item.description.is_none());
        // A null description (common on GitHub) maps to None, not "null".
        let item = repo_item(&json!({ "full_name": "o/r", "private": true, "description": null }));
        assert_eq!(item.full_name, "o/r");
        assert!(item.private);
        assert!(item.description.is_none());
    }

    #[tokio::test]
    async fn check_token_reads_classic_pat_scopes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("X-OAuth-Scopes", "repo, read:org")
                    .set_body_json(json!({ "login": "octocat" })),
            )
            .mount(&server)
            .await;

        let info = GitHub::for_test(&server.uri()).check_token().await.unwrap();
        assert_eq!(info.login, "octocat");
        assert_eq!(info.scopes, vec!["repo".to_string(), "read:org".to_string()]);
    }

    #[tokio::test]
    async fn check_token_fine_grained_has_no_scopes() {
        // Fine-grained PATs omit X-OAuth-Scopes; write access is later derived from
        // the repo `permissions.push` flag instead of the scope list.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "login": "fg-user" })))
            .mount(&server)
            .await;

        let info = GitHub::for_test(&server.uri()).check_token().await.unwrap();
        assert_eq!(info.login, "fg-user");
        assert!(info.scopes.is_empty(), "no scopes header → empty scope list");
    }

    #[tokio::test]
    async fn error_response_surfaces_github_message() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/missing"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "message": "Not Found" })))
            .mount(&server)
            .await;

        let err = GitHub::for_test(&server.uri()).repo("o/missing").await.unwrap_err();
        assert!(err.contains("Not Found"), "got: {err}");
    }

    #[tokio::test]
    async fn create_issue_returns_number_and_sends_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/o/r/issues"))
            .and(body_json(json!({ "title": "Add foo", "body": "details" })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "number": 123 })))
            .mount(&server)
            .await;

        let n = GitHub::for_test(&server.uri())
            .create_issue("o/r", "Add foo", "details")
            .await
            .unwrap();
        assert_eq!(n, 123);
    }

    #[tokio::test]
    async fn pr_lifecycle_create_fetch_merge() {
        let server = MockServer::start().await;
        // create_pull
        Mock::given(method("POST"))
            .and(path("/repos/o/r/pulls"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(json!({ "number": 7, "node_id": "PR_kw", "draft": false })),
            )
            .mount(&server)
            .await;
        // pull
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/7"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "number": 7, "mergeable_state": "clean" })),
            )
            .mount(&server)
            .await;
        // merge_pull
        Mock::given(method("PUT"))
            .and(path("/repos/o/r/pulls/7/merge"))
            .and(body_json(json!({ "merge_method": "squash" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "merged": true })))
            .mount(&server)
            .await;

        let gh = GitHub::for_test(&server.uri());
        let pr = gh.create_pull("o/r", "T", "feature/x", "main", "B", false).await.unwrap();
        assert_eq!(pr["number"], 7);
        let fetched = gh.pull("o/r", 7).await.unwrap();
        assert_eq!(fetched["mergeable_state"], "clean");
        gh.merge_pull("o/r", 7, "squash").await.expect("merge succeeds");
    }

    #[tokio::test]
    async fn for_identity_resolves_token_from_store() {
        use crate::credentials::{CredentialScope, CredentialStore};

        let id = format!("gh-id-{}", uuid::Uuid::new_v4());
        let scope = CredentialScope::Identity { identity_id: id.clone() };

        // No token yet → a clear, user-facing error (not a panic).
        assert!(GitHub::for_identity(&id).await.is_err());

        // Seed a token via the (test-backed) store; now the client constructs.
        CredentialStore::set("github_token", &scope, "ghp_x").unwrap();
        let gh = GitHub::for_identity(&id).await.expect("client builds once token exists");
        // Production clients hit the real API root.
        assert_eq!(gh.base_url, DEFAULT_BASE_URL);
    }

    #[tokio::test]
    async fn requests_carry_bearer_auth() {
        // The token rides in the Authorization header (and only there). Assert the
        // client actually sends it so the choke-point auth can't silently regress.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "login": "octocat" })))
            .mount(&server)
            .await;

        let login = GitHub::for_test(&server.uri()).authenticated_login().await.unwrap();
        assert_eq!(login, "octocat");
    }
}

//! The models each agent's CLI can run, for the repo form's drafting-model
//! suggestions.
//!
//! Only CLIs that can list their models are asked: Codex prints its catalog
//! (`codex debug models`, local and instant) and Antigravity lists what the
//! signed-in account can use (`agy models`, a network call of a second or two).
//! Claude Code has no listing command, so it returns `None` and the form keeps
//! its built-in aliases. Any failure — CLI missing, not signed in, timeout,
//! unparsable output — is also `None`: the list is only a hint, never a gate.
//!
//! Successful lists are cached per agent for [`CACHE_TTL`], so reopening
//! Settings or switching a repo's agent back and forth doesn't re-run `agy`.

use crate::agent::Agent;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a successful listing is reused before the CLI is asked again.
const CACHE_TTL: Duration = Duration::from_secs(10 * 60);

/// How long to wait for a listing command before giving up.
const QUERY_TIMEOUT: Duration = Duration::from_secs(20);

/// A listing and when it was fetched.
type Listing = (Instant, Vec<String>);

static CACHE: Mutex<Option<HashMap<Agent, Listing>>> = Mutex::new(None);

/// The model ids `agent`'s CLI reports, or `None` when it can't be asked (see
/// the module docs). Read-only and called on each repo-form mount, so logged at
/// `debug`.
#[tauri::command]
pub async fn agent_models(agent: Agent) -> Option<Vec<String>> {
    crate::log_invoke_debug!("agent_models", agent = agent.as_str());
    if let Some(hit) = cached(agent) {
        return Some(hit);
    }
    let ids = query(agent).await?;
    if let Ok(mut guard) = CACHE.lock() {
        guard.get_or_insert_with(HashMap::new).insert(agent, (Instant::now(), ids.clone()));
    }
    Some(ids)
}

fn cached(agent: Agent) -> Option<Vec<String>> {
    let guard = CACHE.lock().ok()?;
    let (at, ids) = guard.as_ref()?.get(&agent)?;
    (at.elapsed() < CACHE_TTL).then(|| ids.clone())
}

async fn query(agent: Agent) -> Option<Vec<String>> {
    let args: &[&str] = match agent {
        Agent::Claude => return None,
        Agent::Codex => &["debug", "models"],
        Agent::Antigravity => &["models"],
    };
    crate::tools::find_tool(agent.tool())?;
    let mut cmd = crate::tools::tokio_command(agent.tool());
    cmd.args(args).stdin(std::process::Stdio::null()).kill_on_drop(true);
    let out = match tokio::time::timeout(QUERY_TIMEOUT, cmd.output()).await {
        Ok(Ok(o)) if o.status.success() => o,
        Ok(Ok(o)) => {
            tracing::debug!(agent = agent.as_str(), status = %o.status, "model listing failed");
            return None;
        }
        Ok(Err(e)) => {
            tracing::debug!(agent = agent.as_str(), error = %e, "couldn't run model listing");
            return None;
        }
        Err(_) => {
            tracing::debug!(agent = agent.as_str(), "model listing timed out");
            return None;
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let ids = match agent {
        Agent::Codex => codex_model_slugs(&stdout),
        _ => agy_model_ids(&stdout),
    };
    (!ids.is_empty()).then_some(ids)
}

/// The user-selectable models in `codex debug models` JSON: the slugs whose
/// `visibility` is `"list"` (hidden/internal ones are `"hide"`), in Codex's own
/// `priority` order.
pub(crate) fn codex_model_slugs(stdout: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return Vec::new();
    };
    let mut listed: Vec<(i64, String)> = v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["visibility"] == "list")
        .filter_map(|m| Some((m["priority"].as_i64().unwrap_or(i64::MAX), m["slug"].as_str()?.to_string())))
        .collect();
    listed.sort_by_key(|(p, _)| *p);
    listed.into_iter().map(|(_, slug)| slug).collect()
}

/// The model ids in `agy models` output: the first tab-separated field of each
/// line (`gemini-3.8-flash-low\tGemini 3.8 Flash (Low)`), skipping progress
/// lines such as "Fetching available models...".
pub(crate) fn agy_model_ids(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| l.split('\t').next())
        .map(str::trim)
        .filter(|id| !id.is_empty() && !id.contains(' '))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_slugs_keep_listed_models_in_priority_order() {
        let json = r#"{"models":[
            {"slug":"gpt-5.5","visibility":"list","priority":12},
            {"slug":"gpt-reserve","visibility":"hide","priority":3},
            {"slug":"gpt-6-luna","visibility":"list","priority":3},
            {"slug":"gpt-5.6-terra","visibility":"list","priority":7}
        ]}"#;
        assert_eq!(codex_model_slugs(json), ["gpt-6-luna", "gpt-5.6-terra", "gpt-5.5"]);
    }

    #[test]
    fn codex_slugs_tolerate_bad_output() {
        assert!(codex_model_slugs("not json").is_empty());
        assert!(codex_model_slugs(r#"{"other":1}"#).is_empty());
    }

    #[test]
    fn agy_ids_take_the_first_field_and_skip_progress() {
        let out = "Fetching available models...\ngemini-3.8-flash-low\tGemini 3.8 Flash (Low)\nclaude-sonnet-4-6\tClaude Sonnet 4.6 (Thinking)\n";
        assert_eq!(agy_model_ids(out), ["gemini-3.8-flash-low", "claude-sonnet-4-6"]);
    }

    #[tokio::test]
    async fn claude_has_no_listing() {
        assert_eq!(agent_models(Agent::Claude).await, None);
    }
}

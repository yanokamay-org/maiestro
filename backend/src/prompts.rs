//! The instructions mAIestro Code sends to the repo's agent, and per-repo
//! override resolution (instructions and the per-agent drafting model).
//!
//! Each AI prompt is assembled as `<instruction> + <runtime context>`. The
//! **instruction** is the user-facing, per-repo-configurable part — the schema
//! `default` (see `backend/schemas/repo-settings.schema.json`), or an override
//! from `RepoSettings.prompts`. The **runtime context** (the user's idea, the
//! issue title/body, the diff) is always appended by `spawn.rs` and is never
//! configurable, so an override — even arbitrary text or a `/skill` invocation
//! — can't accidentally drop the data the draft depends on.
//!
//! The default instruction text lives in the JSON Schema only; we read it from
//! there via `repo_settings::schema_default` rather than hardcoding it.

use crate::agent::Agent;
use crate::repo_settings::{schema_default, PromptModels, PromptOverrides};

/// An override counts only if it has non-whitespace content; otherwise the
/// schema default (addressed by `default_ptr`) is used.
fn pick(override_: &Option<String>, default_ptr: &str) -> String {
    match override_.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => schema_default(default_ptr),
    }
}

/// The effective draft-issue instruction for this repo (override or default).
pub fn draft_issue(p: &PromptOverrides) -> String {
    pick(&p.draft_issue, "/properties/prompts/properties/draft_issue/default")
}

/// The effective short-label instruction for this repo (override or default).
pub fn short_label(p: &PromptOverrides) -> String {
    pick(&p.short_label, "/properties/prompts/properties/short_label/default")
}

/// The effective draft-PR instruction for this repo (override or default).
pub fn draft_pr(p: &PromptOverrides) -> String {
    pick(&p.draft_pr, "/properties/prompts/properties/draft_pr/default")
}

/// The effective drafting model for this repo's headless calls on `agent`: the
/// repo's `prompt_models.<agent>` override, or that entry's schema default when
/// unset/empty. `None` means "pass no `--model`" — the Codex default, which
/// defers to the model configured in Codex itself. Governs only mAIestro Code's
/// own drafting prompts, never the launched worktree session.
pub fn model(models: &PromptModels, agent: Agent) -> Option<String> {
    let (configured, ptr) = match agent {
        Agent::Claude => (&models.claude, "/properties/prompt_models/properties/claude/default"),
        Agent::Codex => (&models.codex, "/properties/prompt_models/properties/codex/default"),
    };
    Some(pick(configured, ptr)).filter(|m| !m.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each agent reads its own entry; an unset Claude entry is `haiku`, an unset
    /// Codex entry is no model at all (Codex's configured default).
    #[test]
    fn model_is_chosen_per_agent() {
        let unset = PromptModels::default();
        assert_eq!(model(&unset, Agent::Claude).as_deref(), Some("haiku"));
        assert_eq!(model(&unset, Agent::Codex), None);

        let set = PromptModels { claude: Some("sonnet".into()), codex: Some(" gpt-5-codex ".into()) };
        assert_eq!(model(&set, Agent::Claude).as_deref(), Some("sonnet"));
        assert_eq!(model(&set, Agent::Codex).as_deref(), Some("gpt-5-codex"));

        let blank = PromptModels { claude: Some("  ".into()), codex: Some("".into()) };
        assert_eq!(model(&blank, Agent::Claude).as_deref(), Some("haiku"));
        assert_eq!(model(&blank, Agent::Codex), None);
    }
}

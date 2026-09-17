//! The instructions mAIestro Code sends to Claude, and per-repo override resolution.
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

use crate::repo_settings::{schema_default, PromptOverrides};

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

/// The effective model (a `claude --model` tier alias) for this repo's headless
/// drafting calls — the repo's `prompt_model` override, or the schema default
/// (`haiku`) when unset/empty. Governs only mAIestro Code's own `claude -p` prompts,
/// never the launched worktree session.
pub fn model(prompt_model: &Option<String>) -> String {
    pick(prompt_model, "/properties/prompt_model/default")
}

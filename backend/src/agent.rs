//! The coding agent a repo uses (issues #162, #185): Claude Code, Codex CLI, or
//! Antigravity CLI (`agy`).
//!
//! One repo means one agent. It drives both the session mAIestro Code launches
//! into a worktree and mAIestro Code's own headless AI calls (drafting, the
//! health probe). The per-repo `agent` setting overrides the global one, which
//! falls back to the app-settings schema `default` — see
//! [`crate::repo_settings::effective_agent`]. A spawned session records its agent
//! so reopening never switches an existing worktree to another one.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    #[default]
    Claude,
    Codex,
    Antigravity,
}

impl Agent {
    /// The value as it appears in settings files and session records. Not
    /// necessarily the binary name — resolve the CLI with [`Agent::tool`].
    pub fn as_str(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Antigravity => "antigravity",
        }
    }

    /// The CLI binary to resolve via `crate::tools` (and its `tool_paths` key).
    /// Antigravity's CLI is `agy`, the one agent whose binary differs from its
    /// settings value.
    pub fn tool(self) -> &'static str {
        match self {
            Agent::Antigravity => "agy",
            other => other.as_str(),
        }
    }

    /// Human-facing name, for log lines, the issue comment, and the task label.
    pub fn display_name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
            Agent::Antigravity => "Antigravity",
        }
    }

    /// Parse a settings/schema value. `None` for anything unrecognized.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            "antigravity" => Some(Agent::Antigravity),
            _ => None,
        }
    }
}

impl std::fmt::Display for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_uses_the_lowercase_setting_values() {
        assert_eq!(serde_json::to_value(Agent::Codex).unwrap(), serde_json::json!("codex"));
        assert_eq!(serde_json::from_value::<Agent>(serde_json::json!("claude")).unwrap(), Agent::Claude);
        assert_eq!(serde_json::to_value(Agent::Antigravity).unwrap(), serde_json::json!("antigravity"));
        for a in [Agent::Claude, Agent::Codex, Agent::Antigravity] {
            assert_eq!(Agent::parse(a.as_str()), Some(a));
        }
        assert_eq!(Agent::parse("gemini"), None);
        assert_eq!(Agent::parse("agy"), None, "the binary name is not a settings value");
    }

    /// Antigravity is the one agent whose CLI (`agy`) differs from its setting.
    #[test]
    fn tool_is_the_binary_name() {
        assert_eq!(Agent::Claude.tool(), "claude");
        assert_eq!(Agent::Codex.tool(), "codex");
        assert_eq!(Agent::Antigravity.tool(), "agy");
    }
}

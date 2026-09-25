//! The coding agent a repo uses (issue #162): Claude Code or Codex CLI.
//!
//! One repo means one agent. It drives both the session mAIestro Code launches
//! into a worktree and mAIestro Code's own headless AI calls (drafting, the
//! health probe). The per-repo `agent` setting overrides the global one, which
//! falls back to the app-settings schema `default` — see
//! [`crate::repo_settings::effective_agent`]. A spawned session records its agent
//! so reopening never switches an existing worktree to the other one.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    #[default]
    Claude,
    Codex,
}

impl Agent {
    /// The value as it appears in settings files and session records, which is
    /// also the `tools::find_tool` name of the agent's CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    /// The CLI binary to resolve via `crate::tools`.
    pub fn tool(self) -> &'static str {
        self.as_str()
    }

    /// Human-facing name, for log lines, the issue comment, and the task label.
    pub fn display_name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
        }
    }

    /// Parse a settings/schema value. `None` for anything unrecognized.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
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
        for a in [Agent::Claude, Agent::Codex] {
            assert_eq!(Agent::parse(a.as_str()), Some(a));
        }
        assert_eq!(Agent::parse("gemini"), None);
    }
}

// The coding agents a repo can use (issues #162, #185). The backend's `Agent` enum is
// the source of truth; these are just the display names the UI shows for it.

import type { Agent } from "../api";
import ClaudeIcon from "../icons/claude.svg?react";
import OpenAIIcon from "../icons/openai.svg?react";
import AntigravityIcon from "../icons/antigravity.svg?react";

export const AGENTS: Agent[] = ["claude", "codex", "antigravity"];

/** Short name, e.g. for the session pill tooltip ("Claude · Working"). */
export const AGENT_NAMES: Record<Agent, string> = {
  claude: "Claude",
  codex: "Codex",
  antigravity: "Antigravity",
};

/** Product name, for the settings controls. */
export const AGENT_PRODUCTS: Record<Agent, string> = {
  claude: "Claude Code",
  codex: "Codex CLI",
  antigravity: "Antigravity CLI",
};

/** Each agent's single-color mark (the pill, the agent picker). */
export const AGENT_MARKS: Record<Agent, typeof ClaudeIcon> = {
  claude: ClaudeIcon,
  codex: OpenAIIcon,
  antigravity: AntigravityIcon,
};

/** Narrow an arbitrary value to an `Agent`, else `fallback`. */
export function asAgent(v: unknown, fallback: Agent): Agent {
  return AGENTS.includes(v as Agent) ? (v as Agent) : fallback;
}

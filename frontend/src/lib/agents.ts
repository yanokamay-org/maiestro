// The coding agents a repo can use (issue #162). The backend's `Agent` enum is
// the source of truth; these are just the display names the UI shows for it.

import type { Agent } from "../api";

export const AGENTS: Agent[] = ["claude", "codex"];

/** Short name, e.g. for the session pill tooltip ("Claude · Working"). */
export const AGENT_NAMES: Record<Agent, string> = {
  claude: "Claude",
  codex: "Codex",
};

/** Product name, for the settings controls. */
export const AGENT_PRODUCTS: Record<Agent, string> = {
  claude: "Claude Code",
  codex: "Codex CLI",
};

/** Narrow an arbitrary value to an `Agent`, else `fallback`. */
export function asAgent(v: unknown, fallback: Agent): Agent {
  return v === "claude" || v === "codex" ? v : fallback;
}

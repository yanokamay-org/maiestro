import { Agent, StatusRecord } from "../api";
import { AGENT_NAMES } from "../lib/agents";
import ClaudeIcon from "../icons/claude.svg?react";
import OpenAIIcon from "../icons/openai.svg?react";

// How each status state renders in a work-item row. `running`/`idle` are quiet;
// `busy` and `needs_you` draw attention. States not in the map render nothing.
// The `creating` state (worktree being built after a spawn, issue #77) is
// deliberately absent: it renders as a `workspace-op-pill` ("Creating…") next to
// the title — like "Merging…"/"Tearing down…" — not as an agent status pill, so
// AgentPill renders nothing for it.
const STATUS_LABELS: Record<string, string> = {
  running: "Ready",
  busy: "Working",
  needs_you: "Needs you",
  idle: "Idle",
};

// The agent session pill: the session's agent mark (the Claude logo, or the
// OpenAI/GPT logo for Codex) tints by live state (green=working, amber=needs you,
// muted=ready/idle), with the status word beside it. Clicking jumps to where the
// session lives — the worktree's VS Code window (there is no deep link to the
// session itself). Renders nothing until a status exists, and once the session
// has ended. `agent` is the one the worktree was spawned with.
export function AgentPill({
  agent,
  status,
  onClick,
}: {
  agent: Agent;
  status?: StatusRecord;
  onClick: () => void;
}) {
  if (!status || status.state === "ended") return null;
  const label = STATUS_LABELS[status.state];
  if (!label) return null;
  const name = AGENT_NAMES[agent] ?? AGENT_NAMES.claude;
  // `needs_you` carries the reason (e.g. the permission request) in `detail`.
  const title = status.detail ? `${name} · ${label} — ${status.detail}` : `${name} · ${label}`;
  // A *surfaced* failed tool tints the pill red; a pending/transient one the
  // agent may still recover from doesn't. The error itself lives in the
  // dismissible row block, not this tooltip. (Codex never reports one.)
  const cls = `agent-pill agent-pill--${status.state}${status.state === "busy" ? " busy-ring busy-ring--ai" : ""}${status.last_error?.surfaced ? " agent-pill--error" : ""}`;
  const Mark = agent === "codex" ? OpenAIIcon : ClaudeIcon;
  return (
    <button
      className={cls}
      onClick={onClick}
      title={title}
      aria-label={title}
    >
      <Mark className="agent-pill-mark" />
      <span className="agent-pill-label">{label}</span>
    </button>
  );
}

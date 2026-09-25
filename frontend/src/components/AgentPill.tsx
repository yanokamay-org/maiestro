import { Agent, StatusRecord } from "../api";
import { AGENT_MARKS, AGENT_NAMES } from "../lib/agents";

// How each status state renders in a work-item row. `running`/`idle` are quiet;
// `busy` and `needs_you` draw attention. Anything else — no status yet, an
// `ended` session, or the `creating` state (worktree being built after a spawn,
// issue #77, which has its own "Creating…" `workspace-op-pill` next to the
// title) — renders as `idle`, so the pill always shows.
const STATUS_LABELS: Record<string, string> = {
  running: "Ready",
  busy: "Working",
  needs_you: "Needs you",
  idle: "Idle",
};

// The agent session pill: the session's agent mark (the Claude logo, the
// OpenAI/GPT logo for Codex, the bell-shaped "A" for Antigravity) tints by live state (green=working, amber=needs you,
// muted=ready/idle), with the status word beside it. Clicking jumps to where the
// session lives — the worktree's VS Code window (there is no deep link to the
// session itself). Always rendered: with no live status it reads "Idle".
// `agent` is the session record's agent.
export function AgentPill({
  agent,
  status,
  onClick,
}: {
  agent: Agent;
  status?: StatusRecord;
  onClick: () => void;
}) {
  // No live status (not started, ended, or still creating) reads as idle.
  const live = status && STATUS_LABELS[status.state] ? status : undefined;
  const state = live?.state ?? "idle";
  const label = STATUS_LABELS[state];
  const name = AGENT_NAMES[agent] ?? AGENT_NAMES.claude;
  // `needs_you` carries the reason (e.g. the permission request) in `detail`.
  const title = live?.detail ? `${name} · ${label} — ${live.detail}` : `${name} · ${label}`;
  // A *surfaced* failed tool tints the pill red; a pending/transient one the
  // agent may still recover from doesn't. The error itself lives in the
  // dismissible row block, not this tooltip. (Codex never reports one.)
  const cls = `agent-pill agent-pill--${state}${state === "busy" ? " busy-ring busy-ring--ai" : ""}${live?.last_error?.surfaced ? " agent-pill--error" : ""}`;
  const Mark = AGENT_MARKS[agent] ?? AGENT_MARKS.claude;
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

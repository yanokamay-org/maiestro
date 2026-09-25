import { Agent } from "../api";
import { AGENT_MARKS, AGENT_PRODUCTS, AGENTS } from "../lib/agents";
import { OverlayDialog } from "./OverlayDialog";

// Opened from a work item's "Agentic Coding CLI…" command (issue #186): pick which agent
// this worktree launches. The current one is shown but not selectable; choosing
// another calls `session_set_agent`, after which the row may offer a VS Code
// restart if the window is still running the old agent.
export function AgentSwitchDialog({ title, current, onConfirm, onClose }: {
  title: string;
  current: Agent;
  onConfirm: (agent: Agent) => void;
  onClose: () => void;
}) {
  return (
    <OverlayDialog title={`Agentic Coding CLI · ${title}`} panelClass="hide-dialog" onClose={onClose}>
      <div className="hide-dialog-body">
        <p className="codex-hooks-lead">
          Choose the agentic coding CLI this worktree starts in VS Code. The worktree, branch, and color stay
          the same, but the current conversation doesn&apos;t carry over to the other one.
        </p>
        {AGENTS.map((a) => {
          const Mark = AGENT_MARKS[a];
          const isCurrent = a === current;
          return (
            <button
              key={a}
              className={`hide-choice agent-choice${isCurrent ? " agent-choice--current" : ""}`}
              disabled={isCurrent}
              aria-current={isCurrent || undefined}
              onClick={() => onConfirm(a)}
            >
              <span className="agent-choice-text">
                <span className="hide-choice-label agent-choice-label">
                  <Mark className="agent-choice-mark" aria-hidden />
                  {AGENT_PRODUCTS[a]}
                </span>
                <span className="hide-choice-hint">
                  {isCurrent ? "This worktree's agentic coding CLI now" : `Switch to ${AGENT_PRODUCTS[a]}`}
                </span>
              </span>
              {isCurrent && <span className="agent-choice-badge">✓ Current</span>}
            </button>
          );
        })}
        <p className="session-hint">
          If this worktree&apos;s VS Code window is open, you&apos;ll be asked to restart it so the new
          agent starts.
        </p>
      </div>
    </OverlayDialog>
  );
}

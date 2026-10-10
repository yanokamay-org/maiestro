import { Agent, PrChecks, PrLink, Session, TerminalHost, StatusRecord, WindowPermission } from "../api";
import { AGENT_NAMES } from "../lib/agents";
import { accentColor, checkLabel, PR_STATE_ICONS, PrOpenIcon } from "../lib/lifecycle";
import { effectiveHidden, formatSnoozeRemaining } from "../lib/snooze";
import { AgentPill } from "./AgentPill";
import { DismissibleError } from "./DismissibleError";
import { HideCommandButton, SnoozeLabel } from "./HideControls";
import GitHubIcon from "../icons/github.svg?react";
import FolderIcon from "../icons/folder.svg?react";
import VSCodeIcon from "../icons/vscode.svg?react";
import TerminalIcon from "../icons/terminal.svg?react";
import CmuxIcon from "../icons/cmux.svg?react";
import WindowsTerminalIcon from "../icons/windows-terminal.svg?react";
import ChevronRightIcon from "../icons/chevron-right.svg?react";

// A pending Tear Down prompt: a warnings confirmation, or a "window still open"
// block (which may offer the Accessibility or Automation shortcut so mAIestro
// Code can close it).
export type TeardownPrompt =
  | { id: string; kind: "confirm"; warnings: string[] }
  | { id: string; kind: "blocked"; message: string; permission: WindowPermission | null };

/** How each terminal host is named in the row: the open button, the restart
 *  prompt, and the blocked prompts. */
export const TERMINAL_HOST_UI: Record<TerminalHost, { name: string; Icon: typeof VSCodeIcon }> = {
  vscode: { name: "VS Code", Icon: VSCodeIcon },
  terminal_app: { name: "Terminal", Icon: TerminalIcon },
  cmux: { name: "cmux", Icon: CmuxIcon },
  windows_terminal: { name: "Windows Terminal", Icon: WindowsTerminalIcon },
};

/** The button that opens System Settings at a missing grant. */
const PERMISSION_BUTTON: Record<WindowPermission, string> = {
  accessibility: "Open Accessibility Options",
  automation: "Open Automation Options",
};

// A pending agent-switch prompt: the session's window (VS Code or Terminal) still
// runs `editorAgent` after a switch to `agent` (offered right after the switch, or
// when opening the window later), a restart that couldn't close the window, or a
// failed switch/restart.
export type AgentPrompt =
  | { id: string; kind: "restart"; reason: "switched" | "open"; agent: Agent; editorAgent: Agent }
  | { id: string; kind: "blocked"; message: string; permission: WindowPermission | null }
  | { id: string; kind: "error"; message: string };

export type PrCreateState = { creating?: boolean; requestId?: string; error?: string };
export type PrMergeState = { intent?: boolean; merging?: boolean; requestId?: string; error?: string };

export interface SessionRowProps {
  session: Session;
  status: StatusRecord | undefined;
  pr: PrLink | null | undefined;
  checks: PrChecks | null | undefined;
  prCreate: PrCreateState | undefined;
  prMerge: PrMergeState | undefined;
  teardownBusy: boolean;
  teardownConfirm: TeardownPrompt | null;
  agentPrompt: AgentPrompt | null;
  agentBusy: boolean;
  cmdOpen: boolean;
  repoHidden: boolean;
  now: number;
  openErr: string | undefined;
  busyCls: (requestId?: string) => string;
  busyRingCls: (requestId?: string) => string;
  onToggleCommands: () => void;
  onOpenInEditor: () => void;
  onOpenPath: () => void;
  onOpenUrl: (url: string) => void;
  onOpenPermissionSettings: (permission: WindowPermission) => void;
  onCreatePr: () => void;
  onStartMerge: () => void;
  onTearDown: () => void;
  onChooseAgent: () => void;
  onFocusEditor: () => void;
  onRestartEditor: () => void;
  onDismissAgentPrompt: () => void;
  onRunTeardown: (confirmed: boolean, force: boolean) => void;
  onHide: () => void;
  onUnhide: () => void;
  onCancelTeardownConfirm: () => void;
  onDismissPrCreateError: () => void;
  onDismissPrMergeError: () => void;
  onDismissToolError: () => void;
  onDismissOpenError: () => void;
  onDismissNotice: () => void;
}

export function SessionRow({
  session: s, status, pr, checks, prCreate: prc, prMerge: pm, teardownBusy, teardownConfirm, agentPrompt, agentBusy,
  cmdOpen, repoHidden, now, openErr, busyCls, busyRingCls,
  onToggleCommands, onOpenInEditor, onOpenPath, onOpenUrl, onOpenPermissionSettings,
  onCreatePr, onStartMerge, onTearDown, onChooseAgent, onFocusEditor, onRestartEditor, onDismissAgentPrompt, onRunTeardown, onHide, onUnhide,
  onCancelTeardownConfirm, onDismissPrCreateError, onDismissPrMergeError, onDismissToolError, onDismissOpenError, onDismissNotice,
}: SessionRowProps) {
  // While the worktree is still being built in the background (issue #77),
  // actions that need it to exist are disabled.
  const creating = status?.state === "creating";
  // Only surfaced errors render; pending/transient ones (the agent may still
  // recover) stay hidden until promoted (issue #48).
  const toolErr = status?.last_error?.surfaced ? status.last_error : undefined;
  const PrIcon = pr ? (PR_STATE_ICONS[pr.state] ?? PrOpenIcon) : null;
  const sessHidden = effectiveHidden(s.hidden, now);
  const sessSnoozeLabel = s.hidden?.snooze_until ? formatSnoozeRemaining(s.hidden.snooze_until, now) : "";
  // An active PR (open or still a draft) already covers this branch, so disable
  // Create PR; the pill links to it.
  const prOpen = pr?.state === "open" || pr?.state === "draft";
  // A mAIestro Code operation in flight on this row. The rainbow "working" pill keeps
  // the feedback visible after the command strip collapses on click; mirrors the
  // command buttons' busy flags so it clears on completion or failure.
  const agent = s.agent ?? "claude";
  // The app the session runs in, recorded at spawn (older records: VS Code).
  const terminalHost = TERMINAL_HOST_UI[s.terminal_host ?? "vscode"] ?? TERMINAL_HOST_UI.vscode;
  // Switching mid-PR-create/merge or mid-teardown would fight over the window.
  const switchBusy = creating || teardownBusy || !!prc?.creating || (!!pm?.intent && pr?.state !== "merged");
  const prompt = agentPrompt?.id === s.id ? agentPrompt : null;
  const opLabel = creating
    ? "Creating…"
    : prc?.creating
      ? "Creating PR…"
      : pm?.intent && pr?.state !== "merged"
        ? "Merging…"
        : teardownBusy
          ? "Tearing down…"
          : null;
  // Request id of the in-flight op (if it can run the agent), so the op-pill glows
  // rainbow only while that AI call is active. Teardown is pure git, so none.
  const opRequestId = prc?.creating
    ? prc.requestId
    : pm?.intent && pr?.state !== "merged"
      ? pm.requestId
      : undefined;
  return (
    <div className={`workspace-item ${sessHidden || repoHidden ? "workspace-item--hidden" : ""}`}>
      <div className="workspace-row" style={{ borderLeft: `3px solid ${accentColor(s.color)}` }}>
        <AgentPill agent={agent} status={status} onClick={() => { if (!creating) onOpenInEditor(); }} />
        <span className="workspace-title">{s.session_title}</span>
        {opLabel && <span className={`workspace-op-pill ${busyRingCls(opRequestId)}`}>{opLabel}</span>}
        {sessHidden && (
          <SnoozeLabel snoozeLabel={sessSnoozeLabel} expanded={cmdOpen} onClick={onToggleCommands} />
        )}
        <div className="session-pill">
          {pr && PrIcon && (
            <button
              className="pill-btn pill-btn--pr"
              onClick={() => onOpenUrl(pr.html_url)}
              title={`Open PR #${pr.number} (${pr.state}) on GitHub`}
              aria-label="Open pull request on GitHub"
            >
              <PrIcon />
              #{pr.number}
              {checks && checks.state !== "none" && (
                <span
                  className={`check-dot check-dot--${checks.state}${checks.running ? " check-dot--spin" : ""}`}
                  title={checkLabel(checks)}
                  aria-label={checkLabel(checks)}
                />
              )}
            </button>
          )}
          <button
            className="pill-btn"
            onClick={() => onOpenUrl(s.issue_url)}
            title={`Open issue #${s.issue_number} on GitHub`}
          >
            <GitHubIcon />
            #{s.issue_number}
          </button>
          <button
            className="pill-btn"
            onClick={onOpenPath}
            disabled={creating}
            title={creating ? "Still creating this workspace…" : `Reveal in Finder: ${s.work_dir}`}
            aria-label="Open folder in Finder"
          >
            <FolderIcon />
          </button>
          <button
            className="pill-btn"
            onClick={onOpenInEditor}
            disabled={creating}
            title={creating ? "Still creating this workspace…" : `Open in ${terminalHost.name}`}
            aria-label={`Open in ${terminalHost.name}`}
          >
            <terminalHost.Icon style={{ color: accentColor(s.color) }} />
          </button>
        </div>
        <button
          className={`row-expander ${cmdOpen ? "row-expander--open" : ""}`}
          onClick={onToggleCommands}
          title="Commands"
          aria-label="Commands"
          aria-expanded={cmdOpen}
        >
          <ChevronRightIcon />
        </button>
      </div>
      <div className={`command-strip ${cmdOpen ? "command-strip--open" : ""}`}>
        <button
          className={`command-btn ${prc?.creating ? busyCls(prc.requestId) : ""}`}
          onClick={onCreatePr}
          disabled={prc?.creating || prOpen || creating}
          title={creating ? "Still creating this workspace…" : prOpen ? `PR #${pr?.number} is already open` : undefined}
        >
          {prc?.creating ? "Creating PR…" : "Create PR"}
        </button>
        <button
          className={`command-btn ${pm?.intent && pr?.state !== "merged" ? busyCls(pm.requestId) : ""}`}
          onClick={onStartMerge}
          disabled={pm?.intent || pr?.state === "merged" || creating}
          title={
            creating
              ? "Still creating this workspace…"
              : pr?.state === "merged"
                ? `PR #${pr.number} is already merged`
                : "Create the PR if needed, wait for checks, then merge"
          }
        >
          {pr?.state === "merged" ? "Merged" : pm?.intent ? "Merging…" : "Merge PR"}
        </button>
        <button
          className={`command-btn ${agentBusy ? "btn-busy" : ""}`}
          onClick={onChooseAgent}
          disabled={agentBusy || switchBusy}
          title={creating ? "Still creating this workspace…" : `Choose this worktree's agentic coding CLI (now ${AGENT_NAMES[agent]})`}
        >
          Agentic Coding CLI…
        </button>
        <HideCommandButton hidden={sessHidden} onHide={onHide} onUnhide={onUnhide} />
        <button
          className={`command-btn ${teardownBusy ? "btn-busy" : ""}`}
          disabled={teardownBusy || creating}
          title={creating ? "Still creating this workspace…" : undefined}
          onClick={onTearDown}
        >
          Tear Down
        </button>
      </div>
      {prc?.error && (
        <DismissibleError variant="list" lead="Couldn't create the PR" message={prc.error} onDismiss={onDismissPrCreateError} />
      )}
      {pm?.error && (
        <DismissibleError variant="list" lead="Couldn't merge the PR" message={pm.error} onDismiss={onDismissPrMergeError} />
      )}
      {toolErr && (
        <DismissibleError
          lead={toolErr.tool ? `${toolErr.tool} failed` : "A tool call failed"}
          message={toolErr.message}
          onDismiss={onDismissToolError}
        />
      )}
      {openErr && (
        <DismissibleError lead="Couldn't open" message={openErr} onDismiss={onDismissOpenError} />
      )}
      {s.notice && (
        <DismissibleError variant="list" lead="mAIestro Code changed this worktree" message={s.notice} onDismiss={onDismissNotice} />
      )}
      {prompt?.kind === "error" && (
        <DismissibleError lead="Couldn't switch the agentic coding CLI" message={prompt.message} onDismiss={onDismissAgentPrompt} />
      )}
      {prompt?.kind === "restart" && (
        <div className="cleanup-confirm">
          <p className="cleanup-lead">
            {prompt.reason === "open"
              ? `This session was switched to ${AGENT_NAMES[prompt.agent]}, but its ${terminalHost.name} window is still running ${AGENT_NAMES[prompt.editorAgent]}.`
              : `${terminalHost.name} is still running ${AGENT_NAMES[prompt.editorAgent]} in this worktree.`}{" "}
            Restart the window to start {AGENT_NAMES[prompt.agent]}? The {AGENT_NAMES[prompt.editorAgent]} conversation won't carry over.
          </p>
          <div className="issue-actions">
            <button className={`btn-save ${agentBusy ? "btn-busy" : ""}`} disabled={agentBusy} onClick={onRestartEditor}>
              Restart {terminalHost.name}
            </button>
            <button className="btn-ghost" onClick={onDismissAgentPrompt}>Later</button>
          </div>
        </div>
      )}
      {prompt?.kind === "blocked" && (
        <div className="cleanup-confirm">
          <p className="cleanup-lead" style={{ whiteSpace: "pre-line" }}>{prompt.message}</p>
          <div className="issue-actions">
            <button className="btn-ghost" onClick={onFocusEditor}>Open {terminalHost.name}</button>
            {prompt.permission && (
              <button className="btn-ghost" onClick={() => onOpenPermissionSettings(prompt.permission!)}>
                {PERMISSION_BUTTON[prompt.permission]}
              </button>
            )}
            <button className="btn-ghost" onClick={onDismissAgentPrompt}>Cancel</button>
          </div>
        </div>
      )}
      {teardownConfirm?.id === s.id && teardownConfirm.kind === "confirm" && (
        <div className="cleanup-confirm">
          <p className="cleanup-lead">Remove this workspace?</p>
          <ul className="cleanup-warnings">
            {teardownConfirm.warnings.map((w, i) => (
              <li key={i}>{w}</li>
            ))}
          </ul>
          <div className="issue-actions">
            <button
              className={`btn-danger ${teardownBusy ? "btn-busy" : ""}`}
              disabled={teardownBusy}
              onClick={() => onRunTeardown(true, false)}
            >
              Remove anyway
            </button>
            <button className="btn-ghost" onClick={onCancelTeardownConfirm}>Cancel</button>
          </div>
        </div>
      )}
      {teardownConfirm?.id === s.id && teardownConfirm.kind === "blocked" && (
        <div className="cleanup-confirm">
          <p className="cleanup-lead" style={{ whiteSpace: "pre-line" }}>{teardownConfirm.message}</p>
          <div className="issue-actions">
            <button className="btn-ghost" onClick={onOpenInEditor}>Open in {terminalHost.name}</button>
            {teardownConfirm.permission && (
              <button className="btn-ghost" onClick={() => onOpenPermissionSettings(teardownConfirm.permission!)}>
                {PERMISSION_BUTTON[teardownConfirm.permission]}
              </button>
            )}
            <button
              className={`btn-danger ${teardownBusy ? "btn-busy" : ""}`}
              disabled={teardownBusy}
              onClick={() => onRunTeardown(true, true)}
            >
              Delete anyway
            </button>
            <button className="btn-ghost" onClick={onCancelTeardownConfirm}>Cancel</button>
          </div>
        </div>
      )}
    </div>
  );
}

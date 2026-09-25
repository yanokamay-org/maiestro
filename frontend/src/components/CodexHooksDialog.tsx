import { OverlayDialog } from "./OverlayDialog";

// Shown right before a Codex session opens in VS Code, only when the backend has
// asked Codex and it will prompt the user to review mAIestro Code's status hooks
// (`codex_hooks_review_needed`): the first Codex session, or after the hook
// definitions change. Codex runs a hook only once the user trusts it; the hooks
// are identical for every workspace, so one "trust all" covers them all.
export function CodexHooksDialog({ onContinue, onClose }: {
  onContinue: () => void;
  onClose: () => void;
}) {
  return (
    <OverlayDialog title="One-time step in Codex" panelClass="hide-dialog codex-hooks-dialog" onClose={onClose}>
      <div className="hide-dialog-body">
        <p className="codex-hooks-lead">
          mAIestro Code shows each session&apos;s live status (<em>Working</em>, <em>Needs you</em>,{" "}
          <em>Idle</em>) through Codex hooks. Codex runs hooks only after you approve them, so when
          the session starts it will ask you to review them.
        </p>
        <p className="codex-hooks-lead">
          When Codex asks, choose <strong>trust all</strong>.
        </p>
        <p className="session-hint">
          You only need to do this once. It covers every Codex workspace from now on.
        </p>
        <div className="issue-actions">
          <button className="btn-save" autoFocus onClick={onContinue}>Open in VS Code</button>
          <button className="btn-ghost" onClick={onClose}>Cancel</button>
        </div>
      </div>
    </OverlayDialog>
  );
}

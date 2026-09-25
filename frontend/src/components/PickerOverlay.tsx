import { Ref } from "react";
import { IssueNode } from "../api";
import { ActiveSessions } from "../lib/lifecycle";
import { slugify } from "../lib/issues";
import { IssueRow } from "./IssueRow";
import { OverlayDialog } from "./OverlayDialog";

// The editable spawn preview, shown in the main area before a worktree is made.
// `issueNumber` is null on the create path (issue opened only on confirm).
// `orig*` capture the fetched/drafted values so we know whether to PATCH GitHub.
export type Preview = {
  // "spawn" creates a worktree (session name + slugs); "create" just opens the
  // issue and returns to the list.
  mode: "spawn" | "create";
  issueNumber: number | null;
  issueTitle: string;
  issueBody: string;
  shortTitle: string;
  color: string;
  emoji: string;
  repoName: string;
  worktreePrefix: string;
  origTitle: string;
  origBody: string;
  spawning: boolean;
  // True while the agent's short-title suggestion is in flight, so the Session name
  // field shows a "generating title" rainbow indicator.
  suggesting?: boolean;
  error?: string;
};

export type Picker = {
  repo: string;
  loading: boolean;
  issues: IssueNode[] | null;
  error?: string;
  query: string;
  note?: string;
  // Which create button is in flight, so we can disable both and spin the
  // active one. Undefined when idle.
  creating?: "create" | "spawn";
  // Request id of the in-flight draft, correlating `agent-activity` events so
  // the busy glow turns rainbow exactly while the agent is drafting.
  creatingRequestId?: string;
  // True while the issue list is being re-fetched via the refresh button.
  refreshing?: boolean;
  // Issue number whose spawn preview is currently being prepared (the row's
  // "Spawn Work" button glows until the preview opens or preparation fails).
  preparing?: number;
  // Set when the agent couldn't draft a clear issue: holds the original idea and
  // the agent's reply, prompting the user to confirm creating from raw text.
  // `action` records which button triggered it, so the retry repeats it.
  confirm?: { idea: string; message: string; action: "create" | "spawn" };
  // When set, the overlay shows the spawn preview instead of the issue list.
  preview?: Preview;
};

export type Expand = { kind: "idea" } | { kind: "issue"; number: number } | null;

function SpawnPreview({ pv, onPatch, onConfirm, onCancel }: {
  pv: Preview;
  onPatch: (patch: Partial<Preview>) => void;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const isSpawn = pv.mode === "spawn";
  // Derived names update live as the session name is edited. The backend
  // re-derives authoritatively (and resolves collisions).
  const slug = slugify(pv.shortTitle) || "…";
  // On the create path the issue number isn't known yet, so show a `{issue #}`
  // placeholder where it will be inserted on spawn.
  const numPart = pv.issueNumber != null ? String(pv.issueNumber) : "{issue #}";
  const workspace = `${numPart}-${slug}`;
  const issueDirty = pv.issueNumber != null && (pv.issueTitle !== pv.origTitle || pv.issueBody !== pv.origBody);
  const canConfirm = !!pv.issueTitle.trim() && (!isSpawn || !!pv.shortTitle.trim());
  return (
    <div className="overlay-body preview-body">
      {isSpawn && (
        <>
          <label className="field-label field-label--row" htmlFor="preview-session-name">
            Session name
            {pv.suggesting && <span className="field-busy-note">Generating title…</span>}
          </label>
          <div className={`title-field${pv.suggesting ? " busy-ring busy-ring--ai" : ""}`}>
            <input
              id="preview-session-name"
              className="text-input"
              type="text"
              value={pv.shortTitle}
              autoFocus
              placeholder="short session label"
              disabled={pv.spawning}
              onChange={(e) => onPatch({ shortTitle: e.target.value })}
            />
          </div>
        </>
      )}

      <label className="field-label" htmlFor="preview-issue-title">Issue title</label>
      <input
        id="preview-issue-title"
        className="text-input"
        value={pv.issueTitle}
        autoFocus={!isSpawn}
        disabled={pv.spawning}
        onChange={(e) => onPatch({ issueTitle: e.target.value })}
        spellCheck={false}
      />

      <label className="field-label" htmlFor="preview-issue-body">Issue description</label>
      <textarea
        id="preview-issue-body"
        className="text-input preview-desc"
        value={pv.issueBody}
        rows={5}
        disabled={pv.spawning}
        onChange={(e) => onPatch({ issueBody: e.target.value })}
        spellCheck={false}
      />

      {isSpawn && (
        <div className="preview-derived">
          <div className="preview-derived-row">
            <span className="preview-derived-label">Session</span>
            <span className="preview-session">
              <span className="preview-swatch" style={{ background: pv.color }} />
              {pv.emoji} #{numPart} — {pv.shortTitle || "…"}
            </span>
          </div>
          <div className="preview-derived-row">
            <span className="preview-derived-label">Workspace</span>
            <code>{workspace}</code>
          </div>
          <div className="preview-derived-row">
            <span className="preview-derived-label">Branch</span>
            <code>feature/{workspace}</code>
          </div>
          <div className="preview-derived-row">
            <span className="preview-derived-label">Worktree</span>
            <code>{pv.worktreePrefix}{workspace}/{pv.repoName}</code>
          </div>
        </div>
      )}

      {!isSpawn ? (
        <p className="session-hint">A new issue will be created on GitHub.</p>
      ) : pv.issueNumber == null ? (
        <p className="session-hint">A new issue is created on spawn; its number is prefixed to the branch and worktree names.</p>
      ) : issueDirty ? (
        <p className="session-hint">Saving will update issue #{pv.issueNumber} on GitHub.</p>
      ) : null}
      {pv.error && <p className="cred-error">{pv.error}</p>}

      <div className="issue-actions">
        <button
          className={`btn-save ${pv.spawning ? "btn-busy" : ""}`}
          disabled={pv.spawning || !canConfirm}
          onClick={onConfirm}
        >
          {isSpawn ? (pv.issueNumber == null ? "Create & Spawn" : "Spawn") : "Create Issue"}
        </button>
        <button className="btn-ghost" disabled={pv.spawning} onClick={onCancel}>Cancel</button>
      </div>
    </div>
  );
}

export interface PickerOverlayProps {
  picker: Picker;
  expanded: Expand;
  active: ActiveSessions;
  ordered: IssueNode[];
  ideaRef: Ref<HTMLTextAreaElement>;
  busyCls: (requestId?: string) => string;
  onClose: () => void;
  onQueryChange: (query: string) => void;
  onIdeaFocus: () => void;
  onCancelIdea: () => void;
  onCreateIssue: () => void;
  onCreateAndSpawn: () => void;
  onRefresh: () => void;
  onExpandIssue: (n: number) => void;
  onCollapseIssue: () => void;
  onSpawnIssue: (n: IssueNode) => void;
  onConfirmRaw: () => void;
  onDismissConfirm: () => void;
  onPatchPreview: (patch: Partial<Preview>) => void;
  onConfirmPreview: () => void;
  onCancelPreview: () => void;
}

export function PickerOverlay({
  picker, expanded, active, ordered, ideaRef, busyCls,
  onClose, onQueryChange, onIdeaFocus, onCancelIdea, onCreateIssue, onCreateAndSpawn,
  onRefresh, onExpandIssue, onCollapseIssue, onSpawnIssue, onConfirmRaw, onDismissConfirm,
  onPatchPreview, onConfirmPreview, onCancelPreview,
}: PickerOverlayProps) {
  const ideaOpen = expanded?.kind === "idea";
  const busy = !!picker.creating;
  const title = `${picker.preview ? (picker.preview.mode === "create" ? "New issue" : "Review & spawn") : "Start work"} · ${picker.repo}`;
  return (
    <OverlayDialog title={title} onClose={onClose}>
      {picker.preview ? (
        <SpawnPreview
          pv={picker.preview}
          onPatch={onPatchPreview}
          onConfirm={onConfirmPreview}
          onCancel={onCancelPreview}
        />
      ) : (
        <>
          <div className={`overlay-search-wrap ${ideaOpen ? "idea-open" : ""}`}>
            <textarea
              ref={ideaRef}
              className="text-input idea-textarea"
              aria-label="Your idea"
              placeholder="Write your own idea…"
              rows={1}
              value={picker.query}
              onFocus={onIdeaFocus}
              onChange={(e) => onQueryChange(e.target.value)}
              onKeyDown={(e) => {
                // ⌘/Ctrl+Enter submits the primary action; plain Enter is a newline.
                if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
                  e.preventDefault();
                  if (picker.query.trim() && !busy) onCreateAndSpawn();
                }
              }}
              spellCheck={false}
              autoCapitalize="off"
              autoCorrect="off"
            />
            {ideaOpen && (
              <div className="issue-actions">
                <button
                  className={`btn-ghost ${picker.creating === "create" ? busyCls(picker.creatingRequestId) : ""}`}
                  disabled={!picker.query.trim() || busy}
                  onClick={onCreateIssue}
                >
                  Create Issue
                </button>
                <button
                  className={`btn-save ${picker.creating === "spawn" ? busyCls(picker.creatingRequestId) : ""}`}
                  disabled={!picker.query.trim() || busy}
                  onClick={onCreateAndSpawn}
                >
                  Create Issue and Spawn
                </button>
                <button
                  className="btn-ghost"
                  disabled={busy}
                  onClick={onCancelIdea}
                >
                  Cancel
                </button>
              </div>
            )}
          </div>
          <div className="overlay-body">
            <div className="issue-list-toolbar">
              <span className="issue-list-label">Open issues</span>
              <button
                className={`btn-add ${picker.refreshing ? "btn-busy" : ""}`}
                onClick={onRefresh}
                disabled={picker.loading || picker.refreshing}
                title="Refresh issues"
              >
                {picker.refreshing ? "Refreshing…" : "↻ Refresh"}
              </button>
            </div>
            {picker.loading ? (
              <p className="repo-group-empty">Loading issues…</p>
            ) : picker.error ? (
              <p className="cred-error">{picker.error}</p>
            ) : ordered.length > 0 ? (
              <div className="issue-tree">
                {ordered.map((n) => (
                  <IssueRow
                    key={n.number}
                    node={n}
                    depth={0}
                    expandedNumber={expanded?.kind === "issue" ? expanded.number : null}
                    preparingNumber={picker.preparing ?? null}
                    active={active}
                    onExpand={onExpandIssue}
                    onCollapse={onCollapseIssue}
                    onSpawn={onSpawnIssue}
                  />
                ))}
              </div>
            ) : (
              <p className="repo-group-empty">{picker.query ? "No matching issues." : "No open issues."}</p>
            )}
            {picker.confirm && (
              <div className="confirm-block">
                <p className="confirm-lead">The agent couldn’t turn this into a clear issue:</p>
                <p className="confirm-msg">{picker.confirm.message}</p>
                <div className="issue-actions">
                  <button className="btn-save" onClick={onConfirmRaw}>Create issue from my text</button>
                  <button className="btn-ghost" onClick={onDismissConfirm}>Cancel</button>
                </div>
              </div>
            )}
            {picker.note && <p className="issue-note">{picker.note}</p>}
          </div>
        </>
      )}
    </OverlayDialog>
  );
}

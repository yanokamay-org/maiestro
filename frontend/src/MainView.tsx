import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, Agent, AvailableUpdate, SettingsProblem, DraftPreviewOutcome, HideState, IssueNode, PrChecks, PrLink, RepoSettings, Session, SpawnEdits, SpawnPlan, StatusRecord, WorkState } from "./api";
import {
  ActiveSessions,
  furtherPhase,
  LIFECYCLE_LABELS,
  LifecyclePhase,
  lifecyclePhase,
  mergeBlocker,
  ZONES,
} from "./lib/lifecycle";
import { effectiveHidden, formatSnoozeRemaining, HideTarget } from "./lib/snooze";
import { filterIssues } from "./lib/issues";
import { useTauriListen } from "./hooks/useTauriListen";
import { ResizeGrips } from "./components/ResizeGrips";
import { DismissibleError } from "./components/DismissibleError";
import { UpdateBanner } from "./components/UpdateBanner";
import { SettingsProblemBanner } from "./components/SettingsProblemBanner";
import { HideCommandButton, SnoozeLabel } from "./components/HideControls";
import { RemoveConfirm } from "./components/RemoveConfirm";
import { HideSnoozeDialog } from "./components/HideSnoozeDialog";
import { AgentSwitchDialog } from "./components/AgentSwitchDialog";
import { CodexHooksDialog } from "./components/CodexHooksDialog";
import { AgentPrompt, SessionRow, TeardownPrompt, PrCreateState, PrMergeState } from "./components/SessionRow";
import { PickerOverlay, Picker, Preview, Expand } from "./components/PickerOverlay";
import LogoIcon from "./icons/logo.svg?react";
import GearIcon from "./icons/gear.svg?react";
import EyeIcon from "./icons/eye.svg?react";
import VSCodeIcon from "./icons/vscode.svg?react";
import ChevronRightIcon from "./icons/chevron-right.svg?react";

// Opening Settings goes through the backend so this and the tray menu share one
// show path — the one that tells the Settings webview about each fresh open, so
// it re-reads settings it may have fetched before onboarding ran (#139).
// Best-effort: the window is defined in tauri.conf.json, so a failure here is
// nothing the popover can act on.
function openSettings() {
  api.openSettings().catch(() => {});
}

export function MainView() {
  const [repos, setRepos] = useState<string[]>([]);
  const [picker, setPicker] = useState<Picker | null>(null);
  // Which thing in the overlay is expanded: the idea box, one issue row, or none.
  const [expanded, setExpanded] = useState<Expand>(null);
  // The multi-line idea box; resized to fit its content (capped in CSS).
  const ideaRef = useRef<HTMLTextAreaElement | null>(null);

  const [sessions, setSessions] = useState<Session[]>([]);
  // PR link per session id, discovered live from GitHub. `null` = looked up, none
  // found (or the lookup failed); absent key = not looked up yet.
  const [prs, setPrs] = useState<Record<string, PrLink | null>>({});
  // Local git state per session id (commits ahead / dirty), used to derive the
  // work-lifecycle phase. Refreshed alongside the PR lookup on popover open.
  const [workStates, setWorkStates] = useState<Record<string, WorkState | null>>({});
  // Live status per workspace id (busy / needs_you / idle / …), seeded on open
  // and kept current by the backend's `session-status` event.
  const [statuses, setStatuses] = useState<Record<string, StatusRecord>>({});
  // Session id whose inline command strip is expanded (only one at a time).
  const [commandsOpen, setCommandsOpen] = useState<string | null>(null);
  // Pending Tear Down prompt for a session: either a warnings confirmation, or a
  // "VS Code still open" block that can offer the Accessibility shortcut.
  const [teardownConfirm, setTeardownConfirm] = useState<TeardownPrompt | null>(null);
  // Session ids whose teardown call is currently in flight, so the triggering
  // button glows while the backend removes the worktree.
  const [teardownBusy, setTeardownBusy] = useState<Record<string, boolean>>({});
  // Create-PR progress/error per session id: `{ creating }` while in flight,
  // `{ error }` after a failure. Absent = idle. `requestId` correlates
  // `agent-activity` events to this action's busy glow.
  const [prCreate, setPrCreate] = useState<Record<string, PrCreateState>>({});
  // PR check status per session id, polled from GitHub while the popover is open.
  // Absent = not yet fetched; null = no open PR (or lookup failed).
  const [prChecks, setPrChecks] = useState<Record<string, PrChecks | null>>({});
  // Merge-PR state per session id. `intent` keeps the auto-merge watcher armed
  // until the PR lands; `merging` guards against overlapping merge attempts.
  // `requestId` correlates `agent-activity` events (the merge drafts the PR
  // via the agent when none exists yet) to this action's busy glow.
  const [prMerge, setPrMerge] = useState<Record<string, PrMergeState>>({});
  // Request ids with an agent call currently in flight (`agent-activity`
  // events). A busy button whose request id is here glows rainbow instead of
  // the monochrome sweep; absent = plain. Missed events degrade to monochrome.
  const [aiActive, setAiActive] = useState<Record<string, boolean>>({});
  // Whether the menu-bar popover is currently open. Gates check polling so we
  // don't hit GitHub while the window is hidden.
  const [popoverOpen, setPopoverOpen] = useState(true);
  // Global toggle: reveal hidden/snoozed repos and work items (dimmed).
  const [showHidden, setShowHidden] = useState(false);
  // Per-repo settings, keyed by repo full_name — the source of repo-level hide state.
  const [repoSettings, setRepoSettings] = useState<Record<string, RepoSettings>>({});
  // Open repo options menu (repo full_name); at most one at a time.
  const [repoMenuOpen, setRepoMenuOpen] = useState<string | null>(null);
  // Per-repo "Open in VS Code" launch error, keyed by repo full_name.
  const [openRepoErr, setOpenRepoErr] = useState<Record<string, string>>({});
  // Per-session "Open in VS Code / Finder" launch error, keyed by session id.
  const [openSessionErr, setOpenSessionErr] = useState<Record<string, string>>({});
  // The row's agent-switch prompt (restart / blocked / error), and the session
  // ids whose switch or restart is in flight (issue #186).
  const [agentPrompt, setAgentPrompt] = useState<AgentPrompt | null>(null);
  const [agentBusy, setAgentBusy] = useState<Record<string, boolean>>({});
  // The session whose "AI Agent…" picker is open.
  const [agentTarget, setAgentTarget] = useState<Session | null>(null);
  // Target of the hide/snooze dialog, or null when closed.
  const [hideTarget, setHideTarget] = useState<HideTarget | null>(null);
  // A spawn or reopen waiting on the one-time Codex hook-trust notice.
  const [hooksNotice, setHooksNotice] = useState<{ proceed: () => void } | null>(null);
  // Repo (full_name) awaiting remove confirmation; at most one at a time.
  const [removeConfirm, setRemoveConfirm] = useState<string | null>(null);
  // Per-repo removal failure message, keyed by repo full_name.
  const [removeErr, setRemoveErr] = useState<Record<string, string>>({});

  const refreshSessions = useCallback(() => {
    api.sessionsList().then((list) => {
      setSessions(list);
      // Fire PR lookups in parallel; each settles its own entry and soft-fails,
      // so rows render immediately and a PR button appears as its lookup lands.
      for (const s of list) {
        api.sessionPr(s.id)
          .then((pr) => setPrs((prev) => ({ ...prev, [s.id]: pr })))
          .catch(() => setPrs((prev) => ({ ...prev, [s.id]: null })));
        api.sessionWorkState(s.id)
          .then((ws) => setWorkStates((prev) => ({ ...prev, [s.id]: ws })))
          .catch(() => setWorkStates((prev) => ({ ...prev, [s.id]: null })));
      }
    }).catch(() => {});
  }, []);

  // Replace the status map with a fresh snapshot from disk. Called on open so a
  // reopened/reloaded popover reflects current state even if it missed events.
  const refreshStatuses = useCallback(() => {
    api.sessionsStatusList().then((list) => {
      const next: Record<string, StatusRecord> = {};
      for (const s of list) next[s.workspace] = s;
      setStatuses(next);
    }).catch(() => {});
  }, []);

  // The backend's update check (#182) runs on its own clock; the popover just
  // reads its verdict on each show. A dismiss clears it locally right away and
  // the backend remembers the version, so the next read agrees.
  const [update, setUpdate] = useState<AvailableUpdate | null>(null);
  const refreshUpdate = useCallback(() => {
    api.updateStatus().then((s) => setUpdate(s.available)).catch(() => {});
  }, []);
  const dismissUpdate = useCallback((version: string) => {
    setUpdate(null);
    api.updateDismiss(version).catch(() => {});
  }, []);

  // An unreadable ~/.maiestro/settings.json (hand-edit typo). While it stands
  // the backend refuses every write to it, so say so where the user is looking;
  // re-read on each show, so fixing the file clears the banner on the next open.
  const [settingsProblem, setSettingsProblem] = useState<SettingsProblem | null>(null);
  const refreshSettingsProblem = useCallback(() => {
    api.appSettingsProblem().then(setSettingsProblem).catch(() => {});
  }, []);

  const refreshAll = useCallback(() => {
    api.listRepos().then((list) => {
      setRepos(list);
      // Fan out per-repo settings so repo-level hide state lands as each settles,
      // mirroring how PR lookups are fired in refreshSessions.
      for (const repo of list) {
        api.getRepoSettings(repo)
          .then((s) => setRepoSettings((prev) => ({ ...prev, [repo]: s })))
          .catch(() => {});
      }
    }).catch(() => {});
    refreshSessions();
    refreshStatuses();
    refreshUpdate();
    refreshSettingsProblem();
  }, [refreshSessions, refreshStatuses, refreshUpdate, refreshSettingsProblem]);

  useEffect(() => {
    refreshAll();
  }, [refreshAll]);

  // The popover hides on blur, so each tray-icon click is a fresh open. Re-fetch
  // on "popover-shown" (and on regained focus) so it never shows stale sessions
  // or PR state after work happened while it was hidden. The same signals track
  // whether the popover is open, which gates check polling.
  useTauriListen("popover-shown", () => { setPopoverOpen(true); refreshAll(); });
  useEffect(() => {
    const unlisten = getCurrentWindow().onFocusChanged(({ payload: focused }) => {
      if (focused) refreshAll();
      else setPopoverOpen(false);
    });
    return () => { unlisten.then((f) => f()); };
  }, [refreshAll]);

  // Live status updates from the backend's status-file watcher. Each event is one
  // workspace's latest record; `ended` clears its row indicator.
  // Latest statuses for the listener below, which must compare against the
  // previous state without closing over a stale map.
  const statusesRef = useRef(statuses);
  statusesRef.current = statuses;
  useTauriListen<StatusRecord>("session-status", (rec) => {
    // A background spawn just finished building the worktree: re-read the
    // session record, which may now carry a notice (e.g. a .gitignore change).
    if (statusesRef.current[rec.workspace]?.state === "creating" && rec.state !== "creating") refreshSessions();
    setStatuses((prev) => {
      if (rec.state === "ended") {
        const { [rec.workspace]: _drop, ...rest } = prev;
        return rest;
      }
      return { ...prev, [rec.workspace]: rec };
    });
  });

  // Live agent-call signal from the backend: while a request id is active its
  // button's busy glow turns rainbow (AI), reverting to the monochrome sweep
  // when the call ends — so mixed script/AI actions change color mid-flight.
  useTauriListen<{ request_id: string; active: boolean }>("agent-activity", ({ request_id, active }) => {
    setAiActive((prev) => {
      if (!active) {
        const { [request_id]: _drop, ...rest } = prev;
        return rest;
      }
      return { ...prev, [request_id]: true };
    });
  });

  // Busy classes for a button whose backend command can run the agent: rainbow
  // while its request id has an agent call in flight, monochrome otherwise.
  const busyCls = (requestId?: string) =>
    requestId && aiActive[requestId] ? "btn-busy btn-busy--ai" : "btn-busy";

  // Busy-ring classes for the row's "working" pill: rainbow (AI) while the
  // operation's agent call is in flight, single-hue (non-AI) otherwise — so a
  // Create PR glows rainbow while the agent drafts the body, then reverts for the
  // git push/merge. Teardown passes no request id and stays monochrome.
  const busyRingCls = (requestId?: string) =>
    requestId && aiActive[requestId] ? "busy-ring busy-ring--ai" : "busy-ring";

  // Grow the idea box to fit its content (reset to auto first so it can also
  // shrink), capped by the CSS max-height which then scrolls.
  useEffect(() => {
    const el = ideaRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [picker?.query, expanded]);

  // Tracked sessions grouped by repo full_name.
  const sessionsByRepo = useMemo(() => {
    const grouped: Record<string, Session[]> = {};
    for (const s of sessions) (grouped[s.repo] ??= []).push(s);
    return grouped;
  }, [sessions]);

  function closePicker() {
    setPicker(null);
    setExpanded(null);
  }

  async function openStartWork(repo: string) {
    setExpanded(null);
    setPicker({ repo, loading: true, issues: null, query: "" });
    // Guarded functional updates: a late response must not resurrect a picker
    // the user closed, nor clobber one they opened for another repo since.
    const settle = (next: Picker) => setPicker((p) => (p && p.repo === repo ? next : p));
    try {
      const settings = await api.getRepoSettings(repo);
      if (!settings.identity_id) {
        settle({ repo, loading: false, issues: null, query: "", error: "No identity assigned. Set one in Settings → Repo." });
        return;
      }
      const issues = await api.githubListIssues(settings.identity_id, repo);
      settle({ repo, loading: false, issues, query: "" });
    } catch (e) {
      settle({ repo, loading: false, issues: null, query: "", error: String(e) });
    }
  }

  // Re-fetch the repo's open issues (and sessions, so "working" pills are
  // current) without closing the overlay. Best-effort: keep the list on failure.
  async function refreshIssues() {
    if (!picker) return;
    const repo = picker.repo;
    refreshSessions();
    setPicker((p) => (p ? { ...p, refreshing: true } : p));
    try {
      const settings = await api.getRepoSettings(repo);
      if (settings.identity_id) {
        const issues = await api.githubListIssues(settings.identity_id, repo);
        setPicker((p) => (p && p.repo === repo ? { ...p, issues } : p));
      }
    } catch { /* keep the existing list */ }
    setPicker((p) => (p ? { ...p, refreshing: false } : p));
  }

  // Turn a backend SpawnPlan into the editable preview state. `mode` decides
  // what the preview offers: "spawn" shows the session name + derived slugs and
  // creates a worktree; "create" just opens the issue (no worktree).
  function planToPreview(plan: SpawnPlan, mode: "spawn" | "create"): Preview {
    return {
      mode,
      issueNumber: plan.issue_number,
      issueTitle: plan.issue_title,
      issueBody: plan.issue_body,
      shortTitle: plan.short_title,
      color: plan.color,
      emoji: plan.emoji,
      repoName: plan.repo_name,
      worktreePrefix: plan.worktree_prefix,
      origTitle: plan.issue_title,
      origBody: plan.issue_body,
      spawning: false,
    };
  }

  // Patch fields of the open preview.
  function setPreview(patch: Partial<Preview>) {
    setPicker((p) => (p && p.preview ? { ...p, preview: { ...p.preview, ...patch } } : p));
  }

  // Existing issue → open the spawn preview (no worktree yet; fetches the body).
  async function spawnIssue(node: IssueNode) {
    if (!picker) return;
    const repo = picker.repo;
    setPicker((p) => (p ? { ...p, preparing: node.number, note: `Preparing #${node.number}…` } : p));
    try {
      const plan = await api.prepareSpawn(repo, node.number);
      // `suggesting: true` shows the "generating title" indicator on the Session
      // name field until the AI suggestion lands (or the call fails).
      setPicker((p) => (p ? { ...p, preparing: undefined, note: undefined, preview: { ...planToPreview(plan, "spawn"), suggesting: true } } : p));
      // Fire-and-forget: upgrade the heuristic label to an AI suggestion once
      // the agent replies. The preview is already open and usable meanwhile.
      void suggestLabel(repo, node.number, plan.short_title);
    } catch (e) {
      setPicker((p) => (p ? { ...p, preparing: undefined, note: `Failed to prepare #${node.number}: ${String(e)}` } : p));
    }
  }

  // Swap the spawn preview's short label for the agent's suggestion — only if the
  // preview is still open for this same issue and the field still holds the
  // heuristic value (the user hasn't typed). Failures are silent: the
  // heuristic label is a fine fallback.
  async function suggestLabel(repo: string, issueNumber: number, heuristic: string) {
    let suggestion = "";
    try {
      suggestion = await api.suggestShortTitle(repo, issueNumber);
    } catch { /* keep the heuristic label */ }
    // Clear the "generating title" indicator and, if the preview is still open
    // for this same issue and untouched, swap in the suggestion. Runs on both
    // success and failure so the indicator never sticks.
    setPicker((p) => {
      const pv = p?.preview;
      if (!p || !pv || p.repo !== repo || pv.mode !== "spawn" || pv.issueNumber !== issueNumber) return p;
      const useSuggestion = !!suggestion.trim() && pv.shortTitle === heuristic;
      return { ...p, preview: { ...pv, suggesting: false, shortTitle: useSuggestion ? suggestion : pv.shortTitle } };
    });
  }

  function applyDraftPreview(res: DraftPreviewOutcome, idea: string, mode: "spawn" | "create") {
    if (res.status === "needs_confirmation") {
      setPicker((p) => (p ? { ...p, creating: undefined, creatingRequestId: undefined, note: undefined, confirm: { idea, message: res.message, action: mode } } : p));
    } else {
      setPicker((p) => (p ? { ...p, creating: undefined, creatingRequestId: undefined, note: undefined, confirm: undefined, preview: planToPreview(res, mode) } : p));
    }
  }

  // Cancel the preview → back to the issue list (overlay stays open).
  function cancelPreview() {
    setPicker((p) => (p ? { ...p, preview: undefined, note: undefined } : p));
  }

  // Before VS Code opens a Codex session that will ask the user to review our
  // status hooks, explain the one-time `/hooks` → trust all step. The backend
  // asks Codex itself, so this only shows when Codex really will prompt; the
  // action runs on "Open in VS Code" and is dropped on Cancel.
  async function withCodexHooksNotice(repo: string, sessionId: string | undefined, proceed: () => void) {
    if (await api.codexHooksReviewNeeded(repo, sessionId)) setHooksNotice({ proceed });
    else proceed();
  }

  // Confirm a spawn preview: create/update the issue, then spawn. On success the
  // overlay closes and we land back in the main window.
  async function confirmSpawnNow() {
    if (!picker?.preview) return;
    const repo = picker.repo;
    const pv = picker.preview;
    if (!pv.shortTitle.trim() || !pv.issueTitle.trim()) return;
    withCodexHooksNotice(repo, undefined, () => void spawnFromPreview(repo, pv));
  }

  async function spawnFromPreview(repo: string, pv: NonNullable<Picker["preview"]>) {
    setPreview({ spawning: true, error: undefined });
    const edits: SpawnEdits = {
      issue_number: pv.issueNumber,
      issue_title: pv.issueTitle,
      issue_body: pv.issueBody,
      short_title: pv.shortTitle,
      color: pv.color,
      emoji: pv.emoji,
      update_issue: pv.issueNumber !== null && (pv.issueTitle !== pv.origTitle || pv.issueBody !== pv.origBody),
    };
    try {
      // confirm_spawn now returns as soon as the worktree build is handed to a
      // background task, so this resolves quickly: close the overlay and land on
      // the dashboard, where the new row shows a "Creating…" pill (driven by the
      // backend's `creating` status) until the worktree is ready. A thrown error
      // here is a synchronous failure (issue create/update, identity) — keep the
      // overlay open and show it.
      await api.confirmSpawn(repo, edits);
      refreshSessions();
      refreshStatuses();
      closePicker();
    } catch (e) {
      setPreview({ spawning: false, error: String(e) });
    }
  }

  // Confirm a create-only preview: open the issue from the edited title/body,
  // surface it in the list (optimistically + a reconciling refetch), and return
  // to the list. No worktree is created.
  async function createFromPreview() {
    if (!picker?.preview) return;
    const repo = picker.repo;
    const pv = picker.preview;
    if (!pv.issueTitle.trim()) return;
    setPreview({ spawning: true, error: undefined });
    try {
      const res = await api.createIssueDirect(repo, pv.issueTitle, pv.issueBody);
      if (res.status !== "created") {
        setPreview({ spawning: false, error: "Unexpected response while creating the issue." });
        return;
      }
      const node: IssueNode = { number: res.number, title: pv.issueTitle, html_url: res.issue_url, children: [] };
      setPicker((p) => {
        if (!p) return p;
        // Insert the new issue locally — we know exactly what it is, and GitHub's
        // list endpoint lags, so refetching would briefly drop it. The next
        // popover refresh reconciles with GitHub.
        const present = (p.issues ?? []).some((n) => n.number === res.number);
        const issues = present ? p.issues : [node, ...(p.issues ?? [])];
        return { ...p, preview: undefined, query: "", note: `Created ${res.issue_url}`, issues };
      });
    } catch (e) {
      setPreview({ spawning: false, error: String(e) });
    }
  }

  // The preview's primary button: spawn or create-only depending on its mode.
  function confirmPreview() {
    if (picker?.preview?.mode === "create") createFromPreview();
    else confirmSpawnNow();
  }

  // Idea → AI draft → preview (issue isn't opened until confirm). `mode` picks
  // create-only vs. spawn; `raw` skips the agent's clarity gate on a retry.
  async function draftIdea(mode: "create" | "spawn", idea: string, raw = false) {
    if (!picker) return;
    const repo = picker.repo;
    const trimmed = idea.trim();
    if (!trimmed) return;
    const requestId = crypto.randomUUID();
    const note = raw ? "Drafting from your text…" : `Drafting an issue for “${trimmed}”…`;
    setPicker((p) => (p ? { ...p, creating: mode, creatingRequestId: requestId, note, confirm: undefined } : p));
    try {
      applyDraftPreview(await api.draftSpawnPreview(repo, trimmed, requestId, raw), trimmed, mode);
    } catch (e) {
      setPicker((p) => (p ? { ...p, creating: undefined, creatingRequestId: undefined, note: `${raw ? "Failed" : "Failed to draft issue"}: ${String(e)}` } : p));
    }
  }

  // User chose to proceed from their raw text despite the agent's reply. Repeats
  // whichever action raised it, landing on its preview built from the raw text.
  function confirmRaw() {
    if (!picker?.confirm) return;
    draftIdea(picker.confirm.action, picker.confirm.idea, true);
  }

  // "Create PR": push the branch, draft a description with the agent, and open a
  // draft PR. On success the PR pill refreshes to link the new PR (we don't
  // open it in the browser — the pill is the entry point).
  async function createPr(s: Session) {
    const requestId = crypto.randomUUID();
    setPrCreate((prev) => ({ ...prev, [s.id]: { creating: true, requestId } }));
    try {
      const pr = await api.createPr(s.id, requestId);
      setPrs((prev) => ({ ...prev, [s.id]: pr }));
      setPrCreate((prev) => ({ ...prev, [s.id]: {} }));
    } catch (e) {
      setPrCreate((prev) => ({ ...prev, [s.id]: { error: String(e) } }));
    }
  }

  // One merge attempt for a session: ensure-create + mark-ready + merge-if-clean
  // happen in the backend. A landed PR (`state === "merged"`) clears the intent;
  // a PR that isn't mergeable yet comes back unmerged and stays armed for the
  // next poll to retry; a hard failure (conflict, auth) surfaces in the panel.
  const runMerge = useCallback(async (id: string) => {
    const requestId = crypto.randomUUID();
    setPrMerge((prev) => ({ ...prev, [id]: { ...prev[id], intent: true, merging: true, requestId, error: undefined } }));
    try {
      const pr = await api.mergePr(id, requestId);
      setPrs((prev) => ({ ...prev, [id]: pr }));
      setPrMerge((prev) =>
        pr.state === "merged"
          ? { ...prev, [id]: {} }
          : { ...prev, [id]: { ...prev[id], merging: false } },
      );
    } catch (e) {
      setPrMerge((prev) => ({ ...prev, [id]: { error: String(e) } }));
    }
  }, []);

  // "Merge PR": arm the auto-merge watcher and take the first attempt now, which
  // creates the PR if missing and promotes a draft so its checks start running.
  function startMerge(s: Session) {
    void runMerge(s.id);
  }

  // Latest sessions/PR/merge state reachable from the polling interval without
  // re-arming it on every keystroke of state.
  const pollRef = useRef({ sessions, prs, prMerge });
  pollRef.current = { sessions, prs, prMerge };
  // Monotonic per-session poll counter, so a slow earlier check can't land after
  // a newer one and overwrite it (a flickering dot on out-of-order responses).
  const pollSeqRef = useRef<Record<string, number>>({});

  // Poll check status for every session that has a PR or an armed merge, and let
  // a poll that finds the PR mergeable drive the next auto-merge attempt (so
  // retries are naturally paced to the poll, not a render loop).
  const pollChecks = useCallback(() => {
    const { sessions, prs, prMerge } = pollRef.current;
    for (const s of sessions) {
      if (!prs[s.id] && !prMerge[s.id]?.intent) continue;
      const seq = (pollSeqRef.current[s.id] ?? 0) + 1;
      pollSeqRef.current[s.id] = seq;
      api.sessionPrChecks(s.id)
        .then((c) => {
          // Drop a stale response a newer poll has already superseded.
          if (pollSeqRef.current[s.id] !== seq) return;
          setPrChecks((prev) => ({ ...prev, [s.id]: c }));
          const m = pollRef.current.prMerge[s.id];
          if (!c || !m?.intent || m.merging) return;
          if (c.ready_to_merge) {
            void runMerge(s.id);
            return;
          }
          // Stop waiting on a blocker the user must clear; otherwise keep polling.
          const blocker = mergeBlocker(c);
          if (blocker) setPrMerge((prev) => ({ ...prev, [s.id]: { error: blocker } }));
        })
        .catch(() => {});
    }
  }, [runMerge]);

  // Run the poll on an interval, but only while the popover is open.
  useEffect(() => {
    if (!popoverOpen) return;
    pollChecks();
    const id = setInterval(pollChecks, 6000);
    return () => clearInterval(id);
  }, [popoverOpen, pollChecks]);

  // Run teardown and route its outcome to the right prompt: a warnings
  // confirmation, a "VS Code still open" block, or success (dismiss + refresh).
  // `confirmed` skips the work-state checks; `force` skips closing the editor.
  async function runTeardown(id: string, confirmed: boolean, force: boolean) {
    setTeardownBusy((prev) => ({ ...prev, [id]: true }));
    try {
      const res = await api.teardown(id, confirmed, force);
      if (res.status === "needs_confirmation") {
        setTeardownConfirm({ id, kind: "confirm", warnings: res.warnings });
      } else if (res.status === "blocked_by_editor") {
        setTeardownConfirm({ id, kind: "blocked", message: res.message, accessibility: res.accessibility });
      } else {
        setTeardownConfirm(null);
        setCommandsOpen((open) => (open === id ? null : open));
        refreshSessions();
      }
    } catch (e) {
      setTeardownConfirm({ id, kind: "confirm", warnings: [`Teardown failed: ${String(e)}`] });
    } finally {
      setTeardownBusy((prev) => {
        const { [id]: _done, ...rest } = prev;
        return rest;
      });
    }
  }

  // "Tear Down": remove the worktree. Confirms first unless the backend says
  // it's safe (PR merged, nothing new).
  function tearDown(s: Session) {
    setTeardownConfirm(null);
    runTeardown(s.id, false, false);
  }

  // Untrack a repo (delete its settings file). Worktrees and session records
  // stay on disk; the repo and its work items just drop out of the dashboard.
  async function removeRepo(repo: string) {
    setRemoveConfirm(null);
    setRemoveErr((e) => { const { [repo]: _, ...rest } = e; return rest; });
    try {
      await api.removeRepo(repo);
      setRepos((prev) => prev.filter((r) => r !== repo));
    } catch (err) {
      setRemoveErr((e) => ({ ...e, [repo]: String(err) }));
    }
    refreshAll();
  }

  // Set or clear (hidden = null → unhide) hide/snooze state for a repo or work
  // item, then refresh so the dimming/filtering reflects the new state.
  async function applyVisibility(target: HideTarget, hidden: HideState | null) {
    try {
      if (target.kind === "repo") {
        await api.setRepoVisibility(target.repo, hidden);
      } else {
        await api.setSessionVisibility(target.session.id, hidden);
      }
    } catch { /* best-effort; the refresh below reflects the real state */ }
    setHideTarget(null);
    setRepoMenuOpen(null);
    setCommandsOpen(null);
    refreshAll();
  }

  // Launch VS Code / Finder for a session, surfacing a failure inline (the
  // launcher CLI can be missing — the health check tests for exactly this).
  const clearSessionOpenErr = (id: string) =>
    setOpenSessionErr((m) => { const { [id]: _drop, ...rest } = m; return rest; });
  function openSessionInEditor(id: string, repo: string) {
    clearSessionOpenErr(id); // clear a stale error before retrying, like the repo-level button
    withCodexHooksNotice(repo, id, () => {
      api.openInEditor(id)
        .then((res) => {
          // A pending agent switch: the open window still runs the old agent.
          if (res.status === "restart_required") {
            setAgentPrompt({ id, kind: "restart", reason: "open", agent: res.agent, editorAgent: res.editor_agent });
          }
        })
        .catch((e) => setOpenSessionErr((m) => ({ ...m, [id]: String(e) })));
    });
  }

  // Bring the session's window to the front as-is, skipping the pending-switch
  // check — from the "couldn't restart" prompt, so the user can close it.
  // The "couldn't restart" prompt is dismissed: the user is handling the window.
  function focusSessionEditor(id: string) {
    clearSessionOpenErr(id);
    setAgentPrompt((p) => (p?.id === id ? null : p));
    api.openInEditor(id, true).catch((e) => setOpenSessionErr((m) => ({ ...m, [id]: String(e) })));
  }

  const setAgentBusyFor = (id: string, busy: boolean) =>
    setAgentBusy((prev) => {
      const { [id]: _drop, ...rest } = prev;
      return busy ? { ...rest, [id]: true } : rest;
    });

  // Switch a session's agent (issue #186). The backend rewrites the worktree's
  // launch files and hooks; if its VS Code window is still open (running the old
  // agent), offer to restart it.
  async function switchAgent(s: Session, agent: Agent) {
    setAgentPrompt(null);
    setAgentBusyFor(s.id, true);
    try {
      const res = await api.setSessionAgent(s.id, agent);
      if (res.restart_needed && res.editor_agent) {
        setAgentPrompt({ id: s.id, kind: "restart", reason: "switched", agent, editorAgent: res.editor_agent });
      }
    } catch (e) {
      setAgentPrompt({ id: s.id, kind: "error", message: String(e) });
    } finally {
      setAgentBusyFor(s.id, false);
      refreshSessions();
    }
  }

  // Close and reopen the session's window so it launches the recorded agent. A
  // Codex restart first explains the one-time hook trust, like any Codex open.
  function restartEditor(s: Session) {
    withCodexHooksNotice(s.repo, s.id, async () => {
      setAgentBusyFor(s.id, true);
      try {
        const res = await api.restartSessionEditor(s.id);
        setAgentPrompt(
          res.status === "blocked_by_editor"
            ? { id: s.id, kind: "blocked", message: res.message, accessibility: res.accessibility }
            : null,
        );
      } catch (e) {
        setAgentPrompt({ id: s.id, kind: "error", message: String(e) });
      } finally {
        setAgentBusyFor(s.id, false);
        refreshSessions();
      }
    });
  }
  function revealSessionPath(id: string, dir: string) {
    clearSessionOpenErr(id);
    api.openPath(dir).catch((e) => setOpenSessionErr((m) => ({ ...m, [id]: String(e) })));
  }

  const now = Date.now();

  // The lifecycle zone a session sorts into. An unresolved work state defaults to
  // Planning until the local-git lookup lands (then the item slides if it moved).
  const zoneOf = (s: Session): LifecyclePhase =>
    lifecyclePhase(prs[s.id], workStates[s.id]) ?? "planning";

  return (
    <main className="panel">
      <ResizeGrips />
      <header className="panel-header">
        <LogoIcon className="panel-logo" aria-hidden="true" />
        <h1>m<span className="ai">AI</span>estro Code</h1>
        <button
          className={`icon-btn panel-header-center ${showHidden ? "icon-btn--active" : ""}`}
          onClick={() => setShowHidden((v) => !v)}
          title={showHidden ? "Hide hidden items" : "Show hidden items"}
          aria-label="Toggle hidden items"
          aria-pressed={showHidden}
        >
          <EyeIcon />
        </button>
        <button className="icon-btn" onClick={openSettings} title="Settings" aria-label="Settings">
          <GearIcon />
        </button>
      </header>

      {settingsProblem && <SettingsProblemBanner problem={settingsProblem} />}
      {update && <UpdateBanner update={update} onDismiss={dismissUpdate} />}

      <div className="work-list">
        {repos.length === 0 ? (
          <div className="empty-state">
            <p className="empty-state-title">No repos yet</p>
            <p className="empty-state-body">Add a repo in Settings → Repo, then start work on its issues.</p>
          </div>
        ) : (
          repos.map((repo) => {
            const repoSessions = sessionsByRepo[repo] ?? [];
            const repoHide = repoSettings[repo]?.hidden ?? null;
            const repoHidden = effectiveHidden(repoHide, now);
            // With "Show hidden" off, a hidden repo drops out entirely (its work
            // items go with it). With it on, the repo and its items render dimmed.
            if (repoHidden && !showHidden) return null;
            const repoSnoozeLabel = repoHide?.snooze_until
              ? formatSnoozeRemaining(repoHide.snooze_until, now)
              : "";
            const repoMenu = repoMenuOpen === repo;
            const visibleSessions = showHidden
              ? repoSessions
              : repoSessions.filter((s) => !effectiveHidden(s.hidden, now));
            return (
              <div key={repo} className={`repo-group ${repoHidden ? "repo-group--hidden" : ""}`}>
                <div className="repo-group-header">
                  <span className="repo-group-name">{repo}</span>
                  {repoHidden && (
                    <SnoozeLabel
                      snoozeLabel={repoSnoozeLabel}
                      expanded={repoMenu}
                      onClick={() => setRepoMenuOpen((r) => (r === repo ? null : repo))}
                    />
                  )}
                  <button
                    className="pill-btn repo-open-editor"
                    onClick={async () => {
                      setOpenRepoErr((e) => { const { [repo]: _, ...rest } = e; return rest; });
                      try {
                        await api.openRepoInEditor(repo);
                      } catch (err) {
                        setOpenRepoErr((e) => ({ ...e, [repo]: String(err) }));
                      }
                    }}
                    title="Open cloned repo in VS Code"
                    aria-label="Open cloned repo in VS Code"
                  >
                    <VSCodeIcon />
                  </button>
                  <button className="btn-add" onClick={() => openStartWork(repo)}>
                    Start Work
                  </button>
                  <button
                    className={`row-expander ${repoMenu ? "row-expander--open" : ""}`}
                    onClick={() => setRepoMenuOpen((r) => (r === repo ? null : repo))}
                    title="Repo options"
                    aria-label="Repo options"
                    aria-expanded={repoMenu}
                  >
                    <ChevronRightIcon />
                  </button>
                </div>
                <div className={`command-strip ${repoMenu ? "command-strip--open" : ""}`}>
                  <HideCommandButton
                    hidden={repoHidden}
                    onHide={() => { setRepoMenuOpen(null); setHideTarget({ kind: "repo", repo }); }}
                    onUnhide={() => applyVisibility({ kind: "repo", repo }, null)}
                  />
                  <button className="command-btn" onClick={() => { setRepoMenuOpen(null); setRemoveConfirm(repo); }}>
                    Remove…
                  </button>
                </div>
                {removeConfirm === repo && (
                  <RemoveConfirm
                    name={repo}
                    body="Worktrees, cloned repos, and any work in them are kept on disk."
                    onRemove={() => removeRepo(repo)}
                    onCancel={() => setRemoveConfirm(null)}
                  />
                )}
                {removeErr[repo] && (
                  <DismissibleError
                    lead="Couldn't remove repo"
                    message={removeErr[repo]}
                    onDismiss={() => setRemoveErr((e) => { const { [repo]: _, ...rest } = e; return rest; })}
                  />
                )}
                {openRepoErr[repo] && (
                  <DismissibleError
                    lead="Couldn't open in VS Code"
                    message={openRepoErr[repo]}
                    onDismiss={() => setOpenRepoErr((e) => { const { [repo]: _, ...rest } = e; return rest; })}
                  />
                )}

                {visibleSessions.length === 0 ? (
                  <p className="repo-group-empty">No active work</p>
                ) : (
                  ZONES.map((zone) => {
                    const zoneItems = visibleSessions.filter((s) => zoneOf(s) === zone);
                    if (zoneItems.length === 0) return null;
                    return (
                      <div key={zone} className={`lifecycle-zone lifecycle-zone--${zone}`}>
                        <div className="lifecycle-zone-header">
                          <span className="lifecycle-zone-dot" />
                          <span className="lifecycle-zone-name">{LIFECYCLE_LABELS[zone]}</span>
                        </div>
                        {zoneItems.map((s) => (
                          <SessionRow
                            key={s.id}
                            session={s}
                            status={statuses[s.id]}
                            pr={prs[s.id]}
                            checks={prChecks[s.id]}
                            prCreate={prCreate[s.id]}
                            prMerge={prMerge[s.id]}
                            teardownBusy={!!teardownBusy[s.id]}
                            teardownConfirm={teardownConfirm}
                            agentPrompt={agentPrompt}
                            agentBusy={!!agentBusy[s.id]}
                            cmdOpen={commandsOpen === s.id}
                            repoHidden={repoHidden}
                            now={now}
                            openErr={openSessionErr[s.id]}
                            busyCls={busyCls}
                            busyRingCls={busyRingCls}
                            onToggleCommands={() => setCommandsOpen((id) => (id === s.id ? null : s.id))}
                            onOpenInEditor={() => openSessionInEditor(s.id, s.repo)}
                            onOpenPath={() => revealSessionPath(s.id, s.work_dir)}
                            onOpenUrl={(url) => api.openUrl(url)}
                            onOpenAccessibilitySettings={() => api.openAccessibilitySettings()}
                            onCreatePr={() => { setCommandsOpen(null); createPr(s); }}
                            onStartMerge={() => { setCommandsOpen(null); startMerge(s); }}
                            onTearDown={() => { setCommandsOpen(null); tearDown(s); }}
                            onChooseAgent={() => { setCommandsOpen(null); setAgentTarget(s); }}
                            onFocusEditor={() => focusSessionEditor(s.id)}
                            onRestartEditor={() => restartEditor(s)}
                            onDismissAgentPrompt={() => setAgentPrompt(null)}
                            onRunTeardown={(confirmed, force) => runTeardown(s.id, confirmed, force)}
                            onHide={() => { setCommandsOpen(null); setHideTarget({ kind: "session", session: s }); }}
                            onUnhide={() => applyVisibility({ kind: "session", session: s }, null)}
                            onCancelTeardownConfirm={() => setTeardownConfirm(null)}
                            onDismissPrCreateError={() => setPrCreate((prev) => ({ ...prev, [s.id]: {} }))}
                            onDismissPrMergeError={() => setPrMerge((prev) => ({ ...prev, [s.id]: {} }))}
                            onDismissToolError={() => api.clearSessionError(s.id)}
                            onDismissOpenError={() => clearSessionOpenErr(s.id)}
                            onDismissNotice={() => { api.dismissSessionNotice(s.id).then(refreshSessions).catch(() => {}); }}
                          />
                        ))}
                      </div>
                    );
                  })
                )}
              </div>
            );
          })
        )}
      </div>

      {picker && (() => {
        // Issues in this repo that already have a session, keyed by issue number,
        // carrying the workspace color and (joined) session title(s).
        const active: ActiveSessions = {};
        for (const s of sessionsByRepo[picker.repo] ?? []) {
          const phase = lifecyclePhase(prs[s.id], workStates[s.id]);
          const existing = active[s.issue_number];
          active[s.issue_number] = existing
            ? { color: existing.color, title: `${existing.title}, ${s.session_title}`, phase: furtherPhase(existing.phase, phase) }
            : { color: s.color, title: s.session_title, phase };
        }
        const filtered = filterIssues(picker.issues ?? [], picker.query);
        // Issues already being worked on sink to the end (stable sort keeps the
        // backend's most-recently-modified order within each group).
        const ordered = [...filtered].sort((a, b) => (active[a.number] ? 1 : 0) - (active[b.number] ? 1 : 0));
        return (
          <PickerOverlay
            picker={picker}
            expanded={expanded}
            active={active}
            ordered={ordered}
            ideaRef={ideaRef}
            busyCls={busyCls}
            onClose={closePicker}
            onQueryChange={(q) => setPicker((p) => (p ? { ...p, query: q } : p))}
            onIdeaFocus={() => setExpanded({ kind: "idea" })}
            onCancelIdea={() => { setExpanded(null); setPicker((p) => (p ? { ...p, query: "" } : p)); }}
            onCreateIssue={() => draftIdea("create", picker.query)}
            onCreateAndSpawn={() => draftIdea("spawn", picker.query)}
            onRefresh={refreshIssues}
            onExpandIssue={(num) => setExpanded({ kind: "issue", number: num })}
            onCollapseIssue={() => setExpanded(null)}
            onSpawnIssue={spawnIssue}
            onConfirmRaw={confirmRaw}
            onDismissConfirm={() => setPicker((p) => (p ? { ...p, confirm: undefined } : p))}
            onPatchPreview={setPreview}
            onConfirmPreview={confirmPreview}
            onCancelPreview={cancelPreview}
          />
        );
      })()}

      {hooksNotice && (
        <CodexHooksDialog
          onContinue={() => { const { proceed } = hooksNotice; setHooksNotice(null); proceed(); }}
          onClose={() => setHooksNotice(null)}
        />
      )}

      {agentTarget && (
        <AgentSwitchDialog
          title={agentTarget.session_title}
          current={agentTarget.agent ?? "claude"}
          onConfirm={(agent) => { const s = agentTarget; setAgentTarget(null); switchAgent(s, agent); }}
          onClose={() => setAgentTarget(null)}
        />
      )}
      {hideTarget && (
        <HideSnoozeDialog
          title={hideTarget.kind === "repo" ? hideTarget.repo : hideTarget.session.session_title}
          onConfirm={(hidden) => applyVisibility(hideTarget, hidden)}
          onClose={() => setHideTarget(null)}
        />
      )}
    </main>
  );
}

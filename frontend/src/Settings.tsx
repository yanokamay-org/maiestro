import { useState, useEffect, useCallback, useMemo } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Agent, api, AppSettings, AppVersion, CredentialScope, CredentialTypeDto, GHRepo, HealthCheck, RepoSettings, ResolvedTool } from "./api";
import { JsonForms } from "@jsonforms/react";
import {
  repoSettingsRenderers,
  repoSettingsCells,
  repoSettingsUISchema,
  sanitizeSchemaForForm,
  extractFormDefaults,
  RepoFormDefaults,
} from "./RepoSettingsForm";
import {
  appSettingsRenderers,
  appSettingsCells,
  appSettingsUISchema,
  extractAppFormDefaults,
  AppFormDefaults,
} from "./AppSettingsForm";
import { applyTheme } from "./theme";
import ChevronRightIcon from "./icons/chevron-right.svg?react";
import { CredRows, CredState } from "./components/CredRows";
import { DetailErrorBoundary } from "./components/DetailErrorBoundary";
import { AboutSection } from "./components/AboutSection";
import { HealthModal, HealthState } from "./components/HealthModal";
import { DismissibleError } from "./components/DismissibleError";
import { RemoveConfirm } from "./components/RemoveConfirm";
import { useHideOnClose, useTauriListen } from "./hooks/useTauriListen";
import { useDebouncedAutosave } from "./hooks/useDebouncedAutosave";
import { PathProbeGenerationProvider } from "./PathField";

type SettingsSelection =
  | { kind: "identity"; id: string }
  | { kind: "repo"; repo: string }
  | { kind: "repo-add" }
  | { kind: "preferences" }
  | null;

// A hand-typed repo in the Add Repo picker, e.g. "octocat/Hello-World". Matches
// the characters GitHub allows in an owner or repo name (#124).
const OWNER_NAME_RE = /^[A-Za-z0-9._-]+\/[A-Za-z0-9._-]+$/;

// The "couldn't load ~/.maiestro/*.json" banner shown instead of a form full of
// defaults (which would clobber the file on the next save). `hint` names how to
// recover for the specific file.
function SettingsLoadError({ lead, message, hint }: { lead: string; message: string; hint: string }) {
  return (
    <div className="cleanup-confirm">
      <p className="cleanup-confirm-body">{lead}</p>
      <p className="cleanup-confirm-body" style={{ opacity: 0.85, fontFamily: "var(--font-mono, monospace)", fontSize: 11 }}>
        {message}
      </p>
      <p className="cleanup-confirm-body" style={{ opacity: 0.7 }}>{hint}</p>
    </div>
  );
}

export function Settings() {
  const [selection, setSelection] = useState<SettingsSelection>(null);
  const [identitiesOpen, setIdentitiesOpen] = useState(true);
  const [reposOpen, setReposOpen] = useState(true);
  const [addingIdentityInline, setAddingIdentityInline] = useState(false);
  const [identityInputInline, setIdentityInputInline] = useState("");
  const [knownIdentities, setKnownIdentities] = useState<string[]>([]);
  const [repos, setRepos] = useState<string[]>([]);
  const [credTypes, setCredTypes] = useState<CredentialTypeDto[]>([]);
  const [credStates, setCredStates] = useState<Record<string, CredState>>({});
  // Settings for the selected repo, tagged with the repo they belong to. null
  // until repo_settings_get resolves — the form must not render before then,
  // or JsonForms' initial onChange autosaves placeholder data over the real
  // file (#65).
  const [loadedRepo, setLoadedRepo] = useState<{ repo: string; settings: RepoSettings } | null>(null);
  // The hand-written JSON Schema, fetched from the backend, that drives the
  // repo-detail form. null until loaded.
  const [repoSchema, setRepoSchema] = useState<Record<string, unknown> | null>(null);
  // Default values (worktree prefix, AI prompts) read from the schema's
  // `default` keywords — shown by the custom renderers.
  const [repoFormDefaults, setRepoFormDefaults] = useState<RepoFormDefaults | null>(null);
  // Set when the backend rejects a repo's settings file on load (bad JSON or a
  // schema violation); shown as a banner instead of a form full of defaults.
  const [repoLoadError, setRepoLoadError] = useState<string | null>(null);
  // Set when an autosave write fails.
  const [repoSaveError, setRepoSaveError] = useState<string | null>(null);
  // Whether the Remove Identity confirm is showing for the selected identity.
  const [identityRemoveConfirm, setIdentityRemoveConfirm] = useState(false);
  // Set when removing the selected identity fails.
  const [identityRemoveError, setIdentityRemoveError] = useState<string | null>(null);
  // Whether the Remove Repo confirm is showing for the selected repo.
  const [repoRemoveConfirm, setRepoRemoveConfirm] = useState(false);
  // Set when removing the selected repo fails.
  const [repoRemoveError, setRepoRemoveError] = useState<string | null>(null);
  // Repo health-check modal (#93). Checks stream in one at a time via the
  // `health-check` event; `total` (known once the first event lands) lets the
  // modal stop the spinner. `loading` stays true until the command resolves.
  const [health, setHealth] = useState<HealthState | null>(null);
  // Bumped when something outside the forms' own data may have fixed the
  // filesystem/tool resolution underneath a path hint (#147) — currently, the
  // health modal closing. See `refreshProbes` and `PathProbeGenerationContext`.
  const [probeGeneration, setProbeGeneration] = useState(0);
  const [browse, setBrowse] = useState<{
    identityId: string | null;
    repos: GHRepo[] | null;
    filter: string;
    loading: boolean;
    // True while a typed "owner/name" is being validated against GitHub (#124).
    adding: boolean;
    error?: string;
  }>({ identityId: null, repos: null, filter: "", loading: false, adding: false });
  // Global app settings ("Preferences" panel), rendered via JSON Forms (#85).
  const [appSchema, setAppSchema] = useState<Record<string, unknown> | null>(null);
  const [appSettings, setAppSettings] = useState<AppSettings | null>(null);
  const [appLoadError, setAppLoadError] = useState<string | null>(null);
  const [appSaveError, setAppSaveError] = useState<string | null>(null);
  // How each directly-invoked CLI currently resolves, for the Tool paths status line.
  const [resolvedTools, setResolvedTools] = useState<ResolvedTool[]>([]);
  const [appFormDefaults, setAppFormDefaults] = useState<AppFormDefaults>({ terminalFontDefault: "", agentDefault: "claude" });
  // Version + build metadata for the About block under the Preferences form
  // (#126). Read-only, so a failed fetch just hides the block.
  const [appVersion, setAppVersion] = useState<AppVersion | null>(null);

  // Debounced autosave for the two JsonForms panels. The repo form saves to the
  // repo carried in its own data (the hidden `repo` field round-trips); the app
  // form applies the theme immediately, refreshes tool resolution after a save,
  // and rolls back its baseline on failure so a fixed value can save again.
  const repoAutosave = useDebouncedAutosave<RepoSettings>({
    save: (d) => api.setRepoSettings(d.repo, d),
    onError: setRepoSaveError,
  });
  const appAutosave = useDebouncedAutosave<AppSettings>({
    save: (d) => api.setAppSettings(d),
    onError: setAppSaveError,
    rollbackOnError: true,
    onChange: (d) => applyTheme(d.theme ?? "system"),
    onSaved: () => { api.toolsResolved().then(setResolvedTools).catch(() => {}); },
  });

  useEffect(() => {
    api.listCredentialTypes().then((types) => {
      setCredTypes(types);
      const init: Record<string, CredState> = {};
      for (const t of types) init[t.type_id] = { isSet: false, input: "", status: "idle" };
      setCredStates(init);
    });
    api.listRepos().then(setRepos);
    api.identitiesList().then(setKnownIdentities);
    api.getDefaultIdentity().then((id) => { if (id) setSelection({ kind: "identity", id }); });
    api.repoSettingsSchema().then((s) => {
      setRepoFormDefaults(extractFormDefaults(s));
      setRepoSchema(sanitizeSchemaForForm(s));
    });
    api.appSettingsSchema().then((s) => {
      setAppFormDefaults(extractAppFormDefaults(s));
      setAppSchema(sanitizeSchemaForForm(s));
    });
    loadAppSettings();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // Load the global settings + current tool resolution for the Preferences form.
  function loadAppSettings() {
    api.getAppSettings()
      .then((s) => {
        appAutosave.seed(s);
        setAppSettings(s);
        setAppLoadError(null);
      })
      .catch((e) => setAppLoadError(String(e)));
    api.toolsResolved().then(setResolvedTools).catch(() => setResolvedTools([]));
    api.appVersion().then(setAppVersion).catch(() => setAppVersion(null));
  }

  // Autosave the Preferences form, debounced, skipped while ajv reports errors.
  function handleAppFormChange(data: AppSettings, errors: unknown[] | undefined) {
    setAppSettings(data);
    if ((errors?.length ?? 0) > 0) return;
    appAutosave.schedule(data);
  }

  useHideOnClose();

  // This webview is created at app startup and only hidden (never destroyed) on
  // close, so what it fetched on mount can be stale by the time the window is
  // actually opened — most visibly right after first-run onboarding, which
  // writes `launch_at_login` seconds after this mounted, leaving the Preferences
  // toggle showing the old value (#139). The same staleness hid a freshly
  // created onboarding identity from the Identities list (#148): this mounted
  // and fetched an empty list before onboarding registered one. The backend
  // announces each fresh open; re-read then, exactly as the popover refreshes
  // on `popover-shown`. `loadAppSettings` re-seeds the autosave baseline, so
  // the reload can't save itself back over the file.
  useTauriListen("settings-shown", () => {
    loadAppSettings();
    api.identitiesList().then(setKnownIdentities);
  });

  const patchCred = useCallback((type_id: string, patch: Partial<CredState>) => {
    setCredStates((prev) => ({ ...prev, [type_id]: { ...prev[type_id], ...patch } }));
  }, []);

  const activeScope = useMemo((): CredentialScope | null => {
    if (selection?.kind === "identity")
      return { kind: "identity", identity_id: selection.id };
    return null;
  }, [selection]);

  const scopeKey = activeScope ? `identity:${activeScope.identity_id}` : null;

  // Selecting a different identity dismisses any pending remove confirm/error.
  useEffect(() => {
    setIdentityRemoveConfirm(false);
    setIdentityRemoveError(null);
  }, [scopeKey]);

  useEffect(() => {
    setCredStates((prev) => {
      const next = { ...prev };
      for (const id of Object.keys(next)) next[id] = { ...next[id], isSet: false };
      return next;
    });
    if (!activeScope || credTypes.length === 0) return;
    const scope = activeScope;
    const timer = setTimeout(async () => {
      await Promise.all(
        credTypes.map(async (t) => {
          try {
            const isSet = await api.credentialExists(t.type_id, scope);
            patchCred(t.type_id, { isSet });
          } catch {
            patchCred(t.type_id, { isSet: false });
          }
        }),
      );
    }, 300);
    return () => clearTimeout(timer);
    // `credTypes` must be a dep: on mount it races the default-identity fetch,
    // and if it lands after scopeKey settles the badges would stay "not set".
  }, [scopeKey, credTypes]); // eslint-disable-line react-hooks/exhaustive-deps

  async function handleSave(type_id: string) {
    if (!activeScope) return;
    const state = credStates[type_id];
    if (!state?.input.trim()) return;
    patchCred(type_id, { status: "saving" });
    try {
      await api.setCredential(type_id, activeScope, state.input.trim());
      patchCred(type_id, { status: "saved", isSet: true, input: "" });
      api.identitiesList().then(setKnownIdentities);
      setTimeout(() => patchCred(type_id, { status: "idle" }), 2000);
    } catch (e) {
      patchCred(type_id, { status: "error", error: String(e) });
    }
  }

  const selectedRepo = selection?.kind === "repo" ? selection.repo : null;

  // Identifies the current detail selection, used to key the error boundary so
  // navigating to a different item clears any prior render error.
  const selectionKey =
    selection == null ? "none"
      : selection.kind === "repo" ? `repo:${selection.repo}`
      : selection.kind === "identity" ? `identity:${selection.id}`
      : selection.kind;

  useEffect(() => {
    if (!selectedRepo) return;
    setLoadedRepo(null);
    setRepoLoadError(null);
    setRepoSaveError(null);
    setRepoRemoveConfirm(false);
    setRepoRemoveError(null);
    api.getRepoSettings(selectedRepo)
      .then((s) => {
        repoAutosave.seed(s);
        setLoadedRepo({ repo: selectedRepo, settings: s });
      })
      .catch((e) => setRepoLoadError(String(e)));
  }, [selectedRepo]); // eslint-disable-line react-hooks/exhaustive-deps

  // Extra data the custom renderers (identity select, env-files Scan) read via
  // JsonForms' `config`. Memoized so the form isn't needlessly re-keyed.
  // The global agent a repo with no `agent` of its own falls back to; the repo
  // form names it and picks which drafting-model entry to edit from it.
  const globalAgent: Agent = appSettings?.agent ?? appFormDefaults.agentDefault;
  const repoAgent: Agent = loadedRepo?.settings.agent ?? globalAgent;
  const repoFormConfig = useMemo(
    () => ({
      showUnfocusedDescription: true as const,
      knownIdentities,
      clonedRepoDir: loadedRepo?.settings.cloned_repo_dir ?? null,
      worktreePrefixDefault: repoFormDefaults?.worktreePrefixDefault ?? "",
      promptModelDefaults: repoFormDefaults?.promptModelDefaults ?? { claude: "", codex: "" },
      globalAgent,
      repoAgent,
      booleanDefaults: repoFormDefaults?.booleanDefaults ?? {},
      promptDefaults: repoFormDefaults?.promptDefaults ?? {},
    }),
    [knownIdentities, loadedRepo?.settings.cloned_repo_dir, repoFormDefaults, globalAgent, repoAgent],
  );

  // Config the app-settings custom renderers read (the Tool paths status line,
  // the Terminal font placeholder).
  const appFormConfig = useMemo(
    () => ({ showUnfocusedDescription: true as const, resolvedTools, ...appFormDefaults }),
    [resolvedTools, appFormDefaults],
  );

  // Autosave on change, debounced, skipped while ajv reports errors. JsonForms
  // preserves fields not in the UI schema (repo, hidden), so they round-trip.
  function handleRepoFormChange(repo: string, data: RepoSettings, errors: unknown[] | undefined) {
    // Only the currently-loaded repo may save — a stale render mid-switch must
    // not write its data under another repo's key (#65).
    if (loadedRepo?.repo !== repo) return;
    setLoadedRepo({ repo, settings: data });
    if ((errors?.length ?? 0) > 0) return;
    repoAutosave.schedule(data);
  }

  async function commitIdentityInline() {
    const trimmed = identityInputInline.trim();
    if (!trimmed) return;
    setAddingIdentityInline(false);
    setIdentityInputInline("");
    try {
      await api.identitiesAdd(trimmed);
      const list = await api.identitiesList();
      setKnownIdentities(list);
      setSelection({ kind: "identity", id: trimmed });
    } catch { /* best-effort */ }
  }

  // Remove an identity: Keychain credentials, list entry, and repo references
  // all go; worktrees and sessions are untouched.
  async function handleRemoveIdentity(id: string) {
    setIdentityRemoveConfirm(false);
    try {
      await api.identitiesRemove(id);
      setKnownIdentities((prev) => prev.filter((i) => i !== id));
      setSelection(null);
    } catch (e) {
      setIdentityRemoveError(String(e));
    }
  }

  async function handleClear(type_id: string) {
    if (!activeScope) return;
    patchCred(type_id, { status: "clearing" });
    try {
      await api.deleteCredential(type_id, activeScope);
    } catch { /* already gone */ }
    patchCred(type_id, { status: "idle", isSet: false, input: "" });
  }

  // Picking an identity immediately loads its repos — there is no Fetch button.
  // Switching identities is therefore cheap, so a response that arrives after
  // the user has moved on is discarded rather than replacing the newer list.
  async function handleSelectBrowseIdentity(iid: string) {
    if (!iid) return;
    setBrowse((prev) => ({ ...prev, identityId: iid, repos: null, loading: true, error: undefined }));
    try {
      const fetched = await api.githubListRepos(iid);
      setBrowse((prev) => (prev.identityId === iid ? { ...prev, loading: false, repos: fetched } : prev));
      api.identitiesList().then(setKnownIdentities);
    } catch (e) {
      setBrowse((prev) => (prev.identityId === iid ? { ...prev, loading: false, error: String(e) } : prev));
    }
  }

  // Entering the Add Repo view: pick the default identity (or the only/first
  // one) and start loading its repos right away, so the picker lands ready to
  // use. The identity stays switchable from the select at the top.
  async function openAddRepo() {
    setBrowse({ identityId: null, repos: null, filter: "", loading: true, adding: false });
    setReposOpen(true);
    setSelection({ kind: "repo-add" });
    try {
      const [list, preferred] = await Promise.all([api.identitiesList(), api.getDefaultIdentity()]);
      setKnownIdentities(list);
      const iid = preferred && list.includes(preferred) ? preferred : list[0];
      if (!iid) {
        setBrowse((prev) => ({ ...prev, loading: false })); // no identities yet
        return;
      }
      await handleSelectBrowseIdentity(iid);
    } catch (e) {
      setBrowse((prev) => ({ ...prev, loading: false, error: String(e) }));
    }
  }

  // Untrack the selected repo (delete its settings file). Cancels any pending
  // debounced autosave first so it can't recreate the file after the delete.
  async function handleRemoveRepo(repo: string) {
    setRepoRemoveConfirm(false);
    repoAutosave.cancel();
    try {
      await api.removeRepo(repo);
      setRepos((prev) => prev.filter((r) => r !== repo));
      setSelection(null);
    } catch (e) {
      setRepoRemoveError(String(e));
    }
  }

  // Re-probe the advisory, environment-derived hints in both forms — path
  // existence (cloned repo dir, worktree prefix, env files, tool overrides)
  // and the Tool paths status line — without touching settings *data* (#147).
  // Deliberately not `loadAppSettings()`: that re-reads settings.json and
  // re-seeds the autosave baseline, which would be wrong mid-edit. Called when
  // the health modal closes, since its remediation commands are what the user
  // may have just acted on.
  function refreshProbes() {
    setProbeGeneration((g) => g + 1);
    api.toolsResolved().then(setResolvedTools).catch(() => {});
  }

  // Run the repo health check and stream results into the modal (#93). Each
  // check arrives as a `health-check` event; the command's return value is the
  // authoritative final list (reconciles any missed event).
  async function runHealthCheck(repo: string) {
    setHealth({ repo, checks: [], total: null, running: null, loading: true, error: null });
    const win = getCurrentWindow();
    // A check announces itself (running) before it executes, then reports its
    // result — subscribe to both. Both are scoped to `repo`.
    const unlistenRunning = await win.listen<{ repo: string; total: number; label: string }>(
      "health-check-running",
      (e) => {
        if (e.payload.repo !== repo) return;
        setHealth((h) =>
          h && h.repo === repo ? { ...h, running: e.payload.label, total: e.payload.total } : h
        );
      }
    );
    const unlistenDone = await win.listen<{ repo: string; total: number; check: HealthCheck }>(
      "health-check",
      (e) => {
        if (e.payload.repo !== repo) return;
        setHealth((h) =>
          h && h.repo === repo
            ? { ...h, checks: [...h.checks, e.payload.check], total: e.payload.total }
            : h
        );
      }
    );
    try {
      const report = await api.repoHealthCheck(repo);
      setHealth((h) =>
        h && h.repo === repo
          ? { ...h, checks: report.checks, total: report.checks.length, running: null, loading: false }
          : h
      );
    } catch (e) {
      setHealth((h) =>
        h && h.repo === repo ? { ...h, running: null, loading: false, error: String(e) } : h
      );
    } finally {
      unlistenRunning();
      unlistenDone();
    }
  }

  // Track a repo typed as "owner/name" that the fetched list doesn't contain —
  // typically one the identity has no affiliation with, so `/user/repos` never
  // returns it (#124). Validated against GitHub first, so a typo fails here
  // instead of becoming a broken dashboard row.
  async function handleAddTypedRepo(typed: string) {
    const iid = browse.identityId;
    if (!iid) return;
    setBrowse((prev) => ({ ...prev, adding: true, error: undefined }));
    try {
      const found = await api.githubGetRepo(iid, typed);
      setBrowse((prev) => ({ ...prev, adding: false }));
      // GitHub's canonical `full_name` wins over what was typed — it fixes case.
      await handleSelectRepo(found.full_name);
    } catch (e) {
      setBrowse((prev) => ({ ...prev, adding: false, error: String(e) }));
    }
  }

  async function handleSelectRepo(repo: string) {
    const iid = browse.identityId;
    setRepos((prev) => (prev.includes(repo) ? prev : [...prev, repo]));
    // Persist the identity BEFORE selecting: selection triggers the detail-form
    // load, and a load that races an in-flight write reads the pre-write file —
    // the form then shows no identity and the next autosave clobbers it.
    try {
      const defaults = await api.getRepoSettings(repo);
      await api.setRepoSettings(repo, { ...defaults, identity_id: iid ?? defaults.identity_id });
    } catch (e) {
      setBrowse((b) => ({ ...b, error: `Added ${repo}, but couldn't assign the identity: ${String(e)}` }));
    }
    setSelection({ kind: "repo", repo });
  }

  return (
    <PathProbeGenerationProvider value={probeGeneration}>
    <main className="panel panel--window">
      <div className="settings-layout">
        {/* ── Sidebar ── */}
        <div className="settings-sidebar">
          <div className="settings-tree">

            {/* Identities section */}
            <div className="tree-section">
              <div className="tree-section-header">
                <button
                  className="tree-section-toggle"
                  onClick={() => setIdentitiesOpen((v) => !v)}
                  aria-expanded={identitiesOpen}
                >
                  <ChevronRightIcon className={`tree-chevron ${identitiesOpen ? "tree-chevron--open" : ""}`} />
                  <span className="tree-section-label">Identities</span>
                </button>
                <button
                  className="tree-add-btn"
                  onClick={() => { setIdentitiesOpen(true); setAddingIdentityInline(true); setIdentityInputInline(""); }}
                  title="Add identity"
                  aria-label="Add identity"
                >+</button>
              </div>
              {identitiesOpen && (
                <div className="tree-items">
                  {addingIdentityInline && (
                    <div className="tree-add-row">
                      <input
                        className="text-input tree-add-input"
                        type="text"
                        aria-label="New identity name"
                        placeholder="e.g. default"
                        value={identityInputInline}
                        autoFocus
                        onChange={(e) => setIdentityInputInline(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") commitIdentityInline();
                          if (e.key === "Escape") { setAddingIdentityInline(false); setIdentityInputInline(""); }
                        }}
                        spellCheck={false}
                        autoCapitalize="off"
                        autoCorrect="off"
                      />
                    </div>
                  )}
                  {knownIdentities.length === 0 && !addingIdentityInline && (
                    <p className="tree-empty-hint">No identities yet</p>
                  )}
                  {knownIdentities.map((id) => (
                    <button
                      key={id}
                      className={`tree-item${selection?.kind === "identity" && selection.id === id ? " tree-item--selected" : ""}`}
                      onClick={() => setSelection({ kind: "identity", id })}
                    >
                      {id}
                    </button>
                  ))}
                </div>
              )}
            </div>

            {/* Repos section */}
            <div className="tree-section">
              <div className="tree-section-header">
                <button
                  className="tree-section-toggle"
                  onClick={() => setReposOpen((v) => !v)}
                  aria-expanded={reposOpen}
                >
                  <ChevronRightIcon className={`tree-chevron ${reposOpen ? "tree-chevron--open" : ""}`} />
                  <span className="tree-section-label">Repos</span>
                </button>
                <button
                  className="tree-add-btn"
                  onClick={openAddRepo}
                  title="Add repo"
                  aria-label="Add repo"
                >+</button>
              </div>
              {reposOpen && (
                <div className="tree-items">
                  {repos.length === 0 && (
                    <p className="tree-empty-hint">No repos yet</p>
                  )}
                  {repos.map((repo) => (
                    <button
                      key={repo}
                      className={`tree-item${selection?.kind === "repo" && selection.repo === repo ? " tree-item--selected" : ""}`}
                      onClick={() => setSelection({ kind: "repo", repo })}
                    >
                      {repo.split("/")[1] ?? repo}
                    </button>
                  ))}
                </div>
              )}
            </div>

            {/* Preferences — non-expandable leaf */}
            <div className="tree-section">
              <button
                className={`tree-item tree-item--preferences${selection?.kind === "preferences" ? " tree-item--selected" : ""}`}
                onClick={() => setSelection({ kind: "preferences" })}
              >
                Preferences
              </button>
            </div>

          </div>
        </div>

        {/* ── Detail panel ── */}
        <div className="settings-detail">
          <DetailErrorBoundary key={selectionKey}>
          {!selection ? (
            <div className="cred-list">
              <div className="empty-state">
                <p className="empty-state-body">Select an item on the left.</p>
              </div>
            </div>
          ) : selection.kind === "preferences" ? (
            <div className="cred-list">
              {appLoadError ? (
                // The backend rejected ~/.maiestro/settings.json (bad JSON or a
                // schema violation). Show it rather than a form full of defaults
                // that would clobber the file on the next save.
                <SettingsLoadError
                  lead="Couldn't load settings:"
                  message={appLoadError}
                  hint="Fix ~/.maiestro/settings.json by hand, then reopen Preferences."
                />
              ) : appSchema && appSettings ? (
                <div className="jsf-root">
                  {appSaveError && (
                    <div className="cleanup-confirm">
                      <p className="cleanup-confirm-body">Couldn't save: {appSaveError}</p>
                    </div>
                  )}
                  <JsonForms
                    schema={appSchema}
                    uischema={appSettingsUISchema}
                    data={appSettings}
                    renderers={appSettingsRenderers}
                    cells={appSettingsCells}
                    config={appFormConfig}
                    onChange={({ data, errors }) =>
                      handleAppFormChange(data as AppSettings, errors)
                    }
                  />
                </div>
              ) : (
                <p className="session-hint" style={{ paddingTop: 2 }}>Loading…</p>
              )}
              {appVersion && <AboutSection info={appVersion} />}
            </div>
          ) : selection.kind === "identity" ? (
            <>
              <div className="detail-header">
                <span className="detail-title">{selection.id}</span>
              </div>
              <div className="cred-list">
                <CredRows
                  credTypes={credTypes}
                  credStates={credStates}
                  patchCred={patchCred}
                  onSave={handleSave}
                  onClear={handleClear}
                />
                <div className="settings-group">
                  {identityRemoveError && (
                    <DismissibleError
                      lead="Couldn't remove identity"
                      message={identityRemoveError}
                      onDismiss={() => setIdentityRemoveError(null)}
                    />
                  )}
                  {identityRemoveConfirm ? (
                    <RemoveConfirm
                      name={selection.id}
                      body="Its Keychain credentials are deleted; repos that used it need a new identity."
                      onRemove={() => handleRemoveIdentity(selection.id)}
                      onCancel={() => setIdentityRemoveConfirm(false)}
                    />
                  ) : (
                    <button className="btn-danger" onClick={() => setIdentityRemoveConfirm(true)}>Remove Identity</button>
                  )}
                </div>
              </div>
            </>
          ) : selection.kind === "repo" ? (
            <>
              <div className="detail-header">
                <span className="detail-title">{selection.repo}</span>
                <button
                  className="btn-ghost health-check-btn"
                  disabled={!!health?.loading}
                  onClick={() => runHealthCheck(selection.repo)}
                >
                  Check Health
                </button>
              </div>
              <div className="cred-list">
                {repoLoadError ? (
                  // The backend rejected this repo's settings file (bad JSON or a
                  // schema violation). Show the error rather than a form full of
                  // defaults that would clobber the file on the next save.
                  <SettingsLoadError
                    lead="Couldn't load settings for this repo:"
                    message={repoLoadError}
                    hint="Fix the file by hand, then reselect this repo."
                  />
                ) : repoSchema && loadedRepo?.repo === selection.repo ? (
                  <div className="jsf-root">
                    {repoSaveError && (
                      <div className="cleanup-confirm">
                        <p className="cleanup-confirm-body">Couldn't save: {repoSaveError}</p>
                      </div>
                    )}
                    <JsonForms
                      key={selection.repo}
                      schema={repoSchema}
                      uischema={repoSettingsUISchema}
                      data={loadedRepo.settings}
                      renderers={repoSettingsRenderers}
                      cells={repoSettingsCells}
                      config={repoFormConfig}
                      onChange={({ data, errors }) =>
                        handleRepoFormChange(selection.repo, data as RepoSettings, errors)
                      }
                    />
                  </div>
                ) : (
                  <p className="session-hint" style={{ paddingTop: 2 }}>Loading…</p>
                )}
                {/* Removal works even when the settings file failed to load —
                    deleting the file is the fix for an unparseable one. */}
                <div className="settings-group">
                  {repoRemoveError && (
                    <DismissibleError
                      lead="Couldn't remove repo"
                      message={repoRemoveError}
                      onDismiss={() => setRepoRemoveError(null)}
                    />
                  )}
                  {repoRemoveConfirm ? (
                    <RemoveConfirm
                      name={selection.repo}
                      body="Worktrees, cloned repos, and any work in them are kept on disk."
                      onRemove={() => handleRemoveRepo(selection.repo)}
                      onCancel={() => setRepoRemoveConfirm(false)}
                    />
                  ) : (
                    <button className="btn-danger" onClick={() => setRepoRemoveConfirm(true)}>Remove Repo</button>
                  )}
                </div>
              </div>
            </>
          ) : (
            /* selection.kind === "repo-add": the identity's repos, loaded on
               open, plus manual entry of any "owner/name" (#124). */
            <>
              <div className="detail-header">
                <span className="detail-title">Add Repo</span>
              </div>
              {knownIdentities.length > 0 && (
                <div className="repo-add-controls">
                  <div className="repo-add-field">
                    <label className="field-label" htmlFor="repo-add-identity">
                      Identity
                    </label>
                    <select
                      id="repo-add-identity"
                      className="text-input profile-select"
                      value={browse.identityId ?? ""}
                      onChange={(e) => handleSelectBrowseIdentity(e.target.value)}
                    >
                      {knownIdentities.map((iid) => (
                        <option key={iid} value={iid}>{iid}</option>
                      ))}
                    </select>
                  </div>
                  <div className="repo-add-field">
                    <label className="field-label" htmlFor="repo-add-filter">
                      Repository
                    </label>
                    <input
                      id="repo-add-filter"
                      className="text-input"
                      type="text"
                      placeholder="Filter repos, or type owner/name…"
                      value={browse.filter}
                      autoFocus
                      onChange={(e) => setBrowse((prev) => ({ ...prev, filter: e.target.value }))}
                      spellCheck={false}
                      autoCapitalize="off"
                      autoCorrect="off"
                    />
                  </div>
                </div>
              )}
              <div className="cred-list">
                {knownIdentities.length === 0 && !browse.loading && (
                  <div className="empty-state">
                    <p className="empty-state-title">No identities yet</p>
                    <p className="empty-state-body">
                      Add an identity in the Identities section first, then save a GitHub token for it.
                    </p>
                  </div>
                )}
                {browse.loading && (
                  <div className="empty-state">
                    <p className="empty-state-body">Fetching repos…</p>
                  </div>
                )}
                {browse.error && (
                  <p className="cred-error" style={{ paddingTop: 4 }}>{browse.error}</p>
                )}
                {browse.identityId !== null && (() => {
                  const repos = browse.repos ?? [];
                  const filtered = repos.filter((r) =>
                    !browse.filter || r.full_name.toLowerCase().includes(browse.filter.toLowerCase())
                  );
                  // The filter doubles as manual entry: anything shaped like
                  // "owner/name" and absent from the fetched list can still be
                  // tracked (#124). Offered while the list is still loading too,
                  // so a repo you can already name never waits on the fetch.
                  const typed = browse.filter.trim();
                  const canAddTyped =
                    OWNER_NAME_RE.test(typed) &&
                    !repos.some((r) => r.full_name.toLowerCase() === typed.toLowerCase());
                  return (
                    <>
                      {canAddTyped && (
                        <button
                          className="repo-item"
                          disabled={browse.adding}
                          onClick={() => handleAddTypedRepo(typed)}
                        >
                          <span className="repo-item-name">
                            {browse.adding ? `Adding ${typed}…` : `Add ${typed}…`}
                          </span>
                          <span className="repo-item-chevron">›</span>
                        </button>
                      )}
                      {browse.repos !== null && filtered.length === 0 && !canAddTyped && (
                        <div className="empty-state">
                          <p className="empty-state-body">No repos match your filter.</p>
                        </div>
                      )}
                      {filtered.map((r) => (
                        <button
                          key={r.full_name}
                          className="repo-item"
                          onClick={() => handleSelectRepo(r.full_name)}
                        >
                          <span className="repo-item-name">{r.full_name}</span>
                          {r.private
                            ? <span className="repo-item-private">private</span>
                            : <span className="repo-item-chevron">›</span>
                          }
                        </button>
                      ))}
                    </>
                  );
                })()}
              </div>
            </>
          )}
          </DetailErrorBoundary>
        </div>
      </div>
      {health && (
        <HealthModal
          state={health}
          onRetry={() => runHealthCheck(health.repo)}
          onClose={() => { setHealth(null); refreshProbes(); }}
        />
      )}
    </main>
    </PathProbeGenerationProvider>
  );
}

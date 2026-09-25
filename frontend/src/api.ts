import { invoke } from "@tauri-apps/api/core";

/** Hide/snooze state for a repo or work item. Presence = hidden. */
export interface HideState {
  /** Unix-epoch millis to stay hidden until; null = hidden indefinitely. */
  snooze_until: number | null;
}

/** Per-repo overrides for the AI prompt instructions. Each field null/empty
 *  uses the built-in default. The runtime context is appended automatically. */
export interface PromptOverrides {
  draft_issue: string | null;
  short_label: string | null;
  draft_pr: string | null;
}

/** The coding agent a repo runs (sessions and mAIestro Code's own drafting). */
export type Agent = "claude" | "codex" | "antigravity";

/** The drafting model per agent; null/empty = that entry's schema default
 *  (`haiku` for Claude; Codex's own configured model for Codex; a Gemini Flash
 *  id for Antigravity). */
export interface PromptModels {
  claude: string | null;
  codex: string | null;
  antigravity?: string | null;
}

export interface RepoSettings {
  repo: string;
  cloned_repo_dir: string | null;
  worktree_prefix: string | null;
  env_files: string[];
  post_spawn_commands: string[];
  /** null = use the global `agent` setting. */
  agent: Agent | null;
  prompt_models: PromptModels;
  identity_id: string | null;
  hidden: HideState | null;
  prompts: PromptOverrides;
}

export interface GHRepo {
  full_name: string;
  private: boolean;
  description: string | null;
}

export interface IssueNode {
  number: number;
  title: string;
  html_url: string;
  children: IssueNode[];
}

export interface SpawnResult {
  /** Workspace/session id (= Session.id) of the row this spawn created/reused. */
  session_id: string;
  work_dir: string;
  branch: string;
  issue_url: string;
  reused: boolean;
  warnings: string[];
}

// `create_issue_direct` opens the issue from a reviewed title/body, so the only
// outcome is `created` (tagged for a stable wire shape the caller switches on).
export type CreateIssueOutcome = { status: "created"; number: number; issue_url: string; warnings: string[] };

/** Editable fields shown in the spawn preview before a worktree is created. */
export interface SpawnPlan {
  repo: string;
  /** null on the create-and-spawn path: the issue isn't opened until confirm. */
  issue_number: number | null;
  issue_title: string;
  issue_body: string;
  short_title: string;
  color: string;
  emoji: string;
  /** Local cloned repo dir name, for rendering the worktree path. */
  repo_name: string;
  /** Effective (un-expanded) worktree prefix, for rendering the worktree path. */
  worktree_prefix: string;
}

export type DraftPreviewOutcome =
  | ({ status: "drafted" } & SpawnPlan)
  | { status: "needs_confirmation"; message: string };

/** The reviewed preview sent back on confirm. */
export interface SpawnEdits {
  issue_number: number | null;
  issue_title: string;
  issue_body: string;
  short_title: string;
  color: string;
  emoji: string;
  /** Existing issue only: PATCH the title/body back to GitHub. */
  update_issue: boolean;
}

export type TeardownOutcome =
  | { status: "done" }
  | { status: "needs_confirmation"; warnings: string[] }
  | { status: "blocked_by_editor"; message: string; accessibility: boolean };

/** `session_set_agent`: whether the open window still runs the other agent. */
export interface SetAgentOutcome {
  restart_needed: boolean;
  editor_agent: Agent | null;
}

/** `session_open_in_editor`: opened, or the window must restart to apply a
 *  pending agent switch. */
export type OpenOutcome =
  | { status: "opened" }
  | { status: "restart_required"; agent: Agent; editor_agent: Agent };

/** `session_restart_editor`: same blocked shape as teardown's. */
export type RestartOutcome =
  | { status: "restarted" }
  | { status: "blocked_by_editor"; message: string; accessibility: boolean };

export interface Session {
  id: string;
  repo: string;
  issue_number: number;
  issue_url: string;
  branch: string;
  default_branch: string;
  work_dir: string;
  cloned_repo_dir: string;
  session_title: string;
  color: string;
  emoji: string;
  /** The agent this worktree launches: fixed at spawn, changed only by an
   *  explicit per-session switch (`setSessionAgent`). */
  agent: Agent;
  /** Set while the worktree's VS Code window still runs a different agent than
   *  `agent` (switched while open, not yet restarted). Absent otherwise. */
  editor_agent?: Agent | null;
  hidden: HideState | null;
  /** Something mAIestro Code changed in the worktree that the user should know
   *  about (e.g. an appended `.gitignore` line). Absent once dismissed. */
  notice?: string | null;
}

/** Lifecycle of a pull request, as GitHub reports it. */
export type PrState = "draft" | "open" | "merged" | "closed";

export interface PrLink {
  number: number;
  html_url: string;
  title: string;
  state: PrState;
}

/** The PR pill's merge indicator, derived from GitHub's mergeable_state. */
export type PrCheckState = "passed" | "failed" | "pending" | "none";

export interface PrChecks {
  /** Pill indicator derived from mergeable_state: "passed" (mergeable),
   *  "failed" (conflicts), "pending" (behind/blocked/computing), "none" (draft). */
  state: PrCheckState;
  /** Mergeability still being computed by GitHub — animate the indicator. */
  running: boolean;
  /** GitHub reports the PR mergeable (`mergeable_state == "clean"`). */
  ready_to_merge: boolean;
  /** Raw GitHub mergeable_state: clean / dirty / behind / blocked / unstable /
   *  draft / unknown. Used to stop the auto-merge loop on terminal blockers. */
  mergeable_state: string;
}

/** The live agent session state, from the `maiestro hook` helper. `creating` is
 *  mAIestro Code's own pre-agent state; the rest map from Claude Code / Codex /
 *  Antigravity hook events. */
export type SessionState = "creating" | "running" | "busy" | "needs_you" | "idle" | "ended";

/** Live per-session status, written by the `maiestro hook` helper and watched
 *  by the backend. Pushed to the UI via the `session-status` event and read in
 *  bulk via `sessions_status_list`. */
export interface StatusRecord {
  /** Workspace id (= Session.id). */
  workspace: string;
  state: SessionState;
  session_id?: string;
  cwd?: string;
  /** Short human detail, e.g. "permission: Bash" or a tool name. */
  detail?: string;
  /** The most recent failed tool call, kept until dismissed or a new turn. */
  last_error?: ToolError;
  ts: string;
}

/** A failed tool call from a `PostToolUseFailure` hook (Claude) or an errored
 *  `PostToolUse`/`Stop` (Antigravity, rarely). Logged on every failure but only
 *  shown in the popover once `surfaced` is true (issue #48). Never for Codex,
 *  which has no failed-tool hook. */
export interface ToolError {
  tool?: string;
  message: string;
  ts: string;
  /** Consecutive failures of this same tool. */
  count: number;
  /** Whether to show this error prominently. False = pending/transient (hidden);
   *  true = Claude stopped without recovering, or the tool failed repeatedly. */
  surfaced: boolean;
}

/** Local git facts about a session's worktree, used to derive the work-lifecycle
 *  phase (planning → implementing → merged). Purely local — no GitHub call. */
export interface WorkState {
  /** Commits in origin/<default_branch>..HEAD (local ref; may lag origin). */
  ahead: number;
  /** Worktree has uncommitted changes. */
  dirty: boolean;
}

/** Chosen UI appearance. "system" follows the macOS dark/light setting. */
export type Theme = "light" | "dark" | "system";

/** Explicit paths for the CLIs mAIestro Code invokes directly. Each null/empty =
 *  auto-resolve (login-shell PATH → which → known locations). */
export interface ToolPaths {
  claude?: string | null;
  codex?: string | null;
  agy?: string | null;
  git?: string | null;
  code?: string | null;
}

/** Global, app-wide settings (`~/.maiestro/settings.json`). `window` /
 *  `settings_window` are machine-managed and not edited in the form. */
export interface AppSettings {
  /** Fields are omitted from the wire entirely when unset (serde skips `None`),
   *  so they are optional here, not just nullable. */
  theme?: Theme | null;
  /** Default agent for repos that don't pick one. null = the schema default. */
  agent?: Agent | null;
  tool_paths?: ToolPaths | null;
  /** Font stack for a spawned worktree's VS Code terminal
   *  (`terminal.integrated.fontFamily`). null/empty = the schema default. */
  terminal_font_family?: string | null;
  /** Launch mAIestro Code automatically at login (per-user LaunchAgent). null = false. */
  launch_at_login?: boolean | null;
  window?: { width: number; height: number } | null;
  settings_window?: { width: number; height: number } | null;
  /** Machine-managed: whether onboarding has been completed. Not edited in the form. */
  onboarding_completed?: boolean | null;
  /** Machine-managed: background update-check state (#182). Not edited in the form. */
  update_check?: {
    checked_at?: string | null;
    latest_version?: string | null;
    latest_published_at?: string | null;
    dismissed_version?: string | null;
  } | null;
}

/** Version + build metadata for the running app, shown in Preferences → About (#126). */
export interface AppVersion {
  /** Semver from tauri.conf.json, e.g. "0.2.3". On a dev build, the last
   *  released version this build sits on top of. */
  version: string;
  /** UTC date the binary was built, YYYY-MM-DD. */
  build_date: string;
  /** Short git SHA of the checkout it was built from; null when unavailable. */
  commit: string | null;
  /** GitHub Release page for this version's tag. */
  release_url: string;
  /** True for a local build made after the `version` release, rather than that
   *  release itself — rendered as "X.Y.Z+dev". */
  dev_build: boolean;
}

/** Why ~/.maiestro/settings.json can't be used right now (bad JSON or a schema
 *  violation); null when the file is fine or absent. Drives the popover's
 *  warning banner — while it stands, nothing is written to the file. */
export interface SettingsProblem {
  /** Absolute path of the file, for Reveal. */
  path: string;
  /** The backend's load error, naming the file and the failure. */
  message: string;
}

/** A newer release the popover should offer (#182). */
export interface AvailableUpdate {
  /** Semver of the newer release, e.g. "0.4.0". */
  version: string;
  /** GitHub Release page the banner's Download opens. */
  release_url: string;
  /** When GitHub says it was published, RFC 3339. */
  published_at: string;
}

/** Verdict of the background update check, read on every popover show. */
export interface UpdateStatus {
  /** null when up to date, never checked, inside the 5-day bake period, or dismissed. */
  available: AvailableUpdate | null;
  /** When the last successful check ran (RFC 3339); null if never. */
  checked_at: string | null;
}

/** How a directly-invoked tool currently resolves, for the settings status line. */
export interface ResolvedTool {
  tool: string;
  /** The path we'd invoke — an absolute resolved path, or the bare name if not found. */
  path: string;
  exists: boolean;
}

/** `info` reports no problem — something worth knowing that needs no action
 *  (e.g. the preferred terminal font isn't installed, so a stock one is used). */
export type HealthStatus = "pass" | "fail" | "warn" | "info" | "skipped";

/** One prerequisite check in a repo health report; `sub` nests the GitHub
 *  token check's validity / read / write sub-checks. */
export interface HealthCheck {
  id: string;
  label: string;
  status: HealthStatus;
  detail: string;
  sub: HealthCheck[];
  /** Suggested remediation command (e.g. a `git clone` when not checked out). */
  command?: string;
}

export interface HealthReport {
  repo: string;
  checks: HealthCheck[];
}

export type CredentialScope = { kind: "identity"; identity_id: string };

export interface CredentialTypeDto {
  type_id: string;
  display_name: string;
  description: string;
}

export const api = {
  listCredentialTypes: () =>
    invoke<CredentialTypeDto[]>("plugins_list_credential_types"),

  credentialExists: (type_id: string, scope: CredentialScope) =>
    invoke<boolean>("credentials_exists", { typeId: type_id, scope }),

  setCredential: (type_id: string, scope: CredentialScope, secret: string) =>
    invoke<void>("credentials_set", { typeId: type_id, scope, secret }),

  deleteCredential: (type_id: string, scope: CredentialScope) =>
    invoke<void>("credentials_delete", { typeId: type_id, scope }),

  listRepos: () =>
    invoke<string[]>("repos_list"),

  /** The hand-written JSON Schema for per-repo settings, used by the Settings
   *  window's JSON Forms renderer. */
  repoSettingsSchema: () =>
    invoke<Record<string, unknown>>("repo_settings_schema"),

  getRepoSettings: (repo: string) =>
    invoke<RepoSettings>("repo_settings_get", { repo }),

  setRepoSettings: (repo: string, settings: RepoSettings) =>
    invoke<void>("repo_settings_set", { repo, settings }),

  setRepoVisibility: (repo: string, hidden: HideState | null) =>
    invoke<void>("repo_set_visibility", { repo, hidden }),

  /** Untrack a repo (delete its settings file). Worktrees and sessions are kept. */
  removeRepo: (repo: string) =>
    invoke<void>("repo_remove", { repo }),

  setSessionVisibility: (sessionId: string, hidden: HideState | null) =>
    invoke<void>("session_set_visibility", { sessionId, hidden }),

  scanEnvFiles: (clonedRepoDir: string) =>
    invoke<string[]>("repo_scan_env_files", { clonedRepoDir }),

  /** Run per-repo prerequisite diagnostics (cloned checkout, CLIs, GitHub token
   *  + permissions, env files) for the Settings window's health-check modal. */
  repoHealthCheck: (repo: string) =>
    invoke<HealthReport>("repo_health_check", { repo }),

  identitiesList: () =>
    invoke<string[]>("identities_list"),

  getDefaultIdentity: () =>
    invoke<string | null>("identities_get_default"),

  identitiesAdd: (identityId: string) =>
    invoke<void>("identities_add", { identityId }),

  /** Remove an identity: its Keychain credentials, its list entry, and any repo references. */
  identitiesRemove: (identityId: string) =>
    invoke<void>("identities_remove", { identityId }),

  githubListRepos: (identityId: string) =>
    invoke<GHRepo[]>("github_list_repos", { identityId }),

  /** Look up one repo by "owner/name" — for tracking a repo the identity has no
   *  affiliation with, so it never shows up in `githubListRepos`. */
  githubGetRepo: (identityId: string, repo: string) =>
    invoke<GHRepo>("github_get_repo", { identityId, repo }),

  githubListIssues: (identityId: string, repo: string) =>
    invoke<IssueNode[]>("github_list_issues", { identityId, repo }),

  openUrl: (url: string) =>
    invoke<void>("open_url", { url }),

  openPath: (path: string) =>
    invoke<void>("open_path", { path }),

  /** Whether a user-configured path exists on disk (tilde-expanded). Backs the
   *  soft path validation in the Settings window. */
  pathExists: (path: string) =>
    invoke<boolean>("path_exists", { path }),

  /** Reveal a path in Finder, selecting it in its parent folder (`open -R`). */
  revealPath: (path: string) =>
    invoke<void>("reveal_path", { path }),

  /** Focus or open a session's VS Code window, unless a pending agent switch
   *  means it must restart first. `focusExisting` skips that check and just
   *  brings the (old agent's) window to the front. */
  openInEditor: (sessionId: string, focusExisting = false) =>
    invoke<OpenOutcome>("session_open_in_editor", { sessionId, focusExisting }),

  /** Switch an existing session to another agent (issue #186). */
  setSessionAgent: (sessionId: string, agent: Agent) =>
    invoke<SetAgentOutcome>("session_set_agent", { sessionId, agent }),

  /** Close and reopen a session's VS Code window so it runs the recorded agent. */
  restartSessionEditor: (sessionId: string) =>
    invoke<RestartOutcome>("session_restart_editor", { sessionId }),

  /** Whether opening this session (or spawning in this repo, with no session)
   *  starts a Codex session that will ask the user to review mAIestro Code's
   *  status hooks. Always false for Claude, and on any error. */
  codexHooksReviewNeeded: (repo: string, sessionId?: string) =>
    invoke<boolean>("codex_hooks_review_needed", { repo, sessionId: sessionId ?? null }).catch(() => false),

  openRepoInEditor: (repo: string) =>
    invoke<void>("open_repo_in_editor", { repo }),

  prepareSpawn: (repo: string, issueNumber: number) =>
    invoke<SpawnPlan>("prepare_spawn", { repo, issueNumber }),

  suggestShortTitle: (repo: string, issueNumber: number) =>
    invoke<string>("suggest_short_title", { repo, issueNumber }),

  draftSpawnPreview: (repo: string, idea: string, requestId: string, useRawFallback = false) =>
    invoke<DraftPreviewOutcome>("draft_spawn_preview", { repo, idea, useRawFallback, requestId }),

  confirmSpawn: (repo: string, edits: SpawnEdits, forceNew = false) =>
    invoke<SpawnResult>("confirm_spawn", { repo, edits, forceNew }),

  createIssueDirect: (repo: string, title: string, body: string) =>
    invoke<CreateIssueOutcome>("create_issue_direct", { repo, title, body }),

  sessionsList: () =>
    invoke<Session[]>("sessions_list"),
  /** Clear a work item's `notice` once the user dismisses it. */
  dismissSessionNotice: (sessionId: string) =>
    invoke<void>("session_dismiss_notice", { sessionId }),

  sessionsStatusList: () =>
    invoke<StatusRecord[]>("sessions_status_list"),

  clearSessionError: (workspace: string) =>
    invoke<void>("clear_session_error", { workspace }),

  teardown: (sessionId: string, confirmed = false, force = false) =>
    invoke<TeardownOutcome>("teardown", { sessionId, confirmed, force }),

  openAccessibilitySettings: () =>
    invoke<void>("open_accessibility_settings"),

  sessionPr: (sessionId: string) =>
    invoke<PrLink | null>("session_pr", { sessionId }),

  createPr: (sessionId: string, requestId: string) =>
    invoke<PrLink>("session_create_pr", { sessionId, requestId }),

  sessionPrChecks: (sessionId: string) =>
    invoke<PrChecks | null>("session_pr_checks", { sessionId }),

  sessionWorkState: (sessionId: string) =>
    invoke<WorkState | null>("session_work_state", { sessionId }),

  mergePr: (sessionId: string, requestId: string) =>
    invoke<PrLink>("session_merge_pr", { sessionId, requestId }),

  logsRead: () =>
    invoke<string>("logs_read"),

  logsReveal: () =>
    invoke<void>("logs_reveal"),

  /** Open (and focus) the Settings window. Routed through the backend so this
   *  and the tray menu share one show path, which tells the Settings webview to
   *  re-read its data on each fresh open (#139). */
  openSettings: () =>
    invoke<void>("open_settings"),

  getTheme: () =>
    invoke<Theme>("app_settings_get_theme"),

  /** The hand-written JSON Schema for the global settings, for the Settings
   *  window's JSON Forms renderer. */
  appSettingsSchema: () =>
    invoke<Record<string, unknown>>("app_settings_schema"),

  getAppSettings: () =>
    invoke<AppSettings>("app_settings_get"),

  setAppSettings: (settings: AppSettings) =>
    invoke<void>("app_settings_set", { settings }),

  /** Finish onboarding with the chosen options and dismiss the onboarding window. */
  completeOnboarding: (launchAtLogin: boolean) =>
    invoke<void>("onboarding_complete", { launchAtLogin }),

  /** Per-tool resolution (path + whether it exists), for the Tool paths status line. */
  toolsResolved: () =>
    invoke<ResolvedTool[]>("tools_resolved"),

  /** Version + build metadata of the running app, for the About section. */
  appVersion: () =>
    invoke<AppVersion>("app_version"),

  /** Whether settings.json is currently unreadable, for the popover banner. */
  appSettingsProblem: () =>
    invoke<SettingsProblem | null>("app_settings_problem"),

  /** Current verdict of the background update check (#182). */
  updateStatus: () =>
    invoke<UpdateStatus>("update_check_status"),

  /** Remember that the banner for `version` was dismissed; it returns for a newer one. */
  updateDismiss: (version: string) =>
    invoke<void>("update_dismiss", { version }),
};

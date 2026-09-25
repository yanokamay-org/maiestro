# mAIestro Code

## Shell commands

Never hardcode `/Users/<name>/` in Bash commands. Use relative paths or `~` instead (e.g. `~/src/maiestro`, `./backend`). A hook blocks any command containing a hardcoded `/Users/` path.

A Tauri v2 menu-bar app on macOS that launches git-worktree-per-issue development workspaces, opening a Claude Code session in VSCode or a terminal for each. mAIestro Code is a launcher and dashboard, not a session host.

Directory layout: `backend/` (Rust/Tauri) and `frontend/` (web). Not Tauri's defaults of `src-tauri/` and `src/`.

The frontend uses **pnpm** (`pnpm-lock.yaml`), not npm or yarn. Use `pnpm install` / `pnpm dev` — `npm install` fails on the `link:` workspace deps.

The menu-bar tray icon is a **brain** icon (`backend/icons/tray.png`, a template image set at `main.rs:258`). Any docs or prose that describe the menu-bar icon should call it the brain icon.

## Git workflow

**Every change to `main` must go through a pull request.** Do not commit or push directly to `main`: branch, push the branch, open a PR, and merge it on GitHub. This holds even for small fixes and the release flow.

This is enforced on both sides. Server-side, a branch ruleset on `main` requires a pull request and the two CI checks (**Backend (test + clippy)** and **Frontend (typecheck + tests)**) to pass; zero approving reviews are required, so a solo PR merges itself once CI is green, but until then `gh pr merge` fails with *"the base branch policy prohibits the merge"*. Locally, the committed `.githooks/pre-push` hook rejects any push to `main`; enable it once per clone with `git config core.hooksPath .githooks` (already set in the primary checkout; worktrees share it via the common git dir).

**Claude must not commit, push, open a PR, or merge on its own initiative.** "Implement issue #N" or "implement this plan" means edit the files and verify them (build/test/lint) — stop there and report what changed for the user to review, without running `git commit`, `git push`, `gh pr create`, or `gh pr merge`. Only do those when the user's own message explicitly asks for that step (e.g. "commit this", "push it", "open a PR", "merge it") — even CI going green is not itself permission to merge.

## Feature reference in `docs/`

The detailed how-it-works for each subsystem lives in `docs/`. **Read the relevant doc before changing that subsystem**, and update it when the behaviour changes — it is the source of truth for that feature. The decision sections below record only the decision and the invariants that tests enforce.

| Before changing… | Read |
|---|---|
| Per-repo or global settings, their JSON Schemas, the Settings window forms, launch at login / onboarding, the About block | `docs/settings.md` |
| The Check Health diagnostics (`health.rs`) | `docs/health-check.md` |
| Session status pills, the hook helper (`hooks.rs`), `last_error`, the `creating` state | `docs/session-status.md` |
| Worktree colors/emoji, the Claude session color, the generated `.vscode` files | `docs/theming.md` |
| How the `claude` / `git` / `code` binaries are found, `tool_paths` overrides (`tools.rs`) | `docs/tool-resolution.md` |
| The background update check and the popover's update banner (`update_check.rs`) | `docs/update-check.md` |

## Architectural decisions

### Backend module layout

The spawn subsystem is split along its natural seams so each file owns one responsibility (and the logging/testing conventions stay uniform). Where the pieces live:

- **`spawn.rs`** — spawn core (`do_spawn`/`finish_spawn`), the preview commands (`prepare_spawn`/`draft_spawn_preview`/`confirm_spawn`), and `teardown`.
- **`theming.rs`** — worktree color/emoji picking (`pick_theme`) and the palette → Claude-session-color mapping (`claude_color`).
- **`hooks.rs`** — the agent status-hook subsystem, Claude Code or Codex (write, merge, reconcile) — see "Live per-session status".
- **`editor.rs`** — VS Code workspace-file generation, launch/focus, and the teardown window control (AppleScript/`lsof`).
- **`drafting.rs`** — the AI-drafting calls (`agent_text`, which dispatches to `claude -p` or `codex exec`; issue/short-label drafting; `AgentActivity`).
- **`agent.rs`** — the `Agent` enum (`claude` / `codex`); `repo_settings::effective_agent` resolves a repo's agent.
- **`pr.rs`** — the PR lifecycle commands (`session_pr`, `session_create_pr`, `session_pr_checks`, `session_work_state`, `session_merge_pr`).
- **`health.rs`** — the Check Health diagnostics; **`update_check.rs`** — the background newer-release poll; **`tools.rs`** — external tool resolution; **`app_settings.rs`** / **`repo_settings.rs`** — the two settings files and their schemas; **`about.rs`** — version/build info.
- **Shared helpers**: `gitops.rs` (`git()` / `local_branch_exists()`), `naming.rs` (slug/label helpers), `repo_context.rs` (the settings→identity→GitHub-client resolution + `validated_cloned_repo`), and `tools::{snippet, shell_quote}`.

### mAIestro Code launches sessions; it does not host them

mAIestro Code is a **launcher and dashboard**, not a session host. The popover lists tracked repos and their open issues and lets the user start work — it does **not** contain the chat with Claude.

Hosting the conversation in the popover — running `claude` headlessly with its stdio piped to the backend, with editors attaching to that running session — is explicitly not the design and should not be attempted: there is no headless `claude` subprocess, no stdio piping, and no `SessionRegistry` owning live conversations.

Instead, starting work creates the worktree and **launches the repo's agent (`claude` or `codex`) with the right command-line arguments into a real, user-facing session** — either a VSCode window or a standalone terminal (selected per-repo). The conversation lives in that terminal/editor and the user interacts with it directly.

Consequence: because mAIestro Code does not own the process or its stdio, it cannot directly observe a session's live working/waiting state. That state comes from a separate, out-of-band mechanism instead — see "Live per-session status via agent hooks".

### Talk to GitHub directly, not via `gh`

mAIestro Code uses the GitHub REST API directly (via `octocrab` or a thin `reqwest` wrapper) with per-identity tokens read from Keychain at call time. We do **not** shell out to `gh`.

Reasons:
- `gh auth login` has a single active account, which conflicts with the multi-agent / per-identity model.
- At app launch the shell environment is not sourced and `$PATH` is minimal, so `gh` may not even be discoverable.
- Tokens already need to live in Keychain for the profile model; routing them through `gh` adds a layer with no benefit.

This applies only to mAIestro Code's **own** API calls. The user's `git` operations (fetch/push) happen inside the launched session under the user's ambient git auth, not through mAIestro Code. mAIestro Code's own local git (e.g. `git worktree add`) runs in the existing checkout and relies on its already-configured auth.

### Identity = a named identity, credentials in Keychain

The unit of identity is a plain **identity name** (e.g. `personal`, `work`). The list of known identities (and the optional default) lives in `~/.maiestro/identities.json` (`backend/src/identities.rs`); each repo assigns one via `identity_id` in its per-repo settings. The credential **values** (currently the GitHub token) live in the macOS Keychain under the service name `com.maiestro.cred.<type-id>.identity.<identity-id>` (`backend/src/credentials.rs`) — nothing secret is in `~/.maiestro/`. The JSON files are intentionally developer-friendly (like `~/.ssh/`) so they can be inspected, edited by hand, and managed by dotfile tooling.

Keychain is chosen because it is unlocked at user login, so credentials are available even when mAIestro Code launches at startup (when shell profiles are not sourced and env files are unavailable).

### Launched sessions use the user's ambient environment, via `open -a`

mAIestro Code does **not** build a clean per-profile env or inject credentials into the session it launches. Every launch hands off to the OS: opening an app uses Launch Services (`open -a <App> <worktree-path>`), exactly like a Finder double-click, and starting `claude` in a standalone terminal uses the terminal's own run-command (e.g. `osascript … do script "cd <worktree> && claude …"`), which runs under the user's login shell. Either way the editor/terminal — and the `claude` running inside it — inherits the **user's full ambient environment**: Homebrew PATH, shell integrations, and whatever git/GitHub auth the user already has. Claude authentication likewise comes from the user's own `claude` login (`~/.claude/`). mAIestro Code stores and manages no `ANTHROPIC_API_KEY`, and there is no separate "agent spawn with a constructed env" path.

There is deliberately no per-session GitHub identity isolation, which an injected env would give. The simpler launch path wins; the trade-off is that launched dev sessions act as the user's ambient GitHub identity, so there is effectively one active GitHub identity per machine for the sessions themselves. Keychain-stored tokens matter **only for mAIestro Code's own GitHub API calls** — never injected into the launched session.

One deliberate exception: when VS Code is available, we open worktrees via its `code` CLI (resolved by `tools::find_tool("code")`) instead of `open -a`, so we can pass `--disable-workspace-trust` and skip the "Do you trust the authors?" prompt on every freshly spawned worktree. The `code` CLI forwards that flag even to an already-running VS Code, which `open -a --args` cannot. If no `code` CLI is found we fall back to `open -a "Visual Studio Code" --args --disable-workspace-trust <worktree>`. The session still inherits the user's ambient environment either way.

Which app a session opens in and any workspace-level env files are per-repo settings (`docs/settings.md`).

### External tool resolution: a `tool_paths` pin is authoritative

mAIestro Code shells out to the repo's agent CLI (`claude` or `codex`), `git`, and the VS Code `code` CLI *itself*, and a packaged app launched at login gets a minimal Launch Services `$PATH`. `backend/src/tools.rs` centralizes resolution: at startup it recovers the user's login-shell PATH once and hands it to every child process. An explicit `tool_paths` override in global settings is **authoritative** — `resolve_tool` runs it verbatim even if missing (a broken pin fails loudly), and `find_tool` never falls through to auto-resolution. Only with no pin do we auto-resolve via the enriched PATH and known install locations. An agent binary is resolved only when a repo actually uses that agent, so a single-agent machine never invokes the other one. The same resolved agent path is baked into the session's launch command, so the pin also decides which binary the *session* starts with. Details: `docs/tool-resolution.md`.

### Settings live in `~/.maiestro/`; defaults live only in the JSON Schemas

Per-repo settings are in `~/.maiestro/repos/<owner>-<name>.json` (`repo_settings.rs`), global app settings in `~/.maiestro/settings.json` (`app_settings.rs`). Both are human-editable and dotfile-manageable, and each has a **hand-written JSON Schema** in `backend/schemas/` that is the spec for the format — deliberately not generated from the Rust structs. A `schema_matches_struct` test on each keeps schema and struct in sync (adding a field to one without the other fails `cargo test`), loading validates against the schema and fails loudly on a corrupt file rather than silently using defaults, no writer merges into a corrupt file (`app_settings::update` refuses and logs rather than resetting it), and unknown fields are tolerated for forward-compat. **Defaults are declared as JSON Schema `default` keywords only** and read from there by both the backend (`schema_default`) and the Settings window's JSON Forms renderer; there is no hardcoded copy in Rust or TS. Tracking a repo requires no affiliation with it — anything the identity's token can read works. Field-by-field reference, the forms, launch at login, and the About block: `docs/settings.md`.

### Releases are signed, notarized, and cut by the `/release` skill

Release builds must be signed with a **Developer ID Application** certificate and notarized, and are cut locally on the release Mac rather than in CI. The mechanics — `scripts/release.sh`'s three idempotent phases, the `.env.release` secrets, why signing also governs Keychain trust — live with the **`/release` skill** (`.claude/skills/release/SKILL.md`), which drives the pipeline.

### Backend logging

The Rust backend logs via `tracing` (`backend/src/logging.rs`). `logging::init()` runs first in `main()` and installs two layers: a **daily, UTC-dated file** and **stderr** (so `tauri dev` / `cargo run` show output in the terminal). Log files live at `~/Library/Logs/com.maiestro.app/lYYYYMM/maiestro-YYYYMMDD.log`; the directory and filename derive from the current UTC date, every line's timestamp is UTC (RFC 3339), and the file rolls over at UTC midnight while the long-running process keeps going (a custom `MakeWriter`, since `tauri-plugin-log` fixes its path at startup). `tail` that file to read a packaged build's log.

Conventions:
- **Every Tauri command** logs its invocation via a `log_invoke!`/`log_invoke_debug!` macro (first statement of the command), naming the command and key args. Action commands (spawn, teardown, create PR/issue, set/delete) log at `info`; read-only "get status" commands the UI polls (`sessions_list`, `session_pr`, `repos_list`, `repo_settings_get`, `identities_*`, `credentials_exists`, `github_list_*`, …) log at `debug` so they don't drown the `info` log — set `RUST_LOG=debug` to see them. The one deliberate exception is `logs_read`: the Logs window polls it every ~2s to display the log itself, so logging it would flood the very file it reads.
- **Every GitHub API call** logs method + URL + status from the single `GitHub::send` choke point in `plugins/github.rs`: GETs (read-only, polled) at `debug`, mutations (POST/PATCH/DELETE) at `info`. A non-2xx response logs at `error` regardless.
- **Workspace-scoped commands** (`teardown`, `session_pr`, `session_create_pr`, and the spawn path via `do_spawn`) are wrapped in a `#[tracing::instrument]` span carrying a `session=<workspace-id>` field. Because the span follows the async work across `.await`s, every nested line it emits — git ops, GitHub API calls, Claude drafts — carries the same `session=`, so you can `grep 'session=28-add-foo'` to see one workspace's whole story. Commands with no workspace (e.g. `repos_list`, `github_list_repos`) have no `session` field.
- **Never log credentials.** Command invocations log credential *types* and identity scopes but never secret values; the GitHub token lives only in the `Authorization` header, which is never logged.
- Default level is `info`; override with the `RUST_LOG` env var (standard `EnvFilter` syntax, e.g. `RUST_LOG=maiestro=debug`).
- Old daily files are not auto-pruned yet (cleanup is a possible follow-up).

### One color per worktree, all the way to the session

A spawned worktree gets a deterministic color and emoji from `theming::pick_theme`, and that one color is applied in the popover row, VS Code's title/status/activity bars, and (for Claude) the session's own UI; Codex has no `/color`, so a Codex session carries no color of its own. `PALETTE` is sized and ordered to match Claude Code's eight session colors one-for-one, so `theming::claude_color` is a **bijection** — a unit test enforces it, so adding a ninth palette entry fails the build. The color reaches the session as a `/color <name>` initial prompt on the launch command (not `--agent-color`, which is silently ignored outside teammate sessions). Reopening a worktree regenerates its `.vscode` files from the **session record** — it must never re-theme the worktree, and hand edits to those generated files do not survive. Details: `docs/theming.md`.

### Live per-session status via agent hooks

Because mAIestro Code does not host the session, its working/waiting state comes from agent hooks that the spawn path writes into the worktree's `.claude/settings.local.json` (Claude), or passes on the Codex launch command as `-c hooks.…` session flags (Codex, chosen from the session record's `agent`, with its own event table). Each hook invokes **the mAIestro Code binary itself** (`maiestro hook <state> --workspace <ws-id>`, a hidden CLI subcommand dispatched before Tauri starts) which writes `~/.maiestro/status/<ws-id>.json`; the backend watches that directory and emits `session-status` events. Because the baked binary path only survives while the spawning build does, **startup and every reopen reconcile the hooks** of all tracked sessions to the running binary, so hook or verb changes reach existing worktrees without a re-spawn. `creating` is mAIestro Code's own pre-agent state for the background spawn. Tool failures are logged always but only **surfaced** in the popover when persistent or when Claude stops without recovering; Codex has no failed-tool hook, so Codex sessions never get a `last_error`. Codex runs a hook only after the user trusts that exact definition, so Codex hooks are **identical for every worktree** (a stable `~/.maiestro/bin/maiestro-hook <verb>` wrapper, the workspace resolved from the payload's `cwd`): the user trusts them once with `/hooks` → trust all, and never again. We never write `~/.codex/` or bypass that trust. The helper must never block or crash the user's session. Details: `docs/session-status.md`.

### Repo health check never mutates the repo

**Check Health** in a repo's Settings form runs per-repo diagnostics (cloned repo, git, the repo's agent login + model — only that agent, GitHub token & permissions, session editor, env files, terminal font) and streams them into a modal. It is **informational only** — it never blocks spawning — and GitHub write access is **derived** from token scopes / the repo's `permissions.push` flag, never exercised with a throwaway issue or PR. Details: `docs/health-check.md`.

### The update check is an anonymous, read-only poll of GitHub Releases

`update_check.rs` asks `api.github.com` for this repo's latest published release every ~12 hours (and once ~15 s after launch when the persisted `checked_at` is older than that) and offers it in the popover as a dismissable banner — only when it is strictly newer than the running version and has been public for at least 5 days. It uses `GitHub::anonymous()` — **no identity token, ever** — so it works with no identity configured and reveals nothing about the install (the generic user agent carries no version); it still goes through the `GitHub::send` choke point, so the logging invariant holds. It never auto-downloads or installs: the banner opens the release page. Any failure is logged at `warn` and the previous result stands. The README's privacy section describes this request and must stay accurate if it changes. Details: `docs/update-check.md`.

### Launch at login is a per-user LaunchAgent, driven from Rust

The opt-in **Launch at login** preference is backed by `tauri-plugin-autostart` with `MacosLauncher::LaunchAgent`, writing `~/Library/LaunchAgents/mAIestro.plist` (name pinned, not derived from productName), because a LaunchAgent runs inside the user's GUI login session (Keychain available). The plugin is called **only from our own Rust**, never JS. Startup reconciles the plist with the stored preference (rewriting a stale dev-binary path), and a one-time **onboarding** webview window is gated on the machine-managed `onboarding_completed` flag. Details: `docs/settings.md`.

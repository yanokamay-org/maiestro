# mAIestro

## Why mAIestro

Getting real leverage out of AI coding means running **several agent sessions
in parallel**. The bottleneck stops being any single session and becomes *you*: the
constant context switching between them. That's what mAIestro exists to solve.
mAIestro colors and launches each session, and displays the session status highlighting
which sessions need attention.

mAIestro is deliberately **not** another place to chat with an agent. It
doesn't get in the way of your session interactions — the conversation still
happens in your editor or terminal.

## What it is

A macOS menu-bar app that quickly shows active AI coding sessions. Features include:

- **One worktree per issue.** Spawn Work on an issue creates a branch and a
  worktree (e.g. `~/src/work-42-fix-thing/repo`), runs your setup commands, and
  opens your editor.
- **Live session status.** Each row shows what the AI engine is doing — working,
  waiting for you, idle.
- **Idea → issue.** Type a rough idea and AI drafts the GitHub issue title
  and body before anything is created.
- **PRs from the dashboard.** Create an AI-drafted pull request for a
  workspace, watch its checks, and merge it — then tear the worktree down.
- **Multiple git repo support.** Work on multiple repos at the same time.
- **Visibility and snooze controls.** Just focus on your active work. If there is a repo or work item you want to pause, just hide or snooze it for a couple of days.
- **Color coded sessions.** Each session is assigned a color and emojis for remote control sessions, so it is easy to find across multiple windows.
- **Per-identity GitHub access.** GitHub tokens are stored in the macOS
  Keychain per identity; each repo is tracked under the identity you choose.

## Installation

### MacOS

**1. Install mAIestro.** Download the latest `.dmg` from the
[Releases page](https://github.com/emisch0/maiestro/releases) and drag
**mAIestro** to Applications.

**2. Install Claude Code.** mAIestro launches `claude` into every workspace it
creates, so it has to be installed and logged in first. Either installer works:

```bash
curl -fsSL https://claude.ai/install.sh | bash   # native installer
brew install --cask claude-code                  # or via Homebrew
```

Then check it and log in — run `claude` once and follow the browser prompt:

```bash
claude --version   # prints e.g. 2.1.266 (Claude Code)
claude             # log in on first run; /login inside a session re-runs it
```

Claude Code needs a Pro, Max, Team, Enterprise, or Console account; the free
Claude.ai plan does not include it. mAIestro stores no API key of its own —
sessions run under your `claude` login. See the
[Claude Code setup docs](https://code.claude.com/docs/en/setup) if the install
misbehaves, or run `claude doctor`.

**3. Set up git and GitHub for your sessions.** Spawned sessions push branches
and fetch under your *ambient* git auth, not through mAIestro. Make sure `git`
is there (`git --version` prompts to install the Xcode Command Line Tools if it
isn't), then authenticate — the GitHub CLI is the easiest way, since
`gh auth login` also sets up git's credential helper:

```bash
brew install gh
gh auth login      # choose HTTPS and "authenticate Git with your credentials"
```

mAIestro itself never shells out to `gh` — its own API calls use the token you
save in [GitHub token](#github-token) below. `gh auth login` is for the git
operations that happen *inside* a spawned session.

**4. Recommended: a Nerd Font.** Claude Code's terminal UI draws box-drawing and
powerline glyphs that a plain monospace font renders as tofu (□). Install with
this command:

```bash
brew install --cask font-jetbrains-mono-nerd-font
```

### Other Platforms

mAIestro is not yet available on other platforms, but it is designed to support multiple OS, AI agents, etc. Please submit a github issue to request more platforms.

## Quick start

1. Click the brain icon in the menu bar and open **Settings**.
2. In **Identities**, add an identity (e.g. `default`) and save a GitHub
   personal access token for it — see [GitHub token](#github-token) below for
   the permissions it needs. The token goes into the macOS Keychain;
   `~/.maiestro/identities.json` keeps only the identity name.
3. In **Repos**, click **Add Repo**, pick the identity, and choose a repository
   from the fetched list — or type any `owner/name` you can read. Point its
   settings at your local clone if it isn't at the default `~/src/<repo-name>`.
4. Click **Check Health** at the top of the repo's settings and fix anything it
   reports — see [Check Health](#check-health) below. It catches a missing
   clone, an unresolvable `claude`, or a token without push access *before*
   they break a spawn.
5. Back in the popover, the repo lists its open issues. Hit **Spawn Work** on
   one — mAIestro creates the worktree, copies your env files, runs your
   post-spawn commands, and opens the editor with Claude working the issue.
   Or write your own idea and let Claude draft the issue first.
6. When the work is ready, create the PR from the workspace row (Claude drafts
   the description), merge it once checks pass, and tear the workspace down.

### GitHub token

mAIestro talks to the GitHub REST API directly with a token you store per
identity. A **fine-grained** personal access token
([create one](https://github.com/settings/personal-access-tokens/new)) is the
recommended kind:

- **Repository access** — *Only select repositories*, and pick every repo you
  want mAIestro to track (or *All repositories* if you prefer).
- **Repository permissions** — these four:

  | Permission | Access | Used for |
  | --- | --- | --- |
  | **Metadata** (required) | Read-only (no write option) | Reading the repo itself |
  | **Contents** | Read and write | How mAIestro derives whether the token can push |
  | **Issues** | Read and write | Listing and creating issues, assigning, commenting |
  | **Pull requests** | Read and write | Creating, reading, and merging PRs; the checks dot |

  Add **Workflows** if PRs will touch `.github/workflows/`, and **Merge
  queues** on a repo that merges through a queue — GitHub refuses those merges
  otherwise. **Actions** and **Commit statuses** are *not* needed: the PR
  pill's checks dot comes from the pull request's own `mergeable_state`, not
  the Checks or Statuses APIs, so a token without them still shows check
  state correctly.

A **classic** token works too — it needs the `repo` scope (`public_repo` is
enough for a public repo).

Whichever kind you use, mAIestro never injects it into a spawned session;
sessions push and fetch under your ambient git auth from `gh auth login`. The
health check below verifies the token without ever creating a throwaway issue
or PR to test it.

### Check Health

Each repo's settings form has a **Check Health** button that runs the
prerequisites in one pass and streams the results:

- **Cloned repo exists** — `cloned_repo_dir` is a git repo whose `origin`
  really points at this `owner/name`.
- **Git available** and **Session editor available** — the `git` and VS Code
  `code` CLIs resolve (pin them under **Preferences → Tool paths** if not).
- **Claude logged in**, with a **model available** sub-check — a real probe of
  the repo's `prompt_model`, so a login problem and a bad model name are
  reported separately.
- **GitHub token & permissions** — the token is valid, the repo is readable,
  and the token can push. This is derived from the scopes and permissions
  GitHub reports; mAIestro never creates a throwaway issue or PR to test.
- **Configured env files exist** and **Terminal font installed** — advisory
  warnings, not failures.

Failures are informational — they never block a spawn — and several rows come
with a copy-pasteable fix.

## The main window

An annotated tour of the popover (illustrative diagram, not a screenshot):

<p align="center">
  <img src="docs/images/main-window.svg" width="680" alt="Annotated diagram of the mAIestro popover: header, repo groups, lifecycle zones, workspace rows with status pills, and the per-workspace command strip">
</p>

1. **Show-hidden toggle and Settings** — the eye reveals hidden/snoozed repos
   and workspaces; the gear opens the Settings window (identities, repos,
   appearance).
2. **Repo group** — one section per tracked repo, with a button to open its
   checkout in VS Code.
3. **Start Work** — opens the issue picker for this repo (next diagram).
4. **Claude status pill** — the session's live state, fed by Claude Code hooks:
   *Working* (rainbow ring), *Needs you* (amber — e.g. a permission prompt),
   *Ready*/*Idle* (muted). Click it to jump into the session's editor. A red
   tint means a tool call failed and Claude stopped without recovering; the
   error shows inline and can be dismissed.
5. **Lifecycle zones** — workspaces are grouped as *Planning* → *Implementing*
   (a PR exists) → *Merged*, and slide between zones as their phase changes.
6. **PR pill** — links to the workspace's pull request; the dot is the live
   checks status (green passing, spinning while running).
7. **Quick links** — open the issue on GitHub, reveal the worktree in Finder,
   or open it in VS Code.
8. **Command strip** (expand a row with its chevron) — *Create PR* (Claude
   drafts the description from the diff), *Merge PR* (creates the PR if
   needed, waits for checks, merges), *Hide…* (snooze the row), and *Tear
   Down* (remove the worktree; warns about uncommitted or unpushed work).
   While an operation runs, the row shows a busy pill — *Creating…*,
   *Creating PR…*, *Merging…*, *Tearing down…*.

Clicking **Start Work** opens the issue picker:

<p align="center">
  <img src="docs/images/start-work.svg" width="680" alt="Annotated diagram of the Start Work overlay: idea box, issue list with refresh, and the expanded spawn preview with session label, branch, and worktree path">
</p>

1. **Idea box** — describe what you want in plain words; Claude turns it into
   a titled GitHub issue.
2. **Create Issue / Create Issue and Spawn** — file the drafted issue, or file
   it and immediately spawn a workspace for it.
3. **Open issues** — the repo's open issues, refreshable; issues that already
   have a workspace sink to the bottom with their phase.
4. **Spawn preview** — expanding an issue shows what will be created: an
   editable short session label (drafted by Claude) plus the derived
   workspace, branch, and worktree path.
5. **Spawn Work** — creates the worktree, copies env files, runs post-spawn
   commands, and opens the editor with Claude briefed on the issue.

## Configuration

Everything lives under `~/.maiestro/`, in human-editable JSON that plays well
with dotfile tooling. The Settings window is the GUI over the same files.

| Path | What it holds |
| --- | --- |
| `identities.json` | The known identity names and the optional default (the tokens themselves live in the macOS Keychain, never here) |
| `repos/<owner>-<name>.json` | Per-repo settings (see below) |
| `settings.json` | App-wide settings: theme (`light`/`dark`/`system`), CLI tool-path overrides, terminal font, launch-at-login, popover size |
| `sessions/` / `status/` | Workspace records and live session status (managed by the app) |

Per-repo settings cover the local clone path (`cloned_repo_dir`), where
worktrees are created (`worktree_prefix`), `.env` files to copy into each new
worktree (`env_files`), shell commands to run after a worktree is created
(`post_spawn_commands`, e.g. `pnpm install`), and overrides for the prompts
mAIestro sends Claude when drafting issues, labels, and PRs (`prompts`).

The format is specified by a JSON Schema at
[`backend/schemas/repo-settings.schema.json`](backend/schemas/repo-settings.schema.json)
(bundled with the app at
`/Applications/mAIestro.app/Contents/Resources/schemas/repo-settings.schema.json`).
Point a `$schema` key at it for autocomplete when hand-editing.

Logs are written to `~/Library/Logs/com.maiestro.app/lYYYYMM/maiestro-YYYYMMDD.log`
(UTC-dated, daily rollover) and are viewable from the app's Logs window.

## Development

Built with [Tauri v2](https://tauri.app): a Rust backend and a React + Vite +
TypeScript frontend.

```text
maiestro/
  backend/          # Rust / Tauri: tray, windows, git/GitHub, spawning, hooks
    src/main.rs
    tauri.conf.json
    schemas/        # JSON Schema for the per-repo settings file
    icons/          # app + tray icons
  frontend/         # React + Vite frontend (popover, Settings, Logs windows)
  scripts/          # release.sh — bump / build / publish pipeline
  vite.config.ts    # Vite config (root = frontend/)
  package.json      # JS deps + scripts (dev/build/tauri)
```

### Prerequisites

Install these once. Versions in parentheses are what this was developed
against.

| Tool | Why | Install |
| --- | --- | --- |
| **Homebrew** | macOS package manager used to install Node/pnpm | `/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"` |
| **Xcode Command Line Tools** | C toolchain + macOS SDK that Rust/Tauri link against | `xcode-select --install` |
| **Rust** (1.95) | Compiles the Tauri backend | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| **Node.js** (25) | Runs Vite and the Tauri CLI | `brew install node` |
| **pnpm** (10) | Package manager for the frontend | `brew install pnpm` |

After installing Rust, restart your shell (or `source "$HOME/.cargo/env"`) so
`cargo` is on your `PATH`.

### Develop

```bash
pnpm install     # frontend deps + Tauri CLI (use pnpm, not npm)
pnpm tauri dev   # Vite dev server + the app
```

The first build compiles the full Rust dependency tree and takes a few
minutes; later builds are incremental. Dev builds are ad-hoc-signed, so macOS
re-prompts for Keychain access after each rebuild — expected; only the signed
release build keeps the grant.

### Build and release

```bash
pnpm tauri build
```

produces a `.app` and `.dmg` under `backend/target/release/bundle/` (unsigned
unless you provide signing credentials).

Real releases go through [`scripts/release.sh`](scripts/release.sh) — three
idempotent phases (`bump`, `build`, `publish`) that version-bump, build a
signed + notarized `.dmg`, and publish a GitHub Release. Signing and
notarization credentials live in a gitignored `.env.release` (template:
[`.env.release.example`](.env.release.example)). The `/release` Claude Code
skill drives the whole pipeline.

To regenerate the icon set in `backend/icons/` from a source PNG:

```bash
pnpm tauri icon path/to/source.png -o backend/icons
```

## Architecture

The recorded design decisions live in [`CLAUDE.md`](CLAUDE.md), and the
per-feature reference (settings, health check, session status, theming, tool
resolution) in [`docs/`](docs/) — those are the source of truth; the short
version:

- **mAIestro launches sessions, it doesn't host them.** Starting work opens a
  real VS Code window or terminal running `claude`; the app never owns the
  conversation or its stdio.
- **Session status comes from Claude Code hooks.** Spawning writes hooks into
  the worktree's `.claude/settings.local.json` that call the mAIestro binary,
  which writes status files the app watches.
- **GitHub via the REST API, never `gh`.** The app talks to GitHub directly
  with per-identity tokens read from the Keychain at call time.
- **Secrets in the Keychain, references in JSON.** `~/.maiestro/` holds only
  human-editable configuration; token values never touch disk.
- **Launched sessions get your ambient environment.** Launches go through
  Launch Services / your login shell, so sessions inherit your PATH, git auth,
  and `claude` login — mAIestro injects nothing.

## License

mAIestro is released under the [MIT License](LICENSE).

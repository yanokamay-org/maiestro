# Contributing to mAIestro Code

This guide covers building and running mAIestro Code from source. For what the
app does and how to install a release, see the [README](README.md); for the
recorded design decisions and per-subsystem reference, see
[`CLAUDE.md`](CLAUDE.md) and [`docs/`](docs/).

## Project layout

Built with [Tauri v2](https://tauri.app): a Rust backend and a React + Vite +
TypeScript frontend. The directories are `backend/` and `frontend/`, not
Tauri's default `src-tauri/` and `src/`.

```text
maiestro/
  backend/          # Rust / Tauri: tray, windows, git/GitHub, spawning, hooks
    src/main.rs
    tauri.conf.json
    schemas/        # JSON Schema for the per-repo settings file
    icons/          # app + tray icons
  frontend/         # React + Vite frontend (popover, Settings, Logs windows)
  docs/             # per-feature reference (settings, health check, status, …)
  scripts/          # release.sh — bump / build / publish pipeline
  vite.config.ts    # Vite config (root = frontend/)
  package.json      # JS deps + scripts (dev/build/tauri)
```

## Prerequisites

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

## Develop

```bash
pnpm install     # frontend deps + Tauri CLI (use pnpm, not npm)
pnpm tauri dev   # Vite dev server + the app
```

The first build compiles the full Rust dependency tree and takes a few
minutes; later builds are incremental. Dev builds are ad-hoc-signed, so macOS
re-prompts for Keychain access after each rebuild — expected; only the signed
release build keeps the grant.

## Test and lint

CI runs these on every pull request, and both must pass before a merge:

```bash
cd backend && cargo test --locked && cargo clippy --locked --all-targets -- -D warnings
pnpm typecheck && pnpm test
```

## Git workflow

Every change to `main` goes through a pull request; a branch ruleset on
`main` requires the two CI checks to pass, and the committed
`.githooks/pre-push` hook rejects direct pushes locally. Enable the hook once
per clone:

```bash
git config core.hooksPath .githooks
```

## Build and release

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

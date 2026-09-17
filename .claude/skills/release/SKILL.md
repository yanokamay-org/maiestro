---
name: release
description: Cut a signed, notarized mAIestro Code release — bump the version, build, and publish a GitHub Release with the .dmg attached. Runs the full scripts/release.sh pipeline from the primary checkout, usable from any session including a mAIestro Code-spawned worktree.
allowed-tools: Bash(git *) Bash(gh *) Bash(scripts/release.sh *) Bash(cd *) Read Write AskUserQuestion
---

You are cutting a release. The mechanics live in `scripts/release.sh` (three
idempotent phases: `bump`, `build`, `publish`); your job is the judgment around
them — where to run, what to bump, and the release notes — plus driving the
phases and reporting the result.

Releases are **local**: the build is signed with a **Developer ID Application**
certificate and notarized on the release Mac, not in CI. `build` is slow (a
notarization round-trip to Apple). Because every phase is idempotent,
re-invoking this skill after a failure resumes rather than restarts.

## Procedure

1. **Find the primary checkout and work there.** A mAIestro Code-spawned session runs
   in a *feature* worktree that has no `.env.release` and must never be released
   from. Run `git worktree list`; the **first** entry is the primary checkout.
   Prefix every command below with `cd <primary-checkout> && …` (or verify you
   are already in it). Do **not** switch this session's branch.

2. **Confirm preconditions on the primary checkout.**
   ```
   git -C <primary> branch --show-current      # must be: main
   git -C <primary> status --porcelain          # must be empty
   git -C <primary> fetch origin && git -C <primary> pull --ff-only origin main
   ```
   If the branch isn't `main` or the tree is dirty, stop and tell the user.

3. **Determine the bump kind.** From the skill args (`/release minor`,
   `/release 1.2.0`) if given, else ask with AskUserQuestion among `patch`,
   `minor`, `major`. `patch` is the default for a routine release.

4. **Draft release notes.** Find the previous tag
   (`git -C <primary> describe --tags --abbrev=0` — none means this is the first
   release) and read the commits since it (`git -C <primary> log <prev>..HEAD
   --first-parent --pretty='%s'`, or the last 30 commits when there is no prior
   tag). Write brief, internal-facing markdown notes (this is a solo tool — no
   localization, no character limits). Group user-visible changes; drop pure
   chore/CI noise. **Show the draft and get approval** via AskUserQuestion
   ("Looks good, publish" vs "I'll tweak the wording") before touching anything.

5. **Bump on a release branch, then merge it via PR.** `main` is PR-only — a
   local `pre-push` hook and a GitHub branch ruleset both reject a direct push,
   and the release commit is no exception.
   ```
   cd <primary> && git checkout -b release/v<new-version>
   cd <primary> && scripts/release.sh bump <kind-or-version>
   git -C <primary> commit -am "chore(release): v<new-version>"
   git -C <primary> push -u origin release/v<new-version>
   gh pr create --repo <owner/repo> --base main --head release/v<new-version> \
     --title "chore(release): v<new-version>" --body "<one line + the notes>"
   ```
   `bump` moves `tauri.conf.json`, `package.json`, `Cargo.toml`, and
   `Cargo.lock` together and asserts they agree.

6. **Merge the release PR once CI is green.** The ruleset requires both checks
   (**Backend (test + clippy)** and **Frontend (typecheck + tests)**) and zero
   approvals, so the PR merges itself once they pass — until then `gh pr merge`
   fails with *"the base branch policy prohibits the merge"*. Wait, merge, and
   bring the primary checkout back to `main`:
   ```
   gh pr checks <pr-number> --watch
   gh pr merge <pr-number> --merge
   git -C <primary> checkout main
   git -C <primary> pull --ff-only origin main
   git -C <primary> branch -d release/v<new-version>
   ```
   Everything below runs from this merged `main`, so the tag `publish` creates
   points at the released commit. If CI fails, fix it on the release branch and
   re-run this step — no need to redo the bump.

7. **Build** (slow — notarization):
   ```
   cd <primary> && scripts/release.sh build
   ```

8. **Publish.** Write the approved notes to a temp file, then:
   ```
   cd <primary> && scripts/release.sh publish --notes-file <tmp>
   ```
   This tags `v<version>`, creates the GitHub Release via the REST API, and
   uploads the notarized `.dmg`. It prints the release URL.

9. **Report** the version released and the GitHub Release URL. If any phase
   failed, say which one — re-running the skill resumes from there.

## Phase reference

`scripts/release.sh` is the release pipeline, split into three idempotent phases
so a partially-failed release resumes by re-running it:

- **`bump <patch|minor|major|X.Y.Z>`** — version is single-sourced from
  `backend/tauri.conf.json`; this bumps it there plus `package.json` and
  `backend/Cargo.toml`, syncs `backend/Cargo.lock` (via `cargo metadata
  --offline`, no full build), and asserts all four agree. It does not commit.
- **`build`** — sources `.env.release`, validates the `APPLE_SIGNING_IDENTITY`,
  runs `pnpm tauri build` (Tauri auto-notarizes when the Apple credentials are
  present), and verifies the signature / Gatekeeper assessment / notarization
  staple.
- **`publish [--notes-file <file>]`** — reads the version, derives `owner/repo`
  from the `origin` remote, then tags `vX.Y.Z`, creates the GitHub Release as a
  **draft**, uploads the notarized `.dmg`, and only then publishes it. That
  order is required: the repo has GitHub's **immutable releases** on, which
  freezes a release *and its assets* the moment it is published — a release
  created already-published can never receive its `.dmg` (the upload returns
  HTTP 422 "Cannot upload assets to an immutable release", and the stranded
  release cannot be edited afterwards, only deleted). Because a draft is
  invisible to `GET /releases/tags/<tag>`, the reuse lookup lists releases and
  matches `tag_name` itself, so re-running after a failed upload resumes the
  draft instead of creating a second release. It talks to GitHub via the **REST
  API** (`curl` + `GITHUB_TOKEN` from `.env.release`), not `gh`, matching the
  backend's "Talk to GitHub directly" decision. Each step (tag, release, asset)
  is skipped if already present, and it refuses to publish a dirty tree or an
  un-notarized `.dmg`.

Run `scripts/release.sh` with no args to see this reference from the shell.

## Code signing (and why it also governs Keychain trust)

Beyond distribution, the Developer ID signature is what makes the app's Keychain
access usable: macOS binds a Keychain item's "Always Allow" decision to the
app's *designated requirement*. For an ad-hoc/unsigned build that requirement is
the binary's cdhash, which changes on every rebuild — so the access prompt
returns each launch. A stable Developer ID signature anchors the requirement to
the certificate, so the grant persists.

Dev builds (`tauri dev`) run the raw, ad-hoc-signed binary and will re-prompt on
each rebuild — that is expected and accepted. Signing is **not** committed to
`tauri.conf.json`; the signing identity and notarization secrets live in a
gitignored `.env.release` (template: `.env.release.example`).

## Notes

- `bump` accepts `patch|minor|major` or an explicit `X.Y.Z`.
- `publish` requires `GITHUB_TOKEN` in `.env.release` (Contents: read/write on
  this repo) and refuses to publish an un-notarized `.dmg` or a dirty tree.
- Never release from a feature/spawned worktree — always the primary checkout:
  a spawned worktree has no `.env.release`.
- Never push the release commit straight to `main`; it goes through a PR like
  any other change. `git push --no-verify` gets past the local hook but not the
  GitHub ruleset.

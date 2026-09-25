# Update check

How mAIestro Code learns that a newer release exists and how it tells the user (issue #182). Code: `backend/src/update_check.rs`, the `GitHub::anonymous()` client in `backend/src/plugins/github.rs`, and `frontend/src/components/UpdateBanner.tsx`. The persisted state is the `update_check` block of `~/.maiestro/settings.json` (`docs/settings.md`).

## What it does

- **Polls GitHub Releases in the background.** Every 12 hours (`CHECK_INTERVAL`) the backend fetches `GET https://api.github.com/repos/yanokamay-org/maiestro/releases/latest`. GitHub's `latest` already excludes drafts and prereleases, which matches how `scripts/release.sh publish` creates releases (a draft until the `.dmg` is attached, then published). The reply's `tag_name` (`vX.Y.Z`) and `published_at` are all it keeps.
- **Offers, never installs.** When a newer release qualifies, the popover shows a one-line banner under its header: "A new version of mAIestro Code (v0.4.0) is available", where the version is a link, plus a **View Release** button; both open that version's GitHub Release page in the browser (`links::open_url`), and a **×** dismisses it. Installing the new `.dmg` stays a manual step. There is no `tauri-plugin-updater`, no download, no restart.
- **Nudges, doesn't nag.** The banner appears only when all of these hold (`update_check::verdict`, the one place the rule lives, unit-tested on every branch):
  1. the latest release is **strictly newer** than the running version (a dev build sitting ahead of the last release sees nothing);
  2. it was published **at least 5 days ago** (`BAKE_PERIOD`) — a release that turns out bad and gets pulled or superseded within days never prompts anyone;
  3. it is **not the version the user dismissed** (`dismissed_version`). Dismissing hides that version only; the next release shows the banner again. A dismissed *older* version never suppresses a newer one.

## Schedule and persistence

`update_check::start` runs once from `setup()` in `main.rs`. It seeds a managed `Snapshot` (latest release + `checked_at`) from the persisted block, so a relaunch can show the banner immediately — even offline — and spawns one long-lived task on Tauri's async runtime. The first check waits `next_delay`: the 15 s startup delay (`STARTUP_DELAY`, so it never competes with hook reconciliation, the status watcher, or the first popover) when there is no `checked_at` or it is older than the interval, otherwise until the interval has elapsed since that check. That is what keeps a `tauri dev` rebuild every few minutes from hitting GitHub each time. After the first check the task simply sleeps `CHECK_INTERVAL` between checks; there is no early retry.

A successful check calls `record`, which updates the snapshot and writes `checked_at`, `latest_version`, and `latest_published_at` through `app_settings::update` (the shared read-modify-write lock), keeping `dismissed_version` and every other setting from disk. `app_settings_set` in turn keeps the whole block from disk when the General form saves, and the form hides it. All four fields are strings: a hand edit that is not RFC 3339 or semver degrades to "unknown" at the point of use (`Snapshot::from_persisted`) instead of failing the settings load.

## The request, and what leaves the Mac

The check uses `GitHub::anonymous()`, the only unauthenticated `GitHub` client: it reads nothing from the Keychain and sends **no `Authorization` header** (a wiremock test asserts this). It therefore works with no identity configured and never spends anyone's token or rate limit. What GitHub receives is what any HTTPS request carries — the IP address — plus the app's generic `User-Agent`, `maiestro/0.1`, which deliberately does not include the installed version. No account, machine, or install identifier is sent, and the installed version is compared locally. The README's "Security and privacy" section states exactly this and must be kept accurate if the request changes.

Unauthenticated GitHub calls are limited to 60 per hour per IP. One every 12 hours is far inside that, and the request goes through `GitHub::get_json`, whose ETag cache turns an unchanged `latest` into a `304 Not Modified` that GitHub does not count. Because it goes through `GitHub::send`, the call is logged like every other GitHub call — the GET at `debug`, a non-2xx at `error` — so `RUST_LOG=debug` shows each check.

## Failure policy

Soft-fail everywhere. A network error, a non-2xx status, a tag that is not semver, or a `published_at` that is not RFC 3339 logs a `warn` ("update check failed; keeping previous result") and changes nothing: the in-memory snapshot and the persisted block keep the last good result, `checked_at` is not advanced, and the next scheduled tick retries. Nothing is emitted to the UI, and the popover never shows an error for it. A failed settings write after a successful fetch is also only logged — the in-memory result still serves the popover until the next launch. One case deserves a name: when `settings.json` is present but unreadable (a hand-edit typo), `app_settings::update` refuses to merge into it rather than erase every other setting, so `record` logs the refusal, `start` warns at launch that results won't persist, and — because nothing persisted — the check runs 15 s after every launch until the file is fixed. The popover shows a red "Couldn't read settings.json" banner above this one (`SettingsProblemBanner`), and the Settings window's General panel shows the same error.

## The frontend side

Two commands: `update_check_status` (polled, `debug`) returns `{ available: AvailableUpdate | null, checked_at }`, where `available` is already the verdict — the popover applies no rule of its own; `update_dismiss(version)` (`info`) records the dismissal. `MainView` reads the verdict inside `refreshAll`, which already runs on mount, on every `popover-shown`, and on regained focus, so a verdict that changed while the popover was hidden appears on the next open; no backend → frontend event is needed, since the verdict changes at most once per check. Dismiss hides the banner optimistically and then calls the command, so the next read agrees. `UpdateBanner` (`.notice-banner--update` in `styles.css`, accent-tinted like a nudge rather than an error, one line tall, text truncating with an ellipsis so it never wraps at the popover's 480 px minimum width) sits between `.panel-header` and `.work-list`.

## Trying it locally

From `tauri dev`, hand-edit the `update_check` block in `~/.maiestro/settings.json` before launching: delete `checked_at` (or set it more than 12 hours old) to force a check ~15 s after launch — with `RUST_LOG=debug` the GET appears in the log — or seed `latest_version` / `latest_published_at` with a version above `tauri.conf.json`'s and a date more than 5 days ago to see the banner without a real release. Dismiss, and `dismissed_version` appears in the file; bump `latest_version` again and the banner returns.

## Not part of this

No auto-download or auto-install; no "Check for updates now" button and no "update available" line in the About block; no opt-out preference (the check is always on — a `check_for_updates` toggle would be a follow-up); no prerelease/beta channel; no notification outside the popover (macOS notifications, tray badge).

//! Background update check (issue #182): is there a newer mAIestro Code release
//! than the one running?
//!
//! Every [`CHECK_INTERVAL`] (12 h; and once shortly after launch, when the persisted
//! `checked_at` is absent or older than that) the backend fetches the latest
//! published GitHub Release of this repo — **unauthenticated**, via
//! [`GitHub::anonymous`], so it works with no identity configured and never
//! spends anyone's token — and keeps the result in memory and in the
//! machine-managed `update_check` block of `~/.maiestro/settings.json`. The
//! popover asks for the verdict on every show (`update_check_status`); the
//! banner it renders is offered only when the newer release has been public for
//! at least [`BAKE_PERIOD`] (a release that gets pulled never prompts anyone) and
//! the user hasn't dismissed that exact version (`update_dismiss`).
//!
//! Soft-fail throughout: a network, HTTP, or parse failure is logged at `warn`,
//! the previous result stands, and the next tick retries. Nothing reaches the UI
//! as an error, and nothing auto-downloads — the banner opens the release page.
//! Details and the privacy properties of the request: `docs/update-check.md`.

use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use semver::Version;
use serde::Serialize;
use tauri::Manager;

use crate::app_settings::{self, UpdateCheckState};
use crate::plugins::GitHub;

/// How often to poll GitHub Releases. Unauthenticated calls are limited to 60
/// per hour per IP; one every twelve hours (a `304` from the ETag cache is free)
/// is far inside that.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);
/// How long after launch the first (due) check waits, so it never competes with
/// startup work — hook reconciliation, the status watcher, the first popover.
const STARTUP_DELAY: Duration = Duration::from_secs(15);
/// A release must have been published at least this long before the banner
/// offers it.
const BAKE_PERIOD: chrono::Duration = chrono::Duration::days(5);
/// The GitHub repo whose releases are checked — this app's own.
const RELEASES_REPO: &str = "yanokamay-org/maiestro";

/// The latest published release as reported by GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestRelease {
    pub version: Version,
    pub published_at: DateTime<Utc>,
}

/// What the last successful check learned, plus when it ran. Managed Tauri
/// state (`Mutex<Snapshot>`), seeded from the persisted block at startup so the
/// banner can show right after a relaunch — even offline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub latest: Option<LatestRelease>,
    pub checked_at: Option<DateTime<Utc>>,
}

impl Snapshot {
    /// Rebuild from the persisted block. A malformed or partial block degrades to
    /// "unknown" for that part (a hand-edited file must never break the check).
    pub fn from_persisted(state: Option<&UpdateCheckState>) -> Self {
        let Some(state) = state else { return Self::default() };
        let checked_at = state.checked_at.as_deref().and_then(parse_utc);
        let latest = match (
            state.latest_version.as_deref().and_then(parse_version),
            state.latest_published_at.as_deref().and_then(parse_utc),
        ) {
            (Some(version), Some(published_at)) => Some(LatestRelease { version, published_at }),
            _ => None,
        };
        Self { latest, checked_at }
    }
}

/// The newer release the popover should offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AvailableUpdate {
    /// Semver of the newer release, e.g. `0.4.0`.
    pub version: String,
    /// The GitHub Release page for it (what the banner's Download opens).
    pub release_url: String,
    /// When GitHub says it was published (RFC 3339).
    pub published_at: String,
}

/// Reply of `update_check_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateStatus {
    /// `None` when up to date, never checked, still inside the bake period, or
    /// dismissed.
    pub available: Option<AvailableUpdate>,
    /// When the last successful check ran (RFC 3339), if ever.
    pub checked_at: Option<String>,
}

fn parse_utc(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&Utc))
}

/// Parse a release tag (`v0.4.0`) or bare version (`0.4.0`).
fn parse_version(s: &str) -> Option<Version> {
    Version::parse(s.trim().trim_start_matches('v')).ok()
}

// ── The decision ────────────────────────────────────────────────────────────

/// The one place the banner rule lives: offer `latest` only when it is strictly
/// newer than `current`, has been published for at least [`BAKE_PERIOD`] as of
/// `now`, and is not the version the user dismissed. Dismissing an *older*
/// version never suppresses a newer one.
pub fn verdict(
    current: &Version,
    latest: Option<&LatestRelease>,
    dismissed: Option<&str>,
    now: DateTime<Utc>,
) -> Option<AvailableUpdate> {
    let latest = latest?;
    if latest.version <= *current {
        return None;
    }
    if now - latest.published_at < BAKE_PERIOD {
        return None;
    }
    let version = latest.version.to_string();
    if dismissed.and_then(parse_version).is_some_and(|d| d == latest.version) {
        return None;
    }
    Some(AvailableUpdate {
        release_url: crate::about::release_url(&version),
        published_at: latest.published_at.to_rfc3339(),
        version,
    })
}

/// How long to wait before the next check: the startup delay when no successful
/// check is on record or the last one is older than [`CHECK_INTERVAL`] (so a
/// long-closed app checks promptly), otherwise until that interval has elapsed
/// since the last check (so a `tauri dev` rebuild every few minutes doesn't hit
/// GitHub each time). Never less than the startup delay.
pub fn next_delay(checked_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Duration {
    let Some(checked_at) = checked_at else { return STARTUP_DELAY };
    let interval = chrono::Duration::from_std(CHECK_INTERVAL).expect("interval fits chrono");
    let due = checked_at + interval;
    if due <= now {
        return STARTUP_DELAY;
    }
    (due - now).to_std().unwrap_or(STARTUP_DELAY).max(STARTUP_DELAY)
}

// ── The check ───────────────────────────────────────────────────────────────

/// `GET /repos/<this repo>/releases/latest`. GitHub's `latest` already excludes
/// drafts and prereleases, which is exactly how `scripts/release.sh publish`
/// creates releases. A tag that isn't semver, or an unparsable `published_at`,
/// is an error (logged by the caller; the previous result stands).
pub async fn fetch_latest(gh: &GitHub) -> Result<LatestRelease, String> {
    let url = gh.api(&format!("/repos/{RELEASES_REPO}/releases/latest"));
    let body = gh.get_json(&url).await?;
    let tag = body["tag_name"].as_str().unwrap_or_default();
    let version = parse_version(tag).ok_or_else(|| format!("release tag {tag:?} is not semver"))?;
    let published = body["published_at"].as_str().unwrap_or_default();
    let published_at = parse_utc(published)
        .ok_or_else(|| format!("release published_at {published:?} is not RFC 3339"))?;
    Ok(LatestRelease { version, published_at })
}

/// Record a successful check: update the in-memory snapshot and persist it,
/// keeping `dismissed_version` (and every other setting) from disk.
pub fn record(snapshot: &Mutex<Snapshot>, latest: LatestRelease, now: DateTime<Utc>) {
    let persisted = UpdateCheckState {
        checked_at: Some(now.to_rfc3339()),
        latest_version: Some(latest.version.to_string()),
        latest_published_at: Some(latest.published_at.to_rfc3339()),
        dismissed_version: None, // merged from disk below
    };
    *snapshot.lock().unwrap_or_else(|e| e.into_inner()) =
        Snapshot { latest: Some(latest), checked_at: Some(now) };
    // `update` refuses to merge into an unreadable file (it would erase every
    // other setting), so this fails loudly instead when settings.json is broken.
    if let Err(e) = app_settings::update(|s| {
        let dismissed = s.update_check.as_ref().and_then(|u| u.dismissed_version.clone());
        s.update_check = Some(UpdateCheckState { dismissed_version: dismissed, ..persisted });
    }) {
        tracing::warn!(error = %e, "update check: failed to persist result");
    }
}

/// One scheduled check against the real API. Soft-fail: problems are logged and
/// the previous result stands.
async fn run_once(app: &tauri::AppHandle) {
    let gh = match GitHub::anonymous() {
        Ok(gh) => gh,
        Err(e) => {
            tracing::warn!(error = %e, "update check: could not build client");
            return;
        }
    };
    match fetch_latest(&gh).await {
        Ok(latest) => {
            tracing::info!(
                latest = %latest.version,
                current = %app.package_info().version,
                "update check: fetched latest release"
            );
            record(app.state::<Mutex<Snapshot>>().inner(), latest, Utc::now());
        }
        Err(e) => tracing::warn!(error = %e, "update check failed; keeping previous result"),
    }
}

/// Seed the managed snapshot from disk and start the polling task. Called once
/// from `setup()`; the task lives as long as the app.
pub fn start(app: tauri::AppHandle) {
    // Strict load, so an unreadable settings.json is *said* here rather than
    // silently treated as "never checked" (which would make the check run 15 s
    // after every launch and fail to persist each time — see `record`).
    let settings = app_settings::load_validated().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "update check: settings.json unreadable; starting from scratch and results won't persist until it is fixed");
        Default::default()
    });
    let snapshot = Snapshot::from_persisted(settings.update_check.as_ref());
    let first = next_delay(snapshot.checked_at, Utc::now());
    app.manage(Mutex::new(snapshot));
    tracing::debug!(first_check_in_secs = first.as_secs(), "update check scheduled");
    tauri::async_runtime::spawn(async move {
        let mut delay = first;
        loop {
            tokio::time::sleep(delay).await;
            run_once(&app).await;
            delay = CHECK_INTERVAL;
        }
    });
}

// ── Commands ────────────────────────────────────────────────────────────────

/// The current verdict for the popover. Polled on every popover show, hence
/// `debug`.
#[tauri::command]
pub fn update_check_status(
    app: tauri::AppHandle,
    snapshot: tauri::State<'_, Mutex<Snapshot>>,
) -> UpdateStatus {
    crate::log_invoke_debug!("update_check_status");
    let snap = snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let dismissed = app_settings::load().update_check.and_then(|u| u.dismissed_version);
    UpdateStatus {
        available: verdict(
            &app.package_info().version,
            snap.latest.as_ref(),
            dismissed.as_deref(),
            Utc::now(),
        ),
        checked_at: snap.checked_at.map(|t| t.to_rfc3339()),
    }
}

/// Remember that the user dismissed the banner for `version`; it stays hidden
/// for that version and returns for any newer one.
#[tauri::command]
pub fn update_dismiss(version: String) -> Result<(), String> {
    crate::log_invoke!("update_dismiss", version = %version);
    app_settings::update(|s| {
        s.update_check.get_or_insert_with(Default::default).dismissed_version = Some(version.clone());
    })
    .map_err(|e| format!("Couldn't save the dismissed version: {e}"))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempHome;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn at(s: &str) -> DateTime<Utc> {
        parse_utc(s).unwrap()
    }

    fn release(version: &str, published_at: &str) -> LatestRelease {
        LatestRelease { version: v(version), published_at: at(published_at) }
    }

    const NOW: &str = "2026-09-24T12:00:00Z";

    #[test]
    fn verdict_offers_a_baked_newer_release() {
        let latest = release("0.4.0", "2026-09-10T00:00:00Z");
        let got = verdict(&v("0.3.2"), Some(&latest), None, at(NOW)).unwrap();
        assert_eq!(got.version, "0.4.0");
        assert_eq!(got.release_url, "https://github.com/yanokamay-org/maiestro/releases/tag/v0.4.0");
        assert_eq!(got.published_at, "2026-09-10T00:00:00+00:00");
    }

    #[test]
    fn verdict_is_none_without_a_result_or_when_current_is_not_older() {
        assert!(verdict(&v("0.3.2"), None, None, at(NOW)).is_none());
        let same = release("0.3.2", "2026-01-01T00:00:00Z");
        assert!(verdict(&v("0.3.2"), Some(&same), None, at(NOW)).is_none());
        // A dev build can sit *ahead* of the latest release.
        let older = release("0.3.1", "2026-01-01T00:00:00Z");
        assert!(verdict(&v("0.3.2"), Some(&older), None, at(NOW)).is_none());
    }

    #[test]
    fn verdict_waits_out_the_bake_period() {
        // Published exactly 5 days ago → offered; one second less → not yet.
        let boundary = release("0.4.0", "2026-09-19T12:00:00Z");
        assert!(verdict(&v("0.3.2"), Some(&boundary), None, at(NOW)).is_some());
        let fresh = release("0.4.0", "2026-09-19T12:00:01Z");
        assert!(verdict(&v("0.3.2"), Some(&fresh), None, at(NOW)).is_none());
    }

    #[test]
    fn verdict_honours_a_dismissal_for_that_version_only() {
        let latest = release("0.4.0", "2026-09-01T00:00:00Z");
        assert!(verdict(&v("0.3.2"), Some(&latest), Some("0.4.0"), at(NOW)).is_none());
        // Dismissed with the tag's `v` (a hand edit) still matches.
        assert!(verdict(&v("0.3.2"), Some(&latest), Some("v0.4.0"), at(NOW)).is_none());
        // Dismissing an older version does not hide a newer one.
        assert!(verdict(&v("0.3.2"), Some(&latest), Some("0.3.3"), at(NOW)).is_some());
        assert!(verdict(&v("0.3.2"), Some(&latest), Some("garbage"), at(NOW)).is_some());
    }

    #[test]
    fn next_delay_checks_promptly_when_stale_and_waits_when_fresh() {
        let now = at(NOW);
        assert_eq!(next_delay(None, now), STARTUP_DELAY);
        // Older than the interval → prompt.
        assert_eq!(next_delay(Some(at("2026-09-23T23:59:00Z")), now), STARTUP_DELAY);
        // One hour ago → eleven hours to go.
        assert_eq!(
            next_delay(Some(at("2026-09-24T11:00:00Z")), now),
            Duration::from_secs(11 * 60 * 60)
        );
        // Just checked (clock skew in the future too) → never below the startup delay.
        assert_eq!(next_delay(Some(at("2026-09-24T11:59:59Z")), now), CHECK_INTERVAL - Duration::from_secs(1));
        assert_eq!(next_delay(Some(at("2026-09-24T12:00:00Z")), now), CHECK_INTERVAL);
    }

    #[test]
    fn snapshot_round_trips_and_tolerates_bad_values() {
        let good = UpdateCheckState {
            checked_at: Some("2026-09-24T14:02:11Z".into()),
            latest_version: Some("0.4.0".into()),
            latest_published_at: Some("2026-09-10T18:30:00Z".into()),
            dismissed_version: Some("0.4.0".into()),
        };
        let snap = Snapshot::from_persisted(Some(&good));
        assert_eq!(snap.checked_at, Some(at("2026-09-24T14:02:11Z")));
        assert_eq!(snap.latest, Some(release("0.4.0", "2026-09-10T18:30:00Z")));

        assert_eq!(Snapshot::from_persisted(None), Snapshot::default());
        let bad = UpdateCheckState {
            checked_at: Some("yesterday".into()),
            latest_version: Some("0.4.0".into()),
            latest_published_at: None,
            dismissed_version: None,
        };
        // Each part degrades independently: no latest without both halves.
        assert_eq!(Snapshot::from_persisted(Some(&bad)), Snapshot::default());
    }

    #[test]
    fn record_persists_the_result_and_keeps_the_dismissal() {
        let _home = TempHome::new();
        app_settings::update(|s| {
            s.theme = Some(app_settings::Theme::Dark);
            s.update_check = Some(UpdateCheckState {
                dismissed_version: Some("0.3.9".into()),
                ..Default::default()
            });
        })
        .unwrap();

        let snapshot = Mutex::new(Snapshot::default());
        record(&snapshot, release("0.4.0", "2026-09-10T18:30:00Z"), at(NOW));

        let snap = snapshot.lock().unwrap().clone();
        assert_eq!(snap.checked_at, Some(at(NOW)));
        assert_eq!(snap.latest.unwrap().version, v("0.4.0"));

        let saved = app_settings::load();
        assert_eq!(saved.theme, Some(app_settings::Theme::Dark), "other settings untouched");
        let uc = saved.update_check.unwrap();
        assert_eq!(uc.dismissed_version.as_deref(), Some("0.3.9"));
        assert_eq!(uc.latest_version.as_deref(), Some("0.4.0"));
        assert_eq!(uc.checked_at.as_deref(), Some("2026-09-24T12:00:00+00:00"));
        assert_eq!(uc.latest_published_at.as_deref(), Some("2026-09-10T18:30:00+00:00"));
    }

    #[test]
    fn dismiss_writes_only_the_dismissed_version() {
        let _home = TempHome::new();
        app_settings::update(|s| {
            s.update_check = Some(UpdateCheckState {
                checked_at: Some("2026-09-24T14:02:11Z".into()),
                ..Default::default()
            });
        })
        .unwrap();
        update_dismiss("0.4.0".into()).unwrap();
        let uc = app_settings::load().update_check.unwrap();
        assert_eq!(uc.dismissed_version.as_deref(), Some("0.4.0"));
        assert_eq!(uc.checked_at.as_deref(), Some("2026-09-24T14:02:11Z"));
    }

    // ── wiremock: the request itself ────────────────────────────────────────

    #[tokio::test]
    async fn fetch_latest_parses_the_release() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/yanokamay-org/maiestro/releases/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "tag_name": "v0.4.0",
                "published_at": "2026-09-10T18:30:00Z",
                "html_url": "https://github.com/yanokamay-org/maiestro/releases/tag/v0.4.0"
            })))
            .mount(&server)
            .await;
        let got = fetch_latest(&GitHub::for_test_anonymous(&server.uri())).await.unwrap();
        assert_eq!(got, release("0.4.0", "2026-09-10T18:30:00Z"));
        let sent = server.received_requests().await.unwrap();
        assert!(!sent[0].headers.contains_key("authorization"), "update check must be anonymous");
    }

    #[tokio::test]
    async fn fetch_latest_rejects_bad_tags_and_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/yanokamay-org/maiestro/releases/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "tag_name": "nightly",
                "published_at": "2026-09-10T18:30:00Z"
            })))
            .mount(&server)
            .await;
        let err = fetch_latest(&GitHub::for_test_anonymous(&server.uri())).await.unwrap_err();
        assert!(err.contains("not semver"), "{err}");

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "message": "Not Found" })))
            .mount(&server)
            .await;
        let err = fetch_latest(&GitHub::for_test_anonymous(&server.uri())).await.unwrap_err();
        assert!(err.contains("404") || err.contains("Not Found"), "{err}");
    }
}

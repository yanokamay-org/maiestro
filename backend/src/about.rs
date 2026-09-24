//! Read-only "About" information for the Settings window's Preferences panel
//! (issue #126): which version of mAIestro Code is running, when it was built, and
//! where to read that version's release notes.
//!
//! None of this is user configuration — it is deliberately *not* a field in
//! `~/.maiestro/settings.json` or its schema (the `schema_matches_struct` drift
//! guard would reject it, and the Preferences form's autosave would try to write
//! it back). The version is single-sourced from `backend/tauri.conf.json` via
//! Tauri's `PackageInfo`, so it always matches what `scripts/release.sh bump`
//! writes; the build metadata is stamped in by `build.rs`.

use serde::Serialize;

/// The GitHub repo the release notes link points at.
const REPO_URL: &str = "https://github.com/yanokamay-org/maiestro";

/// Version and build metadata for the running app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppVersion {
    /// Semver from `tauri.conf.json`, e.g. `0.2.3`. On a dev build this is the
    /// *last released* version the build sits on top of, not a release of its own
    /// — see `dev_build`.
    pub version: String,
    /// UTC date the binary was built, `YYYY-MM-DD`.
    pub build_date: String,
    /// Short git SHA of the checkout it was built from, when git was available.
    pub commit: Option<String>,
    /// GitHub Release page for this version's tag.
    pub release_url: String,
    /// True unless this binary came out of `scripts/release.sh build` — i.e. a
    /// local build made *after* the `version` release, not that release itself.
    /// The UI labels it `X.Y.Z+dev` so the two are never confused.
    pub dev_build: bool,
}

/// The GitHub Release page for `version`'s tag (`vX.Y.Z`).
pub(crate) fn release_url(version: &str) -> String {
    format!("{REPO_URL}/releases/tag/v{version}")
}

#[tauri::command]
pub fn app_version(app: tauri::AppHandle) -> AppVersion {
    crate::log_invoke_debug!("app_version");
    let version = app.package_info().version.to_string();
    AppVersion {
        build_date: env!("MAIESTRO_BUILD_DATE").to_string(),
        commit: option_env!("MAIESTRO_GIT_SHA").map(str::to_string),
        release_url: release_url(&version),
        dev_build: option_env!("MAIESTRO_RELEASE_BUILD").is_none(),
        version,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_url_points_at_the_version_tag() {
        assert_eq!(
            release_url("0.2.3"),
            "https://github.com/yanokamay-org/maiestro/releases/tag/v0.2.3"
        );
    }

    /// The About row renders the stamped date as-is, so a malformed stamp would
    /// show up in the UI. Assert `build.rs` produced a `YYYY-MM-DD` value.
    #[test]
    fn build_date_is_an_iso_date() {
        let date = env!("MAIESTRO_BUILD_DATE");
        let parts: Vec<&str> = date.split('-').collect();
        assert_eq!(parts.len(), 3, "expected YYYY-MM-DD, got {date:?}");
        assert_eq!(
            (parts[0].len(), parts[1].len(), parts[2].len()),
            (4, 2, 2),
            "expected YYYY-MM-DD, got {date:?}"
        );
        assert!(
            parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())),
            "expected YYYY-MM-DD, got {date:?}"
        );
    }
}

//! Global, app-wide settings stored at `~/.maiestro/settings.json`.
//!
//! Sibling to `profiles.json` and the per-repo `repos/<…>.json` files, but holds
//! configuration that is neither a credential nor repo-scoped. Today that is the
//! popover and Settings window persisted sizes (issue #40), the UI theme (issue #12),
//! per-tool CLI path overrides (issue #85), the spawned-worktree terminal font,
//! and the onboarding / launch-at-login state (issue #98); the file is intentionally
//! human-editable and dotfile-manageable. A missing or partial file is fine —
//! every field is optional and defaults to "not set".
//!
//! Like per-repo settings, the file format has a hand-written JSON Schema
//! (`backend/schemas/app-settings.schema.json`) that is the spec; the structs
//! below must conform to it (the `schema_matches_struct` test enforces this).
//! The `app_settings_schema` command returns it to the Settings window's JSON
//! Forms renderer.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::utils::config::Color;
use tauri_plugin_autostart::ManagerExt;

/// Opaque native window-background colors matching `styles.css`'s `--bg`
/// tokens (dark `rgba(28, 24, 38, 0.97)`, light `rgba(247, 246, 250, 0.98)`),
/// used only to pre-color the onboarding window before `show()` — see
/// `maybe_show_onboarding`.
const DARK_BG: Color = Color(28, 24, 38, 255);
const LIGHT_BG: Color = Color(247, 246, 250, 255);

/// Persisted size of a window, in logical pixels. Restored on launch before the
/// window is first shown, saved when the window hides (popover on blur, the
/// Settings window when it loses focus or is closed).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WindowSize {
    pub width: f64,
    pub height: f64,
}

/// The UI appearance the user picked. `System` follows the macOS dark/light
/// setting (resolved in the frontend via `prefers-color-scheme`). Absent in the
/// settings file means `System`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
    #[default]
    System,
}

/// Explicit filesystem paths for the external CLIs mAIestro invokes directly.
/// Each field `None`/empty means "auto-resolve" (see `crate::tools`). Set one to
/// pin a specific binary — useful when the app, launched from `/Applications`
/// with a minimal `$PATH`, can't find a tool (issue #85).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPaths {
    /// Path to the `claude` CLI (AI drafting). `None`/empty = auto-resolve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<String>,
    /// Path to `git` (worktree add, branch checks). `None`/empty = auto-resolve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<String>,
    /// Path to the VS Code `code` CLI (open worktrees). `None`/empty = auto-resolve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppSettings {
    /// Saved popover size. `None` (absent) means "use the tauri.conf.json default".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowSize>,
    /// Saved Settings-window size. `None` (absent) means "use the tauri.conf.json default".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_window: Option<WindowSize>,
    /// Chosen UI theme. `None` (absent) means `System`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<Theme>,
    /// Per-tool CLI path overrides. `None` (absent) means all tools auto-resolve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_paths: Option<ToolPaths>,
    /// Font stack for a spawned worktree's VS Code integrated terminal.
    /// `None`/empty means the schema `default`. See [`terminal_font_family`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_font_family: Option<String>,
    /// Whether to launch mAIestro automatically at login via a per-user
    /// LaunchAgent (issue #98). `None` (absent) means `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_at_login: Option<bool>,
    /// Whether the one-time onboarding flow has been completed. Machine-managed
    /// (set once when the onboarding dialog is dismissed), hidden from the Settings
    /// form. Gates the first-run onboarding window; `None`/`false` = not yet
    /// onboarded. Kept as its own flag (not derived from another preference) so
    /// onboarding can grow more options without changing the gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onboarding_completed: Option<bool>,
}

/// The user's explicit path override for a directly-invoked tool
/// (`claude`/`git`/`code`), if set and non-empty. Read by `crate::tools`. An
/// unknown tool name or an empty/whitespace value yields `None` (auto-resolve).
pub fn tool_path_override(name: &str) -> Option<String> {
    let tp = load().tool_paths?;
    let v = match name {
        "claude" => tp.claude,
        "git" => tp.git,
        "code" => tp.code,
        _ => None,
    };
    v.filter(|s| !s.trim().is_empty())
}

/// The font stack to write as `terminal.integrated.fontFamily` into a spawned
/// worktree's `.vscode/settings.json`. The user's setting when they've set a
/// non-empty one, otherwise the schema's `default` — which is the single source
/// of truth for it, so neither this module nor the Settings form hardcodes a copy.
pub fn terminal_font_family() -> String {
    load()
        .terminal_font_family
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| schema_default("/properties/terminal_font_family/default"))
}

// ── Schema (hand-written spec; the structs must conform to it) ──────────────

/// The canonical schema for `~/.maiestro/settings.json`. Hand-written and
/// checked in — *not* generated from the struct. The `schema_matches_struct`
/// test fails the build if they drift apart. Embedded so validation and the
/// `app_settings_schema` command need no file at runtime.
const SCHEMA_JSON: &str = include_str!("../schemas/app-settings.schema.json");

fn schema_value() -> serde_json::Value {
    crate::schema::parse(SCHEMA_JSON, "app-settings")
}

/// A string `default` from the embedded schema, addressed by JSON Pointer.
fn schema_default(pointer: &str) -> String {
    crate::schema::default_str(&schema_value(), pointer)
}

/// Validate a settings JSON value against the embedded schema, naming the failing
/// field(s) on error.
fn validate_against_schema(value: &serde_json::Value) -> Result<(), String> {
    crate::schema::validate(&schema_value(), value)
}

// ── Storage ─────────────────────────────────────────────────────────────────

fn settings_path() -> PathBuf {
    crate::paths::maiestro_dir("settings.json")
}

/// Read the global settings, returning defaults if the file is absent or unreadable.
pub fn load() -> AppSettings {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|data| serde_json::from_str(&data).ok())
        .unwrap_or_default()
}

/// Read and validate the global settings. Unlike the lenient `load()` (which
/// defaults on any problem, for hot-path reads), this returns a loud error when
/// the file is present but invalid — so the Settings window can surface a banner
/// instead of a form full of defaults that would clobber the file on save.
/// Missing file → defaults.
pub fn load_validated() -> Result<AppSettings, String> {
    let path = settings_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(AppSettings::default()),
        Err(e) => return Err(format!("Failed to read {}: {e}", path.display())),
    };
    let label = path.display().to_string();
    let value: serde_json::Value =
        serde_json::from_str(&data).map_err(|e| format!("{label} is not valid JSON: {e}"))?;
    validate_against_schema(&value).map_err(|msg| format!("{label} failed validation — {msg}"))?;
    serde_json::from_value(value).map_err(|e| format!("{label} does not match AppSettings: {e}"))
}

/// Write the global settings, creating `~/.maiestro/` if needed. Atomic
/// (temp+rename) so a crash mid-write can't truncate the file. Prefer
/// [`update`] over a bare `load` + `save`, so concurrent read-modify-writes
/// don't lose each other's changes.
pub fn save(settings: &AppSettings) -> std::io::Result<()> {
    let path = settings_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let data = serde_json::to_string_pretty(settings).unwrap();
    crate::paths::write_atomic(&path, data.as_bytes())
}

/// Serializes read-modify-write cycles on `settings.json`. The popover-size and
/// Settings-window-size persisters and the Preferences form all load-merge-save
/// this file; without a shared lock two of them interleaving would lose one
/// update (last writer wins on the *whole* file, not the field). See [`update`].
static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

/// Read-modify-write the settings file under [`SETTINGS_LOCK`]: load the current
/// on-disk settings, apply `f`, and save — all while holding the lock, so a
/// concurrent persister can't clobber the field `f` just changed. Use this for
/// every partial update (window sizes, theme, tool paths) instead of a bare
/// `load()` + mutate + `save()`.
pub fn update<F: FnOnce(&mut AppSettings)>(f: F) -> std::io::Result<()> {
    let _guard = SETTINGS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut settings = load();
    f(&mut settings);
    save(&settings)
}

// ── Commands ──────────────────────────────────────────────────────────────

/// Read the persisted UI theme (`System` when unset).
#[tauri::command]
pub fn app_settings_get_theme() -> Theme {
    crate::log_invoke_debug!("app_settings_get_theme");
    load().theme.unwrap_or_default()
}

/// The hand-written JSON Schema for the global settings file, for the Settings
/// window's JSON Forms renderer.
#[tauri::command]
pub fn app_settings_schema() -> serde_json::Value {
    crate::log_invoke_debug!("app_settings_schema");
    schema_value()
}

/// Read the full, validated global settings for the Settings form. Fallible so a
/// corrupt file surfaces as a banner rather than silent defaults.
#[tauri::command]
pub fn app_settings_get() -> Result<AppSettings, String> {
    crate::log_invoke_debug!("app_settings_get");
    load_validated()
}

/// Persist the settings edited in the Settings form. The form owns only the
/// user-editable fields (`theme`, `tool_paths`, `terminal_font_family`,
/// `launch_at_login`); the machine-managed window sizes
/// are load-merged from disk so a concurrent size save from another window isn't
/// clobbered. Validates before writing, and broadcasts `theme-changed` when the
/// theme actually changed so every window re-applies live.
#[tauri::command]
pub fn app_settings_set(app: tauri::AppHandle, settings: AppSettings) -> Result<(), String> {
    crate::log_invoke!("app_settings_set", theme = ?settings.theme);
    // Defense in depth: never persist a value the schema would reject on reload.
    let value = serde_json::to_value(&settings).map_err(|e| e.to_string())?;
    validate_against_schema(&value).map_err(|msg| format!("Invalid settings — {msg}"))?;

    // Apply the OS-level launch-at-login change *before* persisting, so a failed
    // enable/disable returns an error (surfaced as a form banner) and the stored
    // pref stays consistent with reality; startup reconcile retries next launch.
    let want_launch = settings.launch_at_login.unwrap_or(false);
    if load().launch_at_login.unwrap_or(false) != want_launch {
        set_autolaunch(&app, want_launch)?;
    }

    // Load-merge under the shared lock so a concurrent window-size persister
    // (which also load-merges) can't drop the theme/tool-paths change or vice versa.
    let mut theme_changed = false;
    update(|current| {
        theme_changed = current.theme != settings.theme;
        current.theme = settings.theme;
        current.tool_paths = settings.tool_paths.clone();
        current.terminal_font_family = settings.terminal_font_family.clone();
        current.launch_at_login = settings.launch_at_login;
        // onboarding_completed / window / settings_window are deliberately kept
        // from disk (machine-managed).
    })
    .map_err(|e| e.to_string())?;

    if theme_changed {
        use tauri::Emitter;
        let _ = app.emit("theme-changed", settings.theme.unwrap_or_default());
    }
    Ok(())
}

// ── Onboarding + launch at login (issue #98) ────────────────────────────────

/// Register (or remove) the per-user LaunchAgent so the app starts at login.
/// `enable()` overwrites the plist with the current binary path (idempotent for a
/// stable `/Applications` install; it also refreshes a stale `current_exe()` baked
/// by a `tauri dev` build). `disable()` is only called when a plist actually
/// exists, so removing when already-off is a no-op rather than a "file not found".
fn set_autolaunch(app: &tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let mgr = app.autolaunch();
    if enabled {
        mgr.enable()
            .map_err(|e| format!("Couldn't register launch-at-login: {e}"))
    } else if mgr.is_enabled().unwrap_or(false) {
        mgr.disable()
            .map_err(|e| format!("Couldn't remove launch-at-login: {e}"))
    } else {
        Ok(())
    }
}

/// Bring the LaunchAgent into agreement with the stored preference on startup.
/// Called from `setup()`. Never fails the app — a problem is logged, not fatal.
/// When the pref is on, this also heals a stale baked binary path (see
/// `set_autolaunch`).
pub fn reconcile_launch_at_login(app: &tauri::AppHandle) {
    let want = load().launch_at_login.unwrap_or(false);
    match set_autolaunch(app, want) {
        Ok(()) => tracing::debug!(launch_at_login = want, "reconciled launch-at-login"),
        Err(e) => tracing::warn!(error = %e, "failed to reconcile launch-at-login"),
    }
}

/// One-time onboarding: the very first time the app runs (before
/// `onboarding_completed` is set), show the branded `onboarding` window. It's a
/// real webview window (not a native alert) so it can display the mAIestro logo
/// and name and grow more options over time — a native dialog can only show the
/// generic OS icon and a fixed button set. The user's choices arrive via
/// `onboarding_complete` (the "Get started" button) or `complete_onboarding`
/// (window closed = accept defaults), either of which flips `onboarding_completed`
/// true so this never reappears. Called from `setup()` after
/// `reconcile_launch_at_login`.
pub fn maybe_show_onboarding(app: &tauri::AppHandle) {
    if load().onboarding_completed.unwrap_or(false) {
        return;
    }
    use tauri::Manager;
    match app.get_webview_window("onboarding") {
        Some(window) => {
            // A window created hidden (`visible: false` in tauri.conf.json) has
            // no painted content yet, so the instant it appears on screen it
            // shows its *native* window/webview background — white by default —
            // until our CSS finishes loading a frame or two later. That's the
            // flash: not a theme mismatch, but nothing themed painted yet at
            // all. `set_background_color` fixes the actual visible symptom by
            // giving the window a themed backing color before `show()`, so
            // there's nothing default-colored to flash. `set_theme` is kept
            // alongside it for native chrome (scrollbars, form controls) and to
            // make `window.theme()` itself reliable. Both read the *native*
            // NSWindow appearance (`window.theme()`, correct even while
            // hidden) rather than the app's `theme` setting, because onboarding
            // only ever runs before the user could have set an explicit
            // Light/Dark preference.
            if let Ok(theme) = window.theme() {
                let _ = window.set_theme(Some(theme));
                let bg = if theme == tauri::Theme::Light {
                    LIGHT_BG
                } else {
                    DARK_BG
                };
                let _ = window.set_background_color(Some(bg));
            }
            let _ = window.show();
            let _ = window.set_focus();
        }
        None => tracing::warn!("onboarding window missing; skipping onboarding"),
    }
}

/// Apply and persist the user's onboarding choices, marking onboarding complete so
/// it never runs again — regardless of what was chosen. Today the only choice is
/// launch-at-login; new options extend the parameters here and in
/// `onboarding_complete`. Failure-tolerant: a failed LaunchAgent write is logged
/// but onboarding is still marked complete, so we don't re-run it on every launch
/// (the Preferences panel remains the way to change any setting afterwards). Ends
/// by popping open the menu-bar popover (`main.rs::show_popover`), so the user
/// lands somewhere useful the moment the onboarding window goes away, instead of
/// finishing onto an empty desktop. Called exactly once per real completion (see
/// callers), so this never double-shows the popover.
pub fn complete_onboarding(app: &tauri::AppHandle, launch_at_login: bool) {
    if let Err(e) = set_autolaunch(app, launch_at_login) {
        tracing::warn!(error = %e, "onboarding launch-at-login toggle failed");
    }
    // Load-merge under the shared lock (like app_settings_set), so a concurrent
    // window-size persist can't clobber these fields or vice versa.
    if let Err(e) = update(|s| {
        s.launch_at_login = Some(launch_at_login);
        s.onboarding_completed = Some(true);
    }) {
        tracing::error!(error = %e, "failed to persist onboarding choices");
    }
    crate::show_popover(app);
}

/// Finish onboarding from the dialog's "Get started" button, then dismiss the
/// window. `destroy()` (not `close()`) so it bypasses the CloseRequested handler —
/// onboarding is already recorded, so the "closed = defaults" path must not also
/// fire. Marks onboarding complete either way.
#[tauri::command]
pub fn onboarding_complete(app: tauri::AppHandle, launch_at_login: bool) {
    crate::log_invoke!("onboarding_complete", launch_at_login = launch_at_login);
    complete_onboarding(&app, launch_at_login);
    use tauri::Manager;
    if let Some(window) = app.get_webview_window("onboarding") {
        let _ = window.destroy();
    }
}

/// Handle the user closing the onboarding window (the title-bar close button)
/// without pressing "Get started": accept the defaults (launch-at-login off) and
/// mark onboarding complete so it doesn't reappear. Guarded on the flag so it's a
/// no-op when the button already recorded the choices (that path uses `destroy()`,
/// which skips this, but the guard is belt-and-suspenders). Called from
/// `main.rs`'s window-event handler.
pub fn complete_onboarding_if_pending(app: &tauri::AppHandle) {
    if load().onboarding_completed.unwrap_or(false) {
        return;
    }
    complete_onboarding(app, false);
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    /// The embedded schema string must be valid JSON, since `schema_value`
    /// `.expect()`s it at runtime.
    #[test]
    fn schema_parses() {
        let v = schema_value();
        assert!(v.get("properties").is_some(), "schema must declare properties");
    }

    /// Drift guard: the hand-written schema and the Rust structs must describe the
    /// same set of top-level fields, and both a default and a fully-populated
    /// `AppSettings` must validate. Adding a field to one without the other fails
    /// here. The key-set check uses the fully-populated instance because every
    /// field is `skip_serializing_if = "Option::is_none"`, so `default` serializes
    /// to `{}`.
    #[test]
    fn schema_matches_struct() {
        let schema = schema_value();
        let schema_props: BTreeSet<String> = schema["properties"]
            .as_object()
            .expect("schema.properties is an object")
            .keys()
            .cloned()
            .collect();

        let populated = AppSettings {
            window: Some(WindowSize { width: 680.0, height: 460.0 }),
            settings_window: Some(WindowSize { width: 720.0, height: 520.0 }),
            theme: Some(Theme::Dark),
            tool_paths: Some(ToolPaths {
                claude: Some("/opt/homebrew/bin/claude".into()),
                git: Some("/opt/homebrew/bin/git".into()),
                code: Some("/usr/local/bin/code".into()),
            }),
            terminal_font_family: Some("Menlo, monospace".into()),
            launch_at_login: Some(true),
            onboarding_completed: Some(true),
        };
        let value = serde_json::to_value(&populated).unwrap();
        let struct_keys: BTreeSet<String> =
            value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            schema_props, struct_keys,
            "schema properties and serialized AppSettings fields drifted apart"
        );
        validate_against_schema(&value)
            .unwrap_or_else(|e| panic!("populated AppSettings rejected by schema: {e}"));

        // A default (empty) instance validates too.
        let default = serde_json::to_value(AppSettings::default()).unwrap();
        validate_against_schema(&default)
            .unwrap_or_else(|e| panic!("default AppSettings rejected by schema: {e}"));
    }

    /// A partial file (only `theme`) validates — every field is optional.
    #[test]
    fn partial_file_validates() {
        validate_against_schema(&json!({ "theme": "light" })).expect("partial file should validate");
        validate_against_schema(&json!({ "tool_paths": { "claude": "/x/claude" } }))
            .expect("tool_paths-only file should validate");
    }

    /// A bad enum value or wrong type is rejected, naming the field.
    #[test]
    fn invalid_values_rejected() {
        assert!(validate_against_schema(&json!({ "theme": "chartreuse" })).is_err());
        assert!(validate_against_schema(&json!({ "tool_paths": { "git": 42 } })).is_err());
    }

    /// Unknown fields are tolerated (forward compat + hand-edited `$schema`).
    #[test]
    fn unknown_fields_tolerated() {
        validate_against_schema(&json!({
            "$schema": "./app-settings.schema.json",
            "theme": "system",
            "future_field": true
        }))
        .expect("unknown fields should be tolerated");
    }

    /// An empty/whitespace override reads as "auto-resolve" (None).
    #[test]
    fn blank_override_is_none() {
        let tp = ToolPaths { claude: Some("  ".into()), git: Some("".into()), code: None };
        // Exercise the same filter `tool_path_override` applies.
        assert!(tp.claude.as_deref().filter(|s| !s.trim().is_empty()).is_none());
        assert!(tp.git.as_deref().filter(|s| !s.trim().is_empty()).is_none());
    }
}

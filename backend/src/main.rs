// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod agent;
mod app_settings;
mod credentials;
mod drafting;
mod editor;
mod gitops;
mod health;
mod hooks;
mod identities;
mod links;
mod logging;
mod naming;
mod paths;
mod popover_placement;
mod plugin;
mod plugins;
mod pr;
mod prompts;
mod repo_context;
mod repo_settings;
mod schema;
mod sessions;
mod spawn;
mod status;
#[cfg(test)]
mod testutil;
mod theming;
mod tools;
mod update_check;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, LogicalPosition, LogicalSize, Manager, WindowEvent,
};

use popover_placement::{Display, Rect as PlacementRect};
use plugin::{PluginRegistry, plugins_list_credential_types};
use plugins::{GitHubPlugin, github_get_repo, github_list_issues, github_list_repos};

/// Records when the popover was auto-hidden on blur. A tray click that *caused*
/// that blur (clicking the icon while the window is open) lands here within a few
/// milliseconds, so we treat it as "close" instead of immediately reopening.
#[derive(Default)]
struct PopoverState {
    last_auto_hide: Mutex<Option<Instant>>,
    /// The tray icon's rect, cached from tray events so we can position the
    /// popover analytically without reading window geometry back (which lags a
    /// cycle on macOS). `None` until the first tray event.
    tray: Mutex<Option<TrayCapture>>,
}

/// A tray rect as tray-icon reports it — physical px at the clicked display's
/// (unknown) scale — plus the pointer in points sampled at the same event,
/// which was over the icon and so identifies its display unambiguously.
#[derive(Clone, Copy)]
struct TrayCapture {
    rect: PlacementRect,
    cursor: Option<(f64, f64)>,
}

/// Minimum popover size, in logical pixels. Mirrors `minWidth`/`minHeight` on the
/// `main` window in `tauri.conf.json`; used to clamp a restored size so a hand-edited
/// or stale `settings.json` can't shrink the popover into an unusable sliver.
const MIN_POPOVER_WIDTH: f64 = 480.0;
const MIN_POPOVER_HEIGHT: f64 = 360.0;

/// Minimum Settings-window size, in logical pixels. Mirrors `minWidth`/`minHeight`
/// on the `settings` window in `tauri.conf.json`; clamps a restored size so a
/// stale or hand-edited `settings.json` can't shrink it below usable.
const MIN_SETTINGS_WIDTH: f64 = 560.0;
const MIN_SETTINGS_HEIGHT: f64 = 400.0;

/// Apply the persisted popover size (if any) to the `main` window. Called in
/// `setup()` while the window is still hidden, so the first show already has the
/// user's chosen dimensions — no resize flash.
fn restore_popover_size(app: &tauri::AppHandle) {
    let Some(size) = app_settings::load().window else {
        return;
    };
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let width = size.width.max(MIN_POPOVER_WIDTH);
    let height = size.height.max(MIN_POPOVER_HEIGHT);
    if let Err(e) = window.set_size(LogicalSize::new(width, height)) {
        tracing::warn!(error = %e, "failed to restore popover size");
    }
}

/// Save the popover's current logical size to the global settings file. Called
/// from the blur handler (the popover always hides on blur), so it captures the
/// final size after a resize drag without writing on every drag frame.
fn persist_popover_size(window: &tauri::Window) {
    let Ok(physical) = window.inner_size() else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let logical = physical.to_logical::<f64>(scale);
    // Load-merge under the shared lock so we don't clobber other fields (e.g. the
    // chosen theme) nor race a concurrent settings write.
    let result = app_settings::update(|settings| {
        settings.window = Some(app_settings::WindowSize {
            width: logical.width,
            height: logical.height,
        });
    });
    if let Err(e) = result {
        tracing::warn!(error = %e, "failed to persist popover size");
    } else {
        tracing::debug!(
            width = logical.width,
            height = logical.height,
            "persisted popover size"
        );
    }
}

/// Apply the persisted Settings-window size (if any) to the `settings` window.
/// Called in `setup()` while the window is still hidden, so it opens at the
/// user's chosen dimensions with no resize flash on first show.
fn restore_settings_size(app: &tauri::AppHandle) {
    let Some(size) = app_settings::load().settings_window else {
        return;
    };
    let Some(window) = app.get_webview_window("settings") else {
        return;
    };
    let width = size.width.max(MIN_SETTINGS_WIDTH);
    let height = size.height.max(MIN_SETTINGS_HEIGHT);
    if let Err(e) = window.set_size(LogicalSize::new(width, height)) {
        tracing::warn!(error = %e, "failed to restore settings size");
    }
}

/// Save the Settings window's current logical size to the global settings file.
/// The Settings window stays open on blur (unlike the popover), so we capture its
/// size when it loses focus or is closed — both land after a resize drag finishes.
fn persist_settings_size(window: &tauri::Window) {
    let Ok(physical) = window.inner_size() else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let logical = physical.to_logical::<f64>(scale);
    // Load-merge under the shared lock so we don't clobber other fields (popover
    // size, theme) nor race a concurrent settings write.
    let result = app_settings::update(|settings| {
        settings.settings_window = Some(app_settings::WindowSize {
            width: logical.width,
            height: logical.height,
        });
    });
    if let Err(e) = result {
        tracing::warn!(error = %e, "failed to persist settings size");
    } else {
        tracing::debug!(
            width = logical.width,
            height = logical.height,
            "persisted settings size"
        );
    }
}

/// Snapshot every display in macOS points (see `popover_placement`). tao
/// reports monitor bounds as points × that monitor's own scale, so dividing by
/// it recovers the exact point rect.
fn displays(window: &tauri::WebviewWindow) -> Vec<Display> {
    window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let scale = m.scale_factor();
            let (p, s) = (m.position(), m.size());
            Display {
                bounds: PlacementRect {
                    x: p.x as f64 / scale,
                    y: p.y as f64 / scale,
                    w: s.width as f64 / scale,
                    h: s.height as f64 / scale,
                },
                scale,
            }
        })
        .collect()
}

/// The mouse pointer in macOS points. tao converts `NSEvent.mouseLocation` to
/// physical px with the *primary* monitor's scale, so divide by that to undo it.
fn cursor_points(app: &tauri::AppHandle) -> Option<(f64, f64)> {
    let cursor = app.cursor_position().ok()?;
    let scale = app.primary_monitor().ok().flatten()?.scale_factor();
    Some((cursor.x / scale, cursor.y / scale))
}

/// Size and position the popover under the tray icon, on the tray's display,
/// fully on-screen — all before `show()`, so it never flashes at the wrong spot.
/// The math lives in `popover_placement` and runs entirely in macOS points, then
/// is applied as `LogicalSize`/`LogicalPosition`: tao's physical setters divide
/// by the window's *current* scale, which put the popover on the wrong display
/// in mixed-DPI setups (#175). It's computed analytically rather than read back
/// from the live window, whose geometry lags a cycle on macOS.
///
/// With no cached tray rect yet (e.g. onboarding finishing, or a single-instance
/// relaunch, before the user has ever hovered/clicked the tray icon) the popover
/// is pinned to the top-right of the display under the cursor. This deliberately
/// isn't the positioner plugin's own `move_window(TrayCenter)`: that panics
/// ("Tray position not set") when *its* internal tray cache is also empty.
fn position_popover(window: &tauri::WebviewWindow, tray: Option<TrayCapture>) {
    let Ok(outer) = window.outer_size() else {
        return;
    };
    let win = outer.to_logical::<f64>(window.scale_factor().unwrap_or(1.0));
    let displays = displays(window);
    let tray = tray.and_then(|t| popover_placement::tray_to_logical(t.rect, t.cursor, &displays));
    let cursor = cursor_points(window.app_handle());
    let Some(p) = popover_placement::place(tray, (win.width, win.height), &displays, cursor) else {
        return;
    };
    tracing::debug!(
        tray = ?tray.map(|(r, _)| r),
        display = p.display,
        scale = displays[p.display].scale,
        position = ?p.position,
        size = ?p.size,
        "placing popover"
    );
    if let Some((w, h)) = p.size {
        let _ = window.set_size(LogicalSize::new(w, h));
    }
    let _ = window.set_position(LogicalPosition::new(p.position.0, p.position.1));
}

pub(crate) fn show_popover(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let tray = app
            .try_state::<PopoverState>()
            .and_then(|s| *s.tray.lock().unwrap());
        position_popover(&window, tray);
        let _ = window.show();
        let _ = window.set_focus();
        // Tell the UI it's being shown so it can refresh; the popover hides on
        // blur, so each show is a fresh open where sessions/PRs may have changed
        // (e.g. work happened or windows closed while it was hidden).
        let _ = window.emit("popover-shown", ());
    }
}

/// Show the Settings window, announcing a genuine open (hidden → shown) to its
/// webview.
///
/// The `settings` webview is created at startup and merely *hidden* when closed
/// (`useHideOnClose`), so it keeps whatever it read on mount — which happens
/// before the first-run onboarding dialog has even been answered. Telling it
/// about each open lets it re-read `~/.maiestro/settings.json`, so the
/// Preferences panel can't go on showing a launch-at-login (or hand-edited)
/// value that is no longer true (#139). Mirrors `popover-shown`; the
/// already-visible guard keeps a second Settings click from reloading the form
/// out from under an edit in progress.
fn show_settings(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("settings") else {
        return;
    };
    let was_visible = window.is_visible().unwrap_or(false);
    let _ = window.show();
    let _ = window.set_focus();
    if !was_visible {
        let _ = window.emit("settings-shown", ());
    }
}

/// Open the Settings window from the popover's gear button. Routed through the
/// backend (rather than `show()` in JS) so both entry points — this and the tray
/// menu — go through [`show_settings`] and announce the open the same way.
#[tauri::command]
fn open_settings(app: tauri::AppHandle) {
    crate::log_invoke!("open_settings");
    show_settings(&app);
}

fn main() {
    // The app binary doubles as the Claude Code hook helper. When invoked as
    // `maiestro hook <state> --workspace <ws-id>` (from a spawned worktree's
    // .claude/settings.local.json), handle the hook and exit BEFORE booting the
    // tray app — otherwise every hook would launch a second mAIestro Code. This path
    // is short-lived and writes only a status file, so it skips logging setup.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("hook") {
        status::run_hook_cli(&args[2..]);
        return;
    }

    logging::init();
    tracing::info!("mAIestro Code starting");

    let registry = PluginRegistry::builder()
        .register(GitHubPlugin)
        .build();

    tauri::Builder::default()
        .manage(PopoverState::default())
        .manage(registry)
        .plugin(tauri_plugin_positioner::init())
        // Launch-at-login writes a per-user LaunchAgent (issue #98). The plist
        // name is pinned to the pre-rebrand "mAIestro" rather than following
        // productName, so existing installs keep (and reconcile) the same
        // ~/Library/LaunchAgents/mAIestro.plist instead of orphaning it.
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .macos_launcher(tauri_plugin_autostart::MacosLauncher::LaunchAgent)
                .app_name("mAIestro")
                .build(),
        )
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Relaunch focuses the existing app instead of spawning a second.
            show_popover(app);
        }))
        .invoke_handler(tauri::generate_handler![
            credentials::credentials_set,
            credentials::credentials_exists,
            credentials::credentials_delete,
            plugins_list_credential_types,
            repo_settings::repos_list,
            repo_settings::repo_settings_schema,
            repo_settings::repo_settings_get,
            repo_settings::repo_settings_set,
            repo_settings::repo_set_visibility,
            repo_settings::repo_remove,
            repo_settings::repo_scan_env_files,
            health::repo_health_check,
            github_list_repos,
            github_get_repo,
            github_list_issues,
            identities::identities_list,
            identities::identities_get_default,
            identities::identities_add,
            identities::identities_remove,
            open_settings,
            links::open_url,
            links::open_path,
            links::path_exists,
            links::reveal_path,
            spawn::prepare_spawn,
            drafting::suggest_short_title,
            spawn::draft_spawn_preview,
            spawn::confirm_spawn,
            spawn::create_issue_direct,
            editor::open_in_editor,
            editor::open_repo_in_editor,
            spawn::teardown,
            editor::open_accessibility_settings,
            pr::session_pr,
            pr::session_create_pr,
            pr::session_pr_checks,
            pr::session_work_state,
            pr::session_merge_pr,
            sessions::sessions_list,
            sessions::session_set_visibility,
            status::sessions_status_list,
            logging::logs_read,
            logging::logs_reveal,
            status::clear_session_error,
            app_settings::app_settings_get_theme,
            app_settings::app_settings_schema,
            app_settings::app_settings_get,
            app_settings::app_settings_problem,
            app_settings::app_settings_set,
            app_settings::onboarding_complete,
            tools::tools_resolved,
            about::app_version,
            update_check::update_check_status,
            update_check::update_dismiss,
        ])
        .setup(|app| {
            // Menu-bar-only: no dock icon on macOS.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Recover the user's real login-shell PATH once, so the CLIs we invoke
            // directly (claude, git, code) resolve even under the minimal PATH the
            // app gets when launched from /Applications at login. See tools.rs / #85.
            tools::init();

            // Apply the user's persisted popover size before the first show, so
            // the popover opens at their chosen dimensions with no resize flash.
            restore_popover_size(app.handle());
            // Same for the Settings window, restored before its first show.
            restore_settings_size(app.handle());

            // Live per-session status: drop orphaned status files, then watch
            // ~/.maiestro/status/ and forward changes to the popover as
            // `session-status` events. The watcher must outlive setup(), so park
            // it in managed state (dropping it would stop the watch).
            status::sweep_stale();
            // Rescue any session left stuck in `creating` by a spawn the app
            // quit/crashed out of mid-flight: surface it as a spawn error the row
            // can be torn down from, rather than a permanent "Creating…" pill (#101).
            status::reconcile_stale_creating();
            // Heal any worktree hooks still pointing at a now-stale binary path
            // (a torn-down/rebuilt spawner), so live status survives across
            // teardowns and `tauri dev` rebuilds. See spawn.rs / issue #35.
            hooks::reconcile_all_session_hooks();

            // Bring the launch-at-login LaunchAgent into agreement with the stored
            // pref (healing a stale baked binary path), then — on the very first
            // run only — run onboarding (which offers launch-at-login). Issue #98.
            app_settings::reconcile_launch_at_login(app.handle());
            app_settings::maybe_show_onboarding(app.handle());

            match status::start_watcher(app.handle().clone()) {
                Ok(watcher) => {
                    app.manage(Mutex::new(watcher));
                }
                Err(e) => tracing::error!(error = %e, "status watcher failed to start"),
            }

            // Background "newer release available?" poll (issue #182): seeds
            // its state from settings.json and checks GitHub Releases every
            // ~12h, unauthenticated. See update_check.rs / docs/update-check.md.
            update_check::start(app.handle().clone());

            let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
            let logs = MenuItem::with_id(app, "logs", "Show Logs", true, None::<&str>)?;
            let separator = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "Quit mAIestro Code", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&settings, &logs, &separator, &quit])?;

            TrayIconBuilder::with_id("main")
                // White brain mark rendered as a macOS template image, so the
                // system tints it for both light and dark menu-bar appearances.
                .icon(tauri::include_image!("icons/tray.png"))
                .icon_as_template(true)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "quit" => app.exit(0),
                    "settings" => show_settings(app),
                    "logs" => {
                        if let Some(window) = app.get_webview_window("logs") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    let app = tray.app_handle();
                    // Cache the tray rectangle so the positioner can place the window.
                    tauri_plugin_positioner::on_tray_event(app, &event);
                    // Keep our own copy too: `position_popover` uses it to place
                    // the window analytically (the positioner's cache is private).
                    if let TrayIconEvent::Click { rect, .. }
                    | TrayIconEvent::Enter { rect, .. }
                    | TrayIconEvent::Move { rect, .. } = &event
                    {
                        if let Some(state) = app.try_state::<PopoverState>() {
                            let (pos, size) = (
                                rect.position.to_physical::<f64>(1.0),
                                rect.size.to_physical::<f64>(1.0),
                            );
                            *state.tray.lock().unwrap() = Some(TrayCapture {
                                rect: PlacementRect { x: pos.x, y: pos.y, w: size.width, h: size.height },
                                cursor: cursor_points(app),
                            });
                        }
                    }

                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let Some(window) = app.get_webview_window("main") else {
                            return;
                        };
                        let visible = window.is_visible().unwrap_or(false);

                        let recently_auto_hidden = {
                            let state = app.state::<PopoverState>();
                            let mut last = state.last_auto_hide.lock().unwrap();
                            let recent = last
                                .map(|t| t.elapsed() < Duration::from_millis(300))
                                .unwrap_or(false);
                            *last = None;
                            recent
                        };

                        if visible {
                            let _ = window.hide();
                        } else if !recently_auto_hidden {
                            show_popover(app);
                        }
                        // else: this click's mousedown already blurred + hid the
                        // window, so leave it closed.
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| match window.label() {
            // The menu-bar popover auto-hides on blur; capture its (possibly
            // just-resized) size before hiding so it survives the next launch.
            "main" => {
                if let WindowEvent::Focused(false) = event {
                    let app = window.app_handle();
                    if let Some(state) = app.try_state::<PopoverState>() {
                        *state.last_auto_hide.lock().unwrap() = Some(Instant::now());
                    }
                    persist_popover_size(window);
                    let _ = window.hide();
                }
            }
            // The Settings window stays open on blur (it's a normal window), so
            // persist its size whenever it loses focus or is closed — either lands
            // after a resize drag finishes, without writing on every drag frame.
            "settings" => match event {
                WindowEvent::Focused(false) | WindowEvent::CloseRequested { .. } => {
                    persist_settings_size(window);
                }
                _ => {}
            },
            // Closing the onboarding window without pressing "Get started" accepts
            // the defaults — record completion so it doesn't reappear.
            "onboarding" => {
                if let WindowEvent::CloseRequested { .. } = event {
                    app_settings::complete_onboarding_if_pending(window.app_handle());
                }
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running mAIestro Code");
}

#[cfg(test)]
mod tests {
    //! Drift guard: the min-window sizes are mirrored between these Rust constants
    //! (used to clamp a restored size before first show) and `tauri.conf.json`
    //! (the actual window `minWidth`/`minHeight`). They must agree, or a restored
    //! size could be clamped to a value the window itself refuses.

    const CONF: &str = include_str!("../tauri.conf.json");

    fn min_size(label: &str) -> (f64, f64) {
        let conf: serde_json::Value = serde_json::from_str(CONF).expect("tauri.conf.json is valid JSON");
        let windows = conf["app"]["windows"].as_array().expect("app.windows array");
        let w = windows
            .iter()
            .find(|w| w["label"] == label)
            .unwrap_or_else(|| panic!("no window labelled {label}"));
        (
            w["minWidth"].as_f64().expect("minWidth"),
            w["minHeight"].as_f64().expect("minHeight"),
        )
    }

    #[test]
    fn popover_min_matches_conf() {
        assert_eq!(min_size("main"), (super::MIN_POPOVER_WIDTH, super::MIN_POPOVER_HEIGHT));
    }

    #[test]
    fn settings_min_matches_conf() {
        assert_eq!(min_size("settings"), (super::MIN_SETTINGS_WIDTH, super::MIN_SETTINGS_HEIGHT));
    }
}

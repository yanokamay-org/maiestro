//! Windows Terminal as a terminal host (Windows): open a session as a titled,
//! colored **Windows Terminal tab**, and later find, select and close that tab
//! again.
//!
//! Every `wt` call goes through [`run`] with an **argument list**, never a
//! shell string. The tab runs the agent directly — the resolved agent path and
//! its arguments, no shell in between — and inherits mAIestro Code's own
//! environment (its recovered login PATH), like a VS Code session started
//! through the `code` CLI; nothing is injected.
//!
//! `wt` rebuilds the child's command line itself, lossily: it wraps an argument
//! that contains a space in `"…"` but escapes nothing inside, drops empty
//! arguments, splits on `;` (its subcommand separator) and expands `%VAR%`.
//! [`encode_arg`] pre-encodes each argument so the agent receives exactly what
//! we meant; `%` has no escape, so user-derived text gets a lookalike
//! ([`user_text`]) and a `%` anywhere else fails the launch loudly.
//!
//! `wt` has no tab ids, and tab indexes and window ids shift, so a tab's
//! identity is its **title**: ours, and kept by `--suppressApplicationTitle`
//! against the agent's own title escapes. Tabs are found through **UI
//! Automation** in any Windows Terminal window (so a tab dragged to another
//! window is followed) and closed through their own close button: ending the
//! agent's process isn't enough, since Windows Terminal keeps a tab open when
//! its process exits non-zero. Looking never calls `wt`, which would open a
//! window.
//!
//! Windows Terminal's `settings.json`, `state.json`, profiles and color schemes
//! are **never modified**, and no settings fragment is written: the title and
//! the tab color are arguments of the one `new-tab` we run.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::editor::WindowClose;
use crate::terminal_host::TerminalLayout;

/// Which Windows Terminal tab a session runs in: the title it was launched
/// with. Never an index, which shifts as tabs open and close.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WtTab {
    pub title: String,
}

/// The message for a Windows Terminal that isn't installed (or whose app
/// execution alias is turned off), shared with Check Health.
pub const NOT_FOUND: &str = "Windows Terminal (wt.exe) not found. It comes with Windows 11; on Windows 10, install it \
     from the Microsoft Store or with `winget install Microsoft.WindowsTerminal`. If it is installed, turn on its app \
     execution alias in Settings → Apps → Advanced app settings → App execution aliases.";

/// How long one `wt` call may run. It returns at once, before the tab exists.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a new tab may take to appear, generous for a cold start.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a closed tab may take to disappear.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(4);

/// The window a session's tab goes into under `layout`: a window of its own
/// (`windows`), the repo's (`per-repo`), or one shared window (`tabs`). `wt -w`
/// reuses a window of that name or opens one, so no window state is kept.
pub fn window_name(layout: TerminalLayout, session_id: &str, repo: &str) -> String {
    let name = match layout {
        TerminalLayout::Windows => format!("maiestro-{session_id}"),
        TerminalLayout::PerRepo => format!("maiestro-{}", repo.replace('/', "-")),
        TerminalLayout::Tabs => "maiestro".to_string(),
    };
    name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' }).collect()
}

/// User-derived text (a tab title, a session name) made safe for `wt`: control
/// characters (tab, newline) become a space, and `%`, which `wt` would expand
/// and can't escape, becomes the fullwidth `％`.
pub fn user_text(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else if c == '%' { '％' } else { c }).collect()
}

/// One value of `wt`'s own options (`-d`, `--title`): `;` escaped, since `wt`
/// splits its whole command line into subcommands on it.
fn escape_separators(s: &str) -> String {
    s.replace(';', r"\;")
}

/// Pre-encode one argument of the tab's command for `wt`'s lossy rebuild: the
/// content escaped by the MSVC rules (a `"` and the backslashes before it, and
/// trailing backslashes when `wt` will wrap the argument because it holds a
/// space), the outer quotes left to `wt`, `""` for an empty argument, and `;`
/// as `\;`.
fn encode_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".into();
    }
    let wrapped = arg.contains(' ');
    let mut out = String::with_capacity(arg.len() + 2);
    let mut backslashes = 0usize;
    for c in arg.chars() {
        if c == '\\' {
            backslashes += 1;
            continue;
        }
        if c == '"' {
            out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n('\\', backslashes));
            if c == ';' {
                out.push('\\');
            }
        }
        out.push(c);
        backslashes = 0;
    }
    out.extend(std::iter::repeat_n('\\', if wrapped { backslashes * 2 } else { backslashes }));
    out
}

/// Fail on a `%` in a value we don't control the text of: `wt` would expand it.
fn reject_percent(what: &str, value: &str) -> Result<(), String> {
    if value.contains('%') {
        return Err(format!("Windows Terminal can't start a session whose {what} contains `%`: {value}"));
    }
    Ok(())
}

/// The full `wt` argument list that opens the session's tab: into window
/// `window`, in `work_dir`, titled `title` (made safe with [`user_text`]),
/// colored `color`, running `program` with `args` — which the caller built
/// from text already passed through [`user_text`] where it is the user's.
fn wt_args(window: &str, work_dir: &str, title: &str, color: &str, program: &str, args: &[String]) -> Result<Vec<String>, String> {
    reject_percent("worktree path", work_dir)?;
    reject_percent("agent path", program)?;
    for a in args {
        reject_percent("agent arguments", a)?;
    }
    let mut out: Vec<String> = vec![
        "-w".into(),
        window.into(),
        "new-tab".into(),
        "-d".into(),
        escape_separators(work_dir),
        "--title".into(),
        escape_separators(&user_text(title)),
        "--suppressApplicationTitle".into(),
    ];
    if is_hex_color(color) {
        out.extend(["--tabColor".into(), color.into()]);
    }
    out.push("--".into());
    out.push(encode_arg(program));
    out.extend(args.iter().map(|a| encode_arg(a)));
    Ok(out)
}

fn is_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Open the session as a new tab running `program args` in `work_dir`, titled
/// `title` and colored `color`, in the window `layout` picks for it, and wait
/// until the tab is there. A tab that never appears fails the launch.
pub async fn launch(
    work_dir: &Path,
    program: &str,
    args: &[String],
    title: &str,
    color: &str,
    window: &str,
) -> Result<WtTab, String> {
    if !cfg!(target_os = "windows") {
        return Err("Windows Terminal sessions are only available on Windows".into());
    }
    let tab = WtTab { title: user_text(title) };
    if probe(&tab).await? {
        // A tab of that title is already open (the user's, or a session whose
        // record lost its handle): opening a second one would leave two tabs
        // nobody could tell apart.
        return Err(format!("A Windows Terminal tab named \"{}\" is already open. Close it, then try again.", tab.title));
    }
    let wt = wt_args(window, &work_dir.to_string_lossy(), title, color, program, args)?;
    run(&wt).await?;
    let deadline = tokio::time::Instant::now() + LAUNCH_TIMEOUT;
    loop {
        if probe(&tab).await? {
            return Ok(tab);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "Windows Terminal didn't open the session's tab within {} seconds. Check that Windows Terminal starts, \
                 then open the session again.",
                LAUNCH_TIMEOUT.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The window the repo row's open button puts `repo`'s plain tab in: the
/// window its sessions share under `layout` (the repo's, or the one shared
/// window), and under `windows`, where no window is the repo's, the most
/// recently used Windows Terminal window (`-w 0`). A tab either way, never a
/// window of its own unless none is open.
pub fn folder_window_name(layout: TerminalLayout, repo: &str) -> String {
    match layout {
        TerminalLayout::Windows => "0".to_string(),
        TerminalLayout::PerRepo | TerminalLayout::Tabs => window_name(layout, "", repo),
    }
}

/// Open a plain tab (the default profile, no agent) in `dir` — the repo row's
/// "open the cloned repo" button — in window `window` (see
/// [`folder_window_name`]). Nothing is tracked.
pub async fn open_folder(dir: &Path, window: &str) -> Result<(), String> {
    if !cfg!(target_os = "windows") {
        return Err("Windows Terminal is only available on Windows".into());
    }
    run(&["-w".into(), window.into(), "new-tab".into(), "-d".into(), escape_separators(&dir.to_string_lossy())]).await
}

/// Bring the session's tab forward: selected in its window, the window
/// restored and raised. `Ok(false)` when the tab is gone, so the caller can
/// open a new one. Never starts Windows Terminal.
pub async fn focus(tab: &WtTab) -> Result<bool, String> {
    let title = tab.title.clone();
    blocking(move || uia::focus(&title)).await
}

/// Whether the session's tab is still open, in any Windows Terminal window.
/// Never starts Windows Terminal.
pub async fn probe(tab: &WtTab) -> Result<bool, String> {
    let title = tab.title.clone();
    blocking(move || uia::exists(&title)).await
}

/// Close the session's tab — with the agent still running, through the tab's
/// own close button, which asks nothing — and confirm it is gone. A failure to
/// look or to close is `StillOpen`, never `Closed`: Windows refuses to remove a
/// worktree that a process still runs in, and nothing else here can tell.
pub async fn close_and_wait(tab: &WtTab) -> WindowClose {
    // Closing a tab brings its Windows Terminal window forward, which would
    // blur — and so hide — the popover the teardown was started from. Hold the
    // popover open meanwhile, and hand the foreground back once the tab is gone.
    let _hold = FocusHold::start();
    let previous = uia::foreground();
    let close = close_tab(tab).await;
    if let Some(previous) = previous {
        tokio::time::sleep(Duration::from_millis(300)).await;
        uia::restore_foreground(previous);
    }
    close
}

async fn close_tab(tab: &WtTab) -> WindowClose {
    let title = tab.title.clone();
    match blocking(move || uia::close(&title)).await {
        Ok(false) => return WindowClose::Closed,
        Ok(true) => {}
        Err(e) => {
            tracing::warn!(error = %e, "could not close the Windows Terminal tab");
            return WindowClose::StillOpen;
        }
    }
    let deadline = tokio::time::Instant::now() + CLOSE_TIMEOUT;
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        match probe(tab).await {
            Ok(false) => return WindowClose::Closed,
            Ok(true) if tokio::time::Instant::now() >= deadline => return WindowClose::StillOpen,
            Ok(true) => {}
            Err(e) => {
                tracing::warn!(error = %e, "could not confirm the Windows Terminal tab closed");
                return WindowClose::StillOpen;
            }
        }
    }
}

/// How many tab closes are in flight; see [`focus_held`].
static FOCUS_HOLDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// While alive, [`focus_held`] is true.
struct FocusHold;

impl FocusHold {
    fn start() -> Self {
        FOCUS_HOLDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        FocusHold
    }
}

impl Drop for FocusHold {
    fn drop(&mut self) {
        FOCUS_HOLDS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Whether a tab is being closed, during which Windows Terminal may briefly
/// take the foreground: the popover must not auto-hide on that blur, since
/// [`close_and_wait`] hands the foreground back afterwards.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn focus_held() -> bool {
    FOCUS_HOLDS.load(std::sync::atomic::Ordering::SeqCst) > 0
}

/// The installed Windows Terminal's version (e.g. `1.24.12741.0`), for Check
/// Health, read without starting it: `wt.exe`'s file version when `wt` is a
/// real file (a `tool_paths` pin to a portable copy), else the installed
/// `Microsoft.WindowsTerminal` package's. `None` when neither is known.
pub async fn installed_version(wt: &Path) -> Option<String> {
    let wt = wt.to_path_buf();
    tokio::task::spawn_blocking(move || version::of(&wt)).await.ok().flatten()
}

/// Run a UI Automation call on a blocking thread (it is COM, and synchronous).
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| format!("the UI Automation call failed: {e}"))?
}

/// Run `wt` with `args`. It returns at once, before the tab exists.
async fn run(args: &[String]) -> Result<(), String> {
    let mut cmd = crate::tools::tokio_command("wt");
    cmd.args(args).stdin(std::process::Stdio::null()).kill_on_drop(true);
    let out = match tokio::time::timeout(CALL_TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(match crate::tools::stale_override("wt") {
                Some(pin) => format!("The Windows Terminal path set in Settings → General → Tool paths doesn't exist: {pin}"),
                None => NOT_FOUND.to_string(),
            })
        }
        Ok(Err(e)) => return Err(format!("couldn't run Windows Terminal: {e}")),
        Err(_) => return Err("Windows Terminal didn't respond".into()),
    };
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(format!(
        "Windows Terminal couldn't open the tab (exit {}): {}",
        out.status.code().map_or("?".to_string(), |c| c.to_string()),
        crate::tools::snippet(stderr.trim())
    ))
}

/// The UI Automation side: Windows Terminal's tabs are `TabItem`s named after
/// their title, in `CASCADIA_HOSTING_WINDOW_CLASS` windows, each with a
/// `CloseButton`. Same-user windows need no grant or elevation.
#[cfg(target_os = "windows")]
mod uia {
    use windows::core::{Error, Result as WinResult, HRESULT};
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationInvokePattern,
        IUIAutomationSelectionItemPattern, TreeScope_Children, TreeScope_Descendants, UIA_AutomationIdPropertyId,
        UIA_ClassNamePropertyId, UIA_ControlTypePropertyId, UIA_InvokePatternId, UIA_NamePropertyId,
        UIA_SelectionItemPatternId, UIA_TabItemControlTypeId,
    };
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, IsIconic, IsWindow, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };

    const WINDOW_CLASS: &str = "CASCADIA_HOSTING_WINDOW_CLASS";

    /// The foreground window, as a plain value that can cross threads.
    pub fn foreground() -> Option<isize> {
        // SAFETY: a plain query with no arguments.
        let hwnd = unsafe { GetForegroundWindow() };
        (!hwnd.is_invalid()).then_some(hwnd.0 as isize)
    }

    /// Give the foreground back to `previous` when a Windows Terminal window
    /// took it — never over a window the user switched to themselves.
    pub fn restore_foreground(previous: isize) {
        let previous = HWND(previous as *mut _);
        // SAFETY: plain window queries; a window that has since closed just
        // makes them fail harmlessly.
        unsafe {
            let now = GetForegroundWindow();
            if now == previous || !IsWindow(Some(previous)).as_bool() {
                return;
            }
            let mut class = [0u16; 64];
            let n = GetClassNameW(now, &mut class);
            if n <= 0 || String::from_utf16_lossy(&class[..n as usize]) != WINDOW_CLASS {
                return;
            }
            let _ = SetForegroundWindow(previous);
        }
    }

    /// COM, initialized on this thread for as long as the guard lives.
    struct Com {
        owned: bool,
    }

    impl Com {
        fn init() -> Result<Self, String> {
            // SAFETY: plain COM initialization; balanced by `Drop` when it succeeded.
            let hr: HRESULT = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if hr == RPC_E_CHANGED_MODE {
                // Already initialized differently on this thread; usable as is.
                return Ok(Com { owned: false });
            }
            hr.ok().map_err(|e| format!("couldn't initialize COM: {e}"))?;
            Ok(Com { owned: true })
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if self.owned {
                // SAFETY: balances the successful `CoInitializeEx` above.
                unsafe { CoUninitialize() };
            }
        }
    }

    /// Run `f` with a UI Automation client. Every COM object is dropped before
    /// COM is uninitialized, so `f` returns plain data only.
    fn with_uia<T>(f: impl FnOnce(&IUIAutomation) -> WinResult<T>) -> Result<T, String> {
        let _com = Com::init()?;
        // SAFETY: creating the in-process UI Automation client.
        let uia: IUIAutomation = unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("couldn't start UI Automation: {e}"))?;
        let result = f(&uia).map_err(|e: Error| format!("UI Automation failed: {e}"));
        drop(uia);
        result
    }

    /// The first tab titled `title` in any Windows Terminal window, with its window.
    fn find(uia: &IUIAutomation, title: &str) -> WinResult<Option<(IUIAutomationElement, IUIAutomationElement)>> {
        // SAFETY: UI Automation calls on objects this client owns.
        unsafe {
            let root = uia.GetRootElement()?;
            let class = uia.CreatePropertyCondition(UIA_ClassNamePropertyId, &VARIANT::from(WINDOW_CLASS))?;
            let windows = root.FindAll(TreeScope_Children, &class)?;
            let tab_cond = uia.CreateAndCondition(
                &uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(UIA_TabItemControlTypeId.0))?,
                &uia.CreatePropertyCondition(UIA_NamePropertyId, &VARIANT::from(title))?,
            )?;
            for i in 0..windows.Length()? {
                let window = windows.GetElement(i)?;
                let tabs = window.FindAll(TreeScope_Descendants, &tab_cond)?;
                if tabs.Length()? > 0 {
                    return Ok(Some((tabs.GetElement(0)?, window)));
                }
            }
            Ok(None)
        }
    }

    pub fn exists(title: &str) -> Result<bool, String> {
        with_uia(|uia| Ok(find(uia, title)?.is_some()))
    }

    /// Select the tab and bring its window forward. True when the tab exists,
    /// even if Windows' foreground lock kept the window from coming forward:
    /// the session is open either way, and opening another would run a second
    /// agent in the worktree.
    pub fn focus(title: &str) -> Result<bool, String> {
        with_uia(|uia| {
            let Some((tab, window)) = find(uia, title)? else {
                return Ok(false);
            };
            // SAFETY: as in `find`; a window that has since closed just makes
            // the Win32 calls fail harmlessly.
            unsafe {
                if let Ok(select) =
                    tab.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
                {
                    let _ = select.Select();
                }
                if let Ok(hwnd) = window.CurrentNativeWindowHandle() {
                    if IsIconic(hwnd).as_bool() {
                        let _ = ShowWindow(hwnd, SW_RESTORE);
                    }
                    let _ = SetForegroundWindow(hwnd);
                }
            }
            Ok(true)
        })
    }

    /// Invoke the tab's close button. False when there is no such tab.
    pub fn close(title: &str) -> Result<bool, String> {
        with_uia(|uia| {
            let Some((tab, _)) = find(uia, title)? else {
                return Ok(false);
            };
            // SAFETY: as in `find`.
            unsafe {
                let cond = uia.CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from("CloseButton"))?;
                let buttons = tab.FindAll(TreeScope_Descendants, &cond)?;
                if buttons.Length()? == 0 {
                    return Err(Error::new(windows::Win32::Foundation::E_FAIL, "the tab has no close button"));
                }
                buttons
                    .GetElement(0)?
                    .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)?
                    .Invoke()?;
            }
            Ok(true)
        })
    }
}

/// UI Automation exists only on Windows; elsewhere there is never a tab.
#[cfg(not(target_os = "windows"))]
mod uia {
    pub fn foreground() -> Option<isize> {
        None
    }
    pub fn restore_foreground(_previous: isize) {}
    pub fn exists(_title: &str) -> Result<bool, String> {
        Err("Windows Terminal is only available on Windows".into())
    }
    pub fn focus(title: &str) -> Result<bool, String> {
        exists(title)
    }
    pub fn close(title: &str) -> Result<bool, String> {
        exists(title)
    }
}

/// Windows Terminal's version, read from files and the package catalog only.
#[cfg(target_os = "windows")]
mod version {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::core::{HSTRING, PCWSTR};
    use windows::Management::Deployment::PackageManager;
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
    };

    /// The stable and Preview packages, in that order.
    const FAMILIES: &[&str] = &["Microsoft.WindowsTerminal_8wekyb3d8bbwe", "Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe"];

    pub fn of(wt: &Path) -> Option<String> {
        file_version(wt).or_else(package_version)
    }

    /// The `VS_FIXEDFILEINFO` version of a real `wt.exe`. `None` for the app
    /// execution alias, which has no version resource of its own.
    fn file_version(path: &Path) -> Option<String> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let name = PCWSTR(wide.as_ptr());
        // SAFETY: the buffer is sized by `GetFileVersionInfoSizeW` and outlives
        // the `VerQueryValueW` pointer into it, which is read before returning.
        unsafe {
            let size = GetFileVersionInfoSizeW(name, None);
            if size == 0 {
                return None;
            }
            let mut buf = vec![0u8; size as usize];
            GetFileVersionInfoW(name, None, size, buf.as_mut_ptr().cast()).ok()?;
            let mut info: *mut std::ffi::c_void = std::ptr::null_mut();
            let mut len = 0u32;
            let root: Vec<u16> = "\\".encode_utf16().chain(Some(0)).collect();
            if !VerQueryValueW(buf.as_ptr().cast(), PCWSTR(root.as_ptr()), &mut info, &mut len).as_bool()
                || info.is_null()
                || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>()
            {
                return None;
            }
            let fixed = &*(info as *const VS_FIXEDFILEINFO);
            Some(format!(
                "{}.{}.{}.{}",
                fixed.dwFileVersionMS >> 16,
                fixed.dwFileVersionMS & 0xffff,
                fixed.dwFileVersionLS >> 16,
                fixed.dwFileVersionLS & 0xffff
            ))
        }
    }

    /// The version of the installed Windows Terminal package for this user.
    fn package_version() -> Option<String> {
        let pm = PackageManager::new().ok()?;
        FAMILIES.iter().find_map(|family| {
            let packages = pm.FindPackagesByUserSecurityIdPackageFamilyName(&HSTRING::new(), &HSTRING::from(*family)).ok()?;
            let package = packages.into_iter().next()?;
            let v = package.Id().ok()?.Version().ok()?;
            Some(format!("{}.{}.{}.{}", v.Major, v.Minor, v.Build, v.Revision))
        })
    }
}

#[cfg(not(target_os = "windows"))]
mod version {
    pub fn of(_wt: &std::path::Path) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MSVC's argument parsing (`CommandLineToArgvW` rules), for checking the
    /// round trip: what the agent receives from the command line `wt` builds.
    fn msvc_parse(cmdline: &str) -> Vec<String> {
        let mut args = Vec::new();
        let mut cur = String::new();
        let mut in_arg = false;
        let mut quoted = false;
        let chars: Vec<char> = cmdline.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' {
                let start = i;
                while i < chars.len() && chars[i] == '\\' {
                    i += 1;
                }
                let n = i - start;
                if i < chars.len() && chars[i] == '"' {
                    cur.extend(std::iter::repeat_n('\\', n / 2));
                    if n % 2 == 1 {
                        cur.push('"');
                        i += 1;
                    }
                } else {
                    cur.extend(std::iter::repeat_n('\\', n));
                }
                in_arg = true;
                continue;
            }
            if c == '"' {
                quoted = !quoted;
                in_arg = true;
            } else if c == ' ' && !quoted {
                if in_arg {
                    args.push(std::mem::take(&mut cur));
                    in_arg = false;
                }
            } else {
                cur.push(c);
                in_arg = true;
            }
            i += 1;
        }
        if in_arg {
            args.push(cur);
        }
        args
    }

    /// How `wt` rebuilds the child's command line from what it receives, as the
    /// spike measured it: `\;` unescaped to `;`, an argument with a space wrapped
    /// in quotes, nothing else escaped.
    fn wt_rebuild(args: &[String]) -> String {
        args.iter()
            .map(|a| {
                let a = a.replace(r"\;", ";");
                if a.contains(' ') { format!("\"{a}\"") } else { a }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn round_trip(arg: &str) -> Vec<String> {
        msvc_parse(&wt_rebuild(&[encode_arg(arg)]))
    }

    #[test]
    fn arguments_survive_wts_rebuild() {
        for arg in [
            "--name",
            "Fix \"quoted\" thing; ok",
            "/color purple",
            "''",
            r"C:\path with space\",
            r"C:\nospace\",
            r#"a\"b"#,
            r"a\;b",
            "plain",
            r#"hooks.PreToolUse=[{matcher="",hooks=[{type="command",command="'C:\Users\u\.maiestro\bin\maiestro-hook.cmd' working"}]}]"#,
        ] {
            assert_eq!(round_trip(arg), [arg], "{arg}");
        }
    }

    #[test]
    fn an_empty_argument_is_kept() {
        assert_eq!(encode_arg(""), "\"\"");
        assert_eq!(msvc_parse(&wt_rebuild(&["a".into(), encode_arg(""), "b".into()])), ["a", "", "b"]);
    }

    #[test]
    fn semicolons_are_escaped() {
        assert_eq!(encode_arg("a;b"), r"a\;b");
        assert_eq!(escape_separators("x;y;z"), r"x\;y\;z");
    }

    #[test]
    fn user_text_drops_control_characters_and_percent() {
        assert_eq!(user_text("a\tb\nc"), "a b c");
        assert_eq!(user_text("100% done %PATH%"), "100％ done ％PATH％");
        assert_eq!(user_text("🍋 #242 — tabs"), "🍋 #242 — tabs");
    }

    #[test]
    fn window_name_follows_the_layout() {
        assert_eq!(window_name(TerminalLayout::Windows, "242-wt-tabs", "acme/widgets"), "maiestro-242-wt-tabs");
        assert_eq!(window_name(TerminalLayout::PerRepo, "242-wt-tabs", "acme/widgets"), "maiestro-acme-widgets");
        assert_eq!(window_name(TerminalLayout::Tabs, "242-wt-tabs", "acme/widgets"), "maiestro");
        assert_eq!(window_name(TerminalLayout::PerRepo, "x", "my.org/a_b"), "maiestro-my.org-a_b");
    }

    /// The repo folder opens as a tab: in the repo's or the shared window, or
    /// the most recent one when every session has a window of its own.
    #[test]
    fn folder_window_is_a_tab_in_an_existing_window() {
        assert_eq!(folder_window_name(TerminalLayout::PerRepo, "acme/widgets"), "maiestro-acme-widgets");
        assert_eq!(folder_window_name(TerminalLayout::Tabs, "acme/widgets"), "maiestro");
        assert_eq!(folder_window_name(TerminalLayout::Windows, "acme/widgets"), "0");
    }

    #[test]
    fn builds_the_new_tab_command() {
        let args = wt_args(
            "maiestro-acme-widgets",
            r"C:\src\work-1\widgets",
            "widgets: 🍋 #1 — a; b\t50%",
            "#ca8a04",
            r"C:\Users\u\.local\bin\claude.exe",
            &["--name".into(), "🍋 #1 — a; b 50％".into(), "/color yellow".into()],
        )
        .unwrap();
        assert_eq!(
            args,
            [
                "-w",
                "maiestro-acme-widgets",
                "new-tab",
                "-d",
                r"C:\src\work-1\widgets",
                "--title",
                r"widgets: 🍋 #1 — a\; b 50％",
                "--suppressApplicationTitle",
                "--tabColor",
                "#ca8a04",
                "--",
                r"C:\Users\u\.local\bin\claude.exe",
                "--name",
                r"🍋 #1 — a\; b 50％",
                "/color yellow",
            ]
        );
    }

    #[test]
    fn a_bad_color_is_left_out() {
        let args = wt_args("w", r"C:\w", "t", "red", "claude", &[]).unwrap();
        assert!(!args.contains(&"--tabColor".to_string()), "{args:?}");
    }

    /// A `%` in anything but the user's own text fails loudly rather than being
    /// expanded by `wt`.
    #[test]
    fn a_percent_outside_user_text_fails() {
        assert!(wt_args("w", r"C:\100%\w", "t", "#000000", "claude", &[]).unwrap_err().contains("worktree path"));
        assert!(wt_args("w", r"C:\w", "t", "#000000", r"C:\%X%\claude", &[]).unwrap_err().contains("agent path"));
        assert!(wt_args("w", r"C:\w", "t", "#000000", "codex", &["-c".into(), "x=%Y%".into()])
            .unwrap_err()
            .contains("agent arguments"));
        assert!(wt_args("w", r"C:\w", "50%", "#000000", "claude", &[]).is_ok(), "the title is the user's");
    }

    /// End to end against the real Windows Terminal: launch a tab with a
    /// hostile title and arguments (checked by a child that writes what it
    /// received), find it, focus it, close it with the child still running, and
    /// check `settings.json` is unchanged. Opens and closes a real tab, so it
    /// runs only on demand: `cargo test windows_terminal -- --ignored`.
    #[cfg(target_os = "windows")]
    #[tokio::test]
    #[ignore = "drives the real Windows Terminal"]
    async fn launch_focus_and_close_a_real_tab() {
        let settings = dirs::data_local_dir()
            .unwrap()
            .join(r"Packages\Microsoft.WindowsTerminal_8wekyb3d8bbwe\LocalState\settings.json");
        let before = std::fs::read(&settings).ok();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("args.txt");
        let hostile = ["Fix \"quoted\" thing; ok", "", r"trailing\", "/color purple", "a\\\"b"];
        // PowerShell writes the arguments it got, one per line, then waits.
        let script = dir.path().join("print-args.ps1");
        std::fs::write(
            &script,
            format!(
                "$args | ForEach-Object {{ \"<$_>\" }} | Set-Content -Encoding utf8 '{}'\nStart-Sleep 600\n",
                out.display()
            ),
        )
        .unwrap();
        let mut args: Vec<String> = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"].map(String::from).to_vec();
        args.push(script.to_string_lossy().into_owned());
        args.extend(hostile.iter().map(|s| s.to_string()));
        let title = format!("maiestro: wt test \"q\"; 50% {}", std::process::id());
        let tab = launch(dir.path(), "powershell.exe", &args, &title, "#dc2626", "maiestro-test")
            .await
            .expect("launch");
        assert_eq!(tab.title, user_text(&title));
        assert!(probe(&tab).await.unwrap(), "the new tab is open");
        let mut got = String::new();
        for _ in 0..40 {
            got = std::fs::read_to_string(&out).unwrap_or_default();
            if !got.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        // Close the tab (the child still running) before checking anything
        // else, so a failed assertion leaves no tab behind.
        let focused = focus(&tab).await.unwrap();
        assert_eq!(close_and_wait(&tab).await, WindowClose::Closed);
        assert!(focused, "the tab could be focused");
        assert!(!probe(&tab).await.unwrap(), "the tab is gone");
        let expected: String = hostile.iter().map(|a| format!("<{a}>\r\n")).collect();
        assert_eq!(got.trim_start_matches('\u{feff}'), expected, "the child got its arguments intact");
        assert_eq!(before, std::fs::read(&settings).ok(), "Windows Terminal's settings.json is unchanged");
        let wt = crate::tools::find_tool("wt").expect("wt resolves");
        let version = installed_version(&wt).await.expect("the version reads without starting wt");
        assert!(version.starts_with("1."), "{version}");
    }
}

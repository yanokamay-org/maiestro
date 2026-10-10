import { useState } from "react";
import { Platform, SetTerminalHostOutcome, TerminalHost, WindowPermission } from "../api";

// The repo form's terminal-host select. Changing it moves the repo's existing
// sessions too, which closes their open windows (ending their agents), so the
// backend answers `needs_confirmation` first when any is open, and this asks
// before calling again with `confirmed`. Only a completed switch reaches the
// form (`onSaved`), so the autosave never writes a host the sessions didn't
// follow.

/** Product names for the terminal hosts. */
export const TERMINAL_HOST_PRODUCTS: Record<TerminalHost, string> = {
  vscode: "Visual Studio Code",
  terminal_app: "Terminal.app",
  cmux: "cmux",
  windows_terminal: "Windows Terminal",
};

/** The terminal hosts to offer on `platform`. One the file already names stays
 *  listed even where it isn't available (a settings file synced from a Mac),
 *  so the select shows the real value rather than a blank. */
export function terminalHostOptions(platform: Platform, current: TerminalHost | null | undefined): TerminalHost[] {
  const hosts: TerminalHost[] =
    platform === "macos" ? ["vscode", "terminal_app", "cmux"] : platform === "windows" ? ["vscode", "windows_terminal"] : ["vscode"];
  if (current && !hosts.includes(current)) hosts.push(current);
  return hosts;
}

const PERMISSION_BUTTON: Record<WindowPermission, string> = {
  accessibility: "Open Accessibility Options",
  automation: "Open Automation Options",
};

type Pending =
  | { kind: "confirm"; value: TerminalHost | null; open: string[] }
  | { kind: "blocked"; message: string; permission: WindowPermission | null }
  | { kind: "error"; message: string };

export interface TerminalHostSwitchProps {
  /** The stored value; null = the schema default. */
  value: TerminalHost | null;
  /** The terminal hosts to list (the running OS's, plus `value`). */
  options: TerminalHost[];
  /** The hosts that exist on this computer. */
  available: TerminalHost[];
  /** What null resolves to, named in the default option. */
  fallback: TerminalHost;
  /** Offer a "Default (…)" option for null (a repo following the global
   *  default). Off for the global setting itself, which shows `fallback`. */
  defaultOption?: boolean;
  /** Who else moves, completing "Switching to X moves …". */
  scope?: string;
  /** Calls `repo_set_terminal_host`. */
  onSwitch: (value: TerminalHost | null, confirmed: boolean) => Promise<SetTerminalHostOutcome>;
  /** The switch is saved; mirror it into the form. */
  onSaved: (value: TerminalHost | null) => void;
  onOpenPermissionSettings: (permission: WindowPermission) => void;
}

export function TerminalHostSwitch({
  value, options, available, fallback, defaultOption = true, scope = "this repo's existing sessions too", onSwitch,
  onSaved, onOpenPermissionSettings,
}: TerminalHostSwitchProps) {
  const [pending, setPending] = useState<Pending | null>(null);
  const [busy, setBusy] = useState(false);

  async function run(next: TerminalHost | null, confirmed: boolean) {
    setBusy(true);
    try {
      const res = await onSwitch(next, confirmed);
      if (res.status === "needs_confirmation") {
        setPending({ kind: "confirm", value: next, open: res.open });
      } else if (res.status === "blocked_by_editor") {
        setPending({ kind: "blocked", message: res.message, permission: res.permission });
      } else {
        setPending(null);
        onSaved(next);
      }
    } catch (e) {
      setPending({ kind: "error", message: String(e) });
    } finally {
      setBusy(false);
    }
  }

  const name = (v: TerminalHost | null) => TERMINAL_HOST_PRODUCTS[v ?? fallback];

  return (
    <>
      <select
        className={`text-input profile-select ${busy ? "btn-busy" : ""}`}
        aria-label="Terminal host"
        value={value ?? (defaultOption ? "" : fallback)}
        disabled={busy || pending?.kind === "confirm"}
        onChange={(e) => run(e.target.value === "" ? null : (e.target.value as TerminalHost), false)}
      >
        {defaultOption && <option value="">Default ({TERMINAL_HOST_PRODUCTS[fallback]})</option>}
        {options.map((h) => (
          <option key={h} value={h}>
            {TERMINAL_HOST_PRODUCTS[h]}
            {available.includes(h) ? "" : " (not available on this computer)"}
          </option>
        ))}
      </select>
      {pending?.kind === "confirm" && (
        <div className="cleanup-confirm">
          <p className="cleanup-lead">
            Switching to {name(pending.value)} moves {scope}. These windows will close, ending their agents (the
            conversations won't carry over):
          </p>
          <ul className="cleanup-warnings">
            {pending.open.map((t) => (
              <li key={t}>{t}</li>
            ))}
          </ul>
          <div className="issue-actions">
            <button className={`btn-danger ${busy ? "btn-busy" : ""}`} disabled={busy} onClick={() => run(pending.value, true)}>
              Close windows and switch
            </button>
            <button className="btn-ghost" disabled={busy} onClick={() => setPending(null)}>Cancel</button>
          </div>
        </div>
      )}
      {pending?.kind === "blocked" && (
        <div className="cleanup-confirm">
          <p className="cleanup-lead" style={{ whiteSpace: "pre-line" }}>{pending.message}</p>
          <div className="issue-actions">
            {pending.permission && (
              <button className="btn-ghost" onClick={() => onOpenPermissionSettings(pending.permission!)}>
                {PERMISSION_BUTTON[pending.permission]}
              </button>
            )}
            <button className="btn-ghost" onClick={() => setPending(null)}>Dismiss</button>
          </div>
        </div>
      )}
      {pending?.kind === "error" && (
        <div className="cleanup-confirm">
          <p className="cleanup-lead">Couldn't switch the terminal host: {pending.message}</p>
          <div className="issue-actions">
            <button className="btn-ghost" onClick={() => setPending(null)}>Dismiss</button>
          </div>
        </div>
      )}
    </>
  );
}

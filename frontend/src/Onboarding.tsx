// One-time onboarding window (issue #98) — a single wizard (issues #148, #189).
// On Windows it opens with "Find the brain icon" (#198), shown only while the
// icon is hidden in the taskbar's `^` overflow. Then it connects a GitHub
// identity, picks the default terminal host, and finally chooses the default
// agent and launch at login together. A branded webview dialog — rather than a native
// alert, which can only show the generic OS icon — so it shows the mAIestro
// Code logo and name. The backend opens this window only on the very first run;
// pressing "Get started" (or closing the window, which accepts the defaults)
// records the choices and marks onboarding complete.
//
// The shell (brand, step rail, footer) is fixed; only the step body swaps, so
// moving between steps reads as one wizard rather than two screens.
//
// To add a preference: add a piece of state and a row on the defaults step,
// then thread its value into `completeOnboarding` (and the backend
// `onboarding_complete` command). To add a step, extend `STEPS` and the render
// below — there's no generic step framework.

import { useEffect, useState } from "react";
import { Agent, InstalledTerminalHost, Platform, ResolvedTool, TerminalHost, api } from "./api";
import { GITHUB_PAT_SETUP_URL } from "./components/CredRows";
import { TERMINAL_HOST_PRODUCTS, terminalHostOptions } from "./components/TerminalHostSwitch";
import { TrayDragDemo } from "./components/TrayDragDemo";
import { Wordmark } from "./components/Wordmark";
import { AGENTS, AGENT_MARKS, AGENT_PRODUCTS, AGENT_TOOLS, asAgent } from "./lib/agents";
import LogoIcon from "./icons/logo.svg?react";
import VSCodeIcon from "./icons/vscode.svg?react";
import TerminalIcon from "./icons/terminal.svg?react";
import CmuxIcon from "./icons/cmux.svg?react";
import WindowsTerminalIcon from "./icons/windows-terminal.svg?react";

type Step = "tray" | "github" | "terminal" | "defaults";

const STEPS: { id: Step; label: string }[] = [
  { id: "tray", label: "Find the brain icon" },
  { id: "github", label: "Connect GitHub" },
  { id: "terminal", label: "Choose a terminal host" },
  { id: "defaults", label: "Choose defaults" },
];

/** Which agent to preselect: the saved setting, else the schema default when
 *  it's installed, else the first installed agent, else the schema default. */
export function initialAgent(
  saved: Agent | null | undefined,
  schemaDefault: Agent,
  installed: (a: Agent) => boolean,
): Agent {
  if (saved) return saved;
  if (installed(schemaDefault)) return schemaDefault;
  return AGENTS.find(installed) ?? schemaDefault;
}

const TERMINAL_HOST_MARKS: Record<TerminalHost, typeof VSCodeIcon> = {
  vscode: VSCodeIcon,
  terminal_app: TerminalIcon,
  cmux: CmuxIcon,
  windows_terminal: WindowsTerminalIcon,
};

/** What a terminal host needs before its first session, if anything. */
const TERMINAL_HOST_NOTES: Partial<Record<TerminalHost, string>> = {
  terminal_app: "The first session asks you to let mAIestro Code control Terminal. Allow it.",
  cmux: "Set cmux's Socket Control Mode to Automation or Password (cmux Settings → Automation).",
};

/** Which terminal host to preselect: the saved setting when this OS has it
 *  (a file synced from another OS may name one it doesn't), else the schema
 *  default when this OS has it, else VS Code. */
export function initialTerminalHost(
  saved: TerminalHost | null | undefined,
  schemaDefault: unknown,
  options: TerminalHost[],
): TerminalHost {
  if (saved && options.includes(saved)) return saved;
  const fallback = options.find((h) => h === schemaDefault);
  return fallback ?? "vscode";
}

export function Onboarding() {
  // Whether the GitHub step is skipped — an install that already has an
  // identity (an existing user upgrading in) goes straight to the terminal
  // host step, never
  // re-prompted. `null` while the checks are in flight, so no step renders and
  // the rail can't visibly change mid-flash.
  const [hasIdentities, setHasIdentities] = useState<boolean | null>(null);
  const [step, setStep] = useState<Step>("github");

  const [identityName, setIdentityName] = useState("default");
  const [githubToken, setGithubToken] = useState("");
  const [identityBusy, setIdentityBusy] = useState(false);
  const [identityError, setIdentityError] = useState<string | null>(null);

  const [agent, setAgent] = useState<Agent>("claude");
  const [tools, setTools] = useState<ResolvedTool[] | null>(null);
  const [terminalHost, setTerminalHost] = useState<TerminalHost>("vscode");
  const [hosts, setHosts] = useState<InstalledTerminalHost[] | null>(null);
  const [launchAtLogin, setLaunchAtLogin] = useState(false);
  // The tray lives in the macOS menu bar but the Windows taskbar (#198).
  const [platform, setPlatform] = useState<Platform>("macos");
  // Whether the Windows "Find the brain icon" step is in the wizard: decided
  // once at load (the icon was hidden or unknown), so the rail doesn't change
  // when the user drags the icon out. `trayPromoted` is the live answer.
  const [showTray, setShowTray] = useState(false);
  const [trayPromoted, setTrayPromoted] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    Promise.all([
      api.identitiesList().catch(() => []),
      api.toolsResolved().catch(() => null),
      api.getAppSettings().catch(() => null),
      api.appSettingsSchema().catch(() => null),
      api.platform().catch(() => null),
      api.trayIconPromoted().catch(() => null),
      api.terminalHostsInstalled().catch(() => null),
    ]).then(([ids, resolved, settings, schema, os, promoted, installedHosts]) => {
      if (os) setPlatform(os);
      const tray = os === "windows" && promoted !== true;
      setShowTray(tray);
      setTrayPromoted(promoted);
      const props = (schema?.properties ?? {}) as Record<string, { default?: unknown }>;
      const isInstalled = (a: Agent) =>
        !!resolved?.find((t) => t.tool === AGENT_TOOLS[a])?.exists;
      setTools(resolved);
      setHosts(installedHosts);
      setAgent(initialAgent(settings?.agent, asAgent(props.agent?.default, "claude"), isInstalled));
      setTerminalHost(
        initialTerminalHost(settings?.terminal_host, props.terminal_host?.default, terminalHostOptions(os ?? "macos", null)),
      );
      const has = ids.length > 0;
      setHasIdentities(has);
      setStep(tray ? "tray" : has ? "terminal" : "github");
    });
  }, []);

  // Watch for the user dragging the icon out while the tray step is up.
  // Explorer writes the registry entry a moment after the icon first appears,
  // so `null` early on just means "not yet known".
  useEffect(() => {
    if (step !== "tray") return;
    const id = setInterval(() => {
      api.trayIconPromoted().then(setTrayPromoted, () => {});
    }, 1000);
    return () => clearInterval(id);
  }, [step]);

  async function handleContinue() {
    const token = githubToken.trim();
    if (token) {
      setIdentityBusy(true);
      setIdentityError(null);
      try {
        const name = identityName.trim() || "default";
        await api.setCredential("github_token", { kind: "identity", identity_id: name }, token);
      } catch (e) {
        setIdentityBusy(false);
        setIdentityError(String(e));
        return;
      }
      setIdentityBusy(false);
    }
    setStep("terminal");
  }

  const finish = () => {
    setBusy(true);
    // The command destroys this window; no need to handle the resolved promise.
    api.completeOnboarding(launchAtLogin, agent, terminalHost).catch(() => setBusy(false));
  };

  const steps = STEPS.filter(
    (s) => (s.id !== "tray" || showTray) && (s.id !== "github" || !hasIdentities),
  );
  const stepIndex = steps.findIndex((s) => s.id === step);
  const toolFor = (a: Agent) => tools?.find((t) => t.tool === AGENT_TOOLS[a]);
  const chosenMissing = tools !== null && !toolFor(agent)?.exists;
  const hostFor = (h: TerminalHost) => hosts?.find((x) => x.terminal_host === h);
  const hostMissing = hosts !== null && !hostFor(terminalHost)?.installed;
  const terminalNote = TERMINAL_HOST_NOTES[terminalHost];

  return (
    <main className="onboarding">
      <header className="onboarding-head">
        <div className="onboarding-brand">
          <LogoIcon className="onboarding-logo" aria-hidden="true" />
          <h1 className="onboarding-title">
            <Wordmark />
          </h1>
        </div>
        {hasIdentities !== null && steps.length > 1 && (
          <ol className="onboarding-rail" aria-label="Setup steps">
            {steps.map((s, i) => (
              <li
                key={s.id}
                className={`onboarding-rail-step${i === stepIndex ? " current" : ""}${i < stepIndex ? " done" : ""}`}
                aria-current={i === stepIndex ? "step" : undefined}
              >
                <span className="onboarding-rail-bar" />
                <span className="onboarding-rail-label">
                  {i + 1}. {s.label}
                </span>
              </li>
            ))}
          </ol>
        )}
      </header>

      {hasIdentities !== null && (
        <section className="onboarding-body" key={step}>
          {step === "tray" ? (
            <>
              <h2 className="onboarding-heading">Find the brain icon</h2>
              <p className="onboarding-lead">
                mAIestro Code lives in the taskbar. Windows hides new icons under the{" "}
                <strong>^</strong> arrow, so open it and drag the brain onto the taskbar to keep it
                one click away.
              </p>
              <TrayDragDemo done={trayPromoted === true} />
              {trayPromoted === true ? (
                <p className="onboarding-tray-status found" role="status">
                  The brain icon is on your taskbar.
                </p>
              ) : trayPromoted === false ? (
                <p className="onboarding-tray-status" role="status">
                  Waiting for you to drag it out…
                </p>
              ) : null}
            </>
          ) : step === "github" ? (
            <>
              <h2 className="onboarding-heading">Connect GitHub</h2>
              <p className="onboarding-lead">
                mAIestro Code lists issues and opens pull requests with a personal access token,
                kept in your macOS Keychain.
              </p>

              <div className="onboarding-fields">
                <label className="onboarding-field">
                  <span className="onboarding-field-label">Identity name</span>
                  <input
                    className="text-input"
                    placeholder="default"
                    value={identityName}
                    disabled={identityBusy}
                    onChange={(e) => setIdentityName(e.target.value)}
                  />
                  <span className="onboarding-field-hint">
                    A label for this account, like "work" or "personal". You can add more later.
                  </span>
                </label>
                <label className="onboarding-field">
                  <span className="onboarding-field-label">Personal access token</span>
                  <input
                    className="text-input secret-input"
                    type="password"
                    placeholder="ghp_… or github_pat_…"
                    value={githubToken}
                    disabled={identityBusy}
                    autoFocus
                    onChange={(e) => setGithubToken(e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && handleContinue()}
                  />
                  <span className="onboarding-field-hint">
                    Only for mAIestro Code's own GitHub calls. Sessions keep using your usual git
                    login. Without a token, the identity isn't created yet.
                  </span>
                  <button
                    type="button"
                    className="help-link"
                    onClick={() => { void api.openUrl(GITHUB_PAT_SETUP_URL).catch(() => {}); }}
                  >
                    How to create a token
                  </button>
                </label>
                {identityError && <p className="cred-error">{identityError}</p>}
              </div>
            </>
          ) : step === "terminal" ? (
            <>
              <h2 className="onboarding-heading">Choose a terminal host</h2>
              <p className="onboarding-lead">
                Each new session opens its agent here. A repo can pick its own in its settings.
              </p>

              <div className="onboarding-agents" role="radiogroup" aria-label="Default terminal host">
                {terminalHostOptions(platform, null).map((h) => {
                  const Mark = TERMINAL_HOST_MARKS[h];
                  const found = hostFor(h);
                  return (
                    <label key={h} className={`onboarding-agent${h === terminalHost ? " selected" : ""}`}>
                      <input
                        type="radio"
                        name="terminal-host"
                        value={h}
                        checked={h === terminalHost}
                        onChange={() => setTerminalHost(h)}
                      />
                      <Mark className="onboarding-agent-mark" aria-hidden />
                      <span className="onboarding-agent-name">{TERMINAL_HOST_PRODUCTS[h]}</span>
                      {hosts !== null && (
                        <span
                          className={`onboarding-agent-status${found?.installed ? " found" : ""}`}
                          title={found?.path ?? undefined}
                        >
                          {found?.installed ? "Installed" : "Not installed"}
                        </span>
                      )}
                    </label>
                  );
                })}
              </div>
              {hostMissing ? (
                <p className="onboarding-agent-note">
                  Install {TERMINAL_HOST_PRODUCTS[terminalHost]} before you start a session. mAIestro
                  Code finds it in its usual install location, or you can set its path in Settings.
                </p>
              ) : (
                terminalNote && <p className="onboarding-agent-note">{terminalNote}</p>
              )}
            </>
          ) : (
            <>
              <h2 className="onboarding-heading">Choose defaults</h2>
              <p className="onboarding-lead">
                Your agentic coding CLI starts in every new worktree. A repo can pick its own
                in its settings.
              </p>

              <div className="onboarding-agents" role="radiogroup" aria-label="Default agentic coding CLI">
                {AGENTS.map((a) => {
                  const Mark = AGENT_MARKS[a];
                  const t = toolFor(a);
                  return (
                    <label key={a} className={`onboarding-agent${a === agent ? " selected" : ""}`}>
                      <input
                        type="radio"
                        name="agent"
                        value={a}
                        checked={a === agent}
                        onChange={() => setAgent(a)}
                      />
                      <Mark className="onboarding-agent-mark" aria-hidden />
                      <span className="onboarding-agent-name">{AGENT_PRODUCTS[a]}</span>
                      {tools !== null && (
                        <span
                          className={`onboarding-agent-status${t?.exists ? " found" : ""}`}
                          title={t?.exists ? t.path : undefined}
                        >
                          {t?.exists ? "Installed" : "Not installed"}
                        </span>
                      )}
                    </label>
                  );
                })}
              </div>
              {chosenMissing && (
                <p className="onboarding-agent-note">
                  Install {AGENT_PRODUCTS[agent]} (<code>{AGENT_TOOLS[agent]}</code>) before you start
                  a session. mAIestro Code finds it on your PATH, or you can set its path in
                  Settings.
                </p>
              )}

              <div className="onboarding-option">
                <div className="toggle-text">
                  <div className="onboarding-option-label">Launch at login</div>
                  <div className="onboarding-option-hint">
                    Keep the brain icon in your {platform === "windows" ? "taskbar" : "menu bar"} after
                    a restart.
                  </div>
                </div>
                <button
                  type="button"
                  role="switch"
                  aria-checked={launchAtLogin}
                  aria-label="Launch at login"
                  className={`toggle-switch ${launchAtLogin ? "on" : ""}`}
                  onClick={() => setLaunchAtLogin((v) => !v)}
                >
                  <span className="toggle-knob" />
                </button>
              </div>
            </>
          )}
        </section>
      )}

      {hasIdentities !== null && (
        <footer className="onboarding-foot">
          <span className="onboarding-foot-note">Change any of this later in Settings.</span>
          <div className="onboarding-actions">
            {stepIndex > 0 && (
              <button
                className="onboarding-btn onboarding-btn-secondary"
                disabled={busy || identityBusy}
                onClick={() => setStep(steps[stepIndex - 1].id)}
              >
                Back
              </button>
            )}
            {step === "tray" ? (
              <button className="onboarding-btn" onClick={() => setStep(steps[stepIndex + 1].id)}>
                {trayPromoted === false ? "Skip for now" : "Continue"}
              </button>
            ) : step === "terminal" ? (
              <button className="onboarding-btn" onClick={() => setStep("defaults")}>
                Continue
              </button>
            ) : step === "github" ? (
              <button className="onboarding-btn" disabled={identityBusy} onClick={handleContinue}>
                {identityBusy ? "Saving…" : githubToken.trim() ? "Continue" : "Skip for now"}
              </button>
            ) : (
              <button className="onboarding-btn" disabled={busy} onClick={finish}>
                Get started
              </button>
            )}
          </div>
        </footer>
      )}
    </main>
  );
}

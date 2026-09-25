// One-time onboarding window (issue #98) — a single wizard (issues #148, #189).
// Step 1 connects a GitHub identity; step 2 chooses the default agent and
// launch at login together. A branded webview dialog — rather than a native
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
import { Agent, ResolvedTool, api } from "./api";
import { GITHUB_PAT_SETUP_URL } from "./components/CredRows";
import { AGENTS, AGENT_MARKS, AGENT_PRODUCTS, AGENT_TOOLS, asAgent } from "./lib/agents";
import LogoIcon from "./icons/logo.svg?react";

type Step = "github" | "defaults";

const STEPS: { id: Step; label: string }[] = [
  { id: "github", label: "Connect GitHub" },
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

export function Onboarding() {
  // Whether the GitHub step is skipped — an install that already has an
  // identity (an existing user upgrading in) goes straight to defaults, never
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
  const [launchAtLogin, setLaunchAtLogin] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    Promise.all([
      api.identitiesList().catch(() => []),
      api.toolsResolved().catch(() => null),
      api.getAppSettings().catch(() => null),
      api.appSettingsSchema().catch(() => null),
    ]).then(([ids, resolved, settings, schema]) => {
      const props = (schema?.properties ?? {}) as Record<string, { default?: unknown }>;
      const isInstalled = (a: Agent) =>
        !!resolved?.find((t) => t.tool === AGENT_TOOLS[a])?.exists;
      setTools(resolved);
      setAgent(initialAgent(settings?.agent, asAgent(props.agent?.default, "claude"), isInstalled));
      const has = ids.length > 0;
      setHasIdentities(has);
      setStep(has ? "defaults" : "github");
    });
  }, []);

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
    setStep("defaults");
  }

  const finish = () => {
    setBusy(true);
    // The command destroys this window; no need to handle the resolved promise.
    api.completeOnboarding(launchAtLogin, agent).catch(() => setBusy(false));
  };

  const steps = hasIdentities ? STEPS.filter((s) => s.id !== "github") : STEPS;
  const stepIndex = steps.findIndex((s) => s.id === step);
  const toolFor = (a: Agent) => tools?.find((t) => t.tool === AGENT_TOOLS[a]);
  const chosenMissing = tools !== null && !toolFor(agent)?.exists;

  return (
    <main className="onboarding">
      <header className="onboarding-head">
        <div className="onboarding-brand">
          <LogoIcon className="onboarding-logo" aria-hidden="true" />
          <h1 className="onboarding-title">
            m<span className="ai">AI</span>estro Code
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
          {step === "github" ? (
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
                    Keep the brain icon in your menu bar after a restart.
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
            {step === "defaults" && !hasIdentities && (
              <button
                className="onboarding-btn onboarding-btn-secondary"
                disabled={busy}
                onClick={() => setStep("github")}
              >
                Back
              </button>
            )}
            {step === "github" ? (
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

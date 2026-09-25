// One-time onboarding window (issue #98), now a two-step wizard (issue #148):
// step 1 connects a GitHub identity, step 2 collects preferences. A branded
// webview dialog — rather than a native alert, which can only show the generic
// OS icon — so it shows the mAIestro Code logo and name and can grow more options
// over time. The backend opens this window only on the very first run; pressing
// "Get started" (or closing the window, which accepts the defaults) records the
// choices and marks onboarding complete.
//
// To add a preference: add a piece of state and an `OnboardingOption` row on
// step 2, then thread its value into `completeOnboarding` (and the backend
// `onboarding_complete` command). To add a wizard step, extend the `step` union
// and the render below — there's no generic step framework, just these two.

import { useEffect, useState } from "react";
import { api } from "./api";
import { GITHUB_PAT_SETUP_URL } from "./components/CredRows";
import LogoIcon from "./icons/logo.svg?react";

type Step = 0 | 1;

export function Onboarding() {
  // Whether the identity step is skipped entirely for this session — an install
  // that already has an identity (an existing user upgrading in) goes straight
  // to preferences, never re-prompted. `null` while the check is in flight, so
  // no step content renders and the step count can't visibly change mid-flash.
  const [hasIdentities, setHasIdentities] = useState<boolean | null>(null);
  const [step, setStep] = useState<Step>(0);

  const [identityName, setIdentityName] = useState("default");
  const [githubToken, setGithubToken] = useState("");
  const [identityBusy, setIdentityBusy] = useState(false);
  const [identityError, setIdentityError] = useState<string | null>(null);

  const [launchAtLogin, setLaunchAtLogin] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api.identitiesList().then((list) => {
      const has = list.length > 0;
      setHasIdentities(has);
      setStep(has ? 1 : 0);
    });
  }, []);

  async function handleNext() {
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
    setStep(1);
  }

  const finish = () => {
    setBusy(true);
    // The command destroys this window; no need to handle the resolved promise.
    api.completeOnboarding(launchAtLogin).catch(() => setBusy(false));
  };

  const brand = (
    <div className="onboarding-brand">
      <LogoIcon className="onboarding-logo" aria-hidden="true" />
      <h1 className="onboarding-title">
        m<span className="ai">AI</span>estro Code
      </h1>
    </div>
  );

  if (hasIdentities === null) {
    return <main className="onboarding">{brand}</main>;
  }

  return (
    <main className="onboarding">
      {brand}

      {step === 0 ? (
        <>
          <p className="onboarding-subtitle">Connect a GitHub identity to get started.</p>

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
                A short label for this GitHub account, e.g. "work" or "personal". You can track
                repos under multiple identities later — this just names the first one.
              </span>
            </label>
            <label className="onboarding-field">
              <span className="onboarding-field-label">GitHub personal access token</span>
              <input
                className="text-input secret-input"
                type="password"
                placeholder="Leave blank to skip"
                value={githubToken}
                disabled={identityBusy}
                onChange={(e) => setGithubToken(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && handleNext()}
              />
              <span className="onboarding-field-hint">
                Used only for mAIestro Code's own GitHub API calls (listing issues, opening PRs) — never
                for the git/GitHub auth inside a launched session. Leave blank to skip; the identity
                above still won't be created until a token is added, here or later in Settings.
              </span>
              <button
                type="button"
                className="help-link"
                onClick={() => { void api.openUrl(GITHUB_PAT_SETUP_URL).catch(() => {}); }}
              >
                How to create a token →
              </button>
            </label>
            {identityError && <p className="cred-error">{identityError}</p>}
          </div>

          <button className="onboarding-btn" disabled={identityBusy} onClick={handleNext}>
            {identityBusy ? "…" : "Next"}
          </button>
          <p className="onboarding-foot">You can add or edit identities anytime in Settings.</p>
        </>
      ) : (
        <>
          <p className="onboarding-subtitle">A few preferences to get you started.</p>

          <div className="onboarding-options">
            <OnboardingOption
              label="Launch at login"
              hint="Start mAIestro Code automatically when you log in."
              on={launchAtLogin}
              onToggle={() => setLaunchAtLogin((v) => !v)}
            />
          </div>

          <div className="onboarding-actions">
            {!hasIdentities && (
              <button
                className="onboarding-btn onboarding-btn-secondary"
                disabled={busy}
                onClick={() => setStep(0)}
              >
                Back
              </button>
            )}
            <button className="onboarding-btn" disabled={busy} onClick={finish}>
              Get started
            </button>
          </div>
          <p className="onboarding-foot">You can change these anytime in Settings › General.</p>
        </>
      )}
    </main>
  );
}

/** One toggle-able onboarding preference: a label + hint on the left, an on/off
 *  switch (shared `.toggle-switch` styling with the General panel) on the right. */
function OnboardingOption({
  label,
  hint,
  on,
  onToggle,
}: {
  label: string;
  hint: string;
  on: boolean;
  onToggle: () => void;
}) {
  return (
    <div className="onboarding-option">
      <div className="toggle-text">
        <div className="onboarding-option-label">{label}</div>
        <div className="onboarding-option-hint">{hint}</div>
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={on}
        aria-label={label}
        className={`toggle-switch ${on ? "on" : ""}`}
        onClick={onToggle}
      >
        <span className="toggle-knob" />
      </button>
    </div>
  );
}

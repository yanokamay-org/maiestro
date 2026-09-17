// Read-only "About" block at the bottom of the Preferences panel (issue #126):
// which version of mAIestro Code is running, when it was built, and a link out to
// that version's release notes on GitHub.
//
// Not part of the Preferences JSON Forms — none of this is persisted settings
// (see backend/src/about.rs), so it renders as plain markup after the form.

import { api, AppVersion } from "../api";

export function AboutSection({ info }: { info: AppVersion }) {
  // A dev build carries the *last released* version, so showing a bare "0.2.3"
  // would claim to be that release. Mark it as sitting after it instead.
  const label = info.dev_build ? `${info.version}+dev` : info.version;
  return (
    <div className="settings-group about-group">
      <div className="settings-group-header">
        <span className="about-title">About</span>
      </div>
      <div className="about-row">
        <div className="about-lines">
          <p className="about-version">
            m<span className="ai">AI</span>estro Code {label}
            {info.dev_build && <span className="about-dev-badge">dev build</span>}
          </p>
          <p className="session-hint">
            {info.dev_build && `Local build, after the v${info.version} release · `}
            Built {info.build_date}
            {info.commit && (
              <>
                {" · "}
                <code>{info.commit}</code>
              </>
            )}
          </p>
        </div>
        <button
          className="btn-ghost"
          title={`Release notes for v${info.version}`}
          onClick={() => { void api.openUrl(info.release_url).catch(() => {}); }}
        >
          Release notes
        </button>
      </div>
    </div>
  );
}

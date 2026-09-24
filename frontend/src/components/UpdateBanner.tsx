// The "a newer release is available" strip under the popover header (#182).
// Shown only when the backend's update check says so (newer than the running
// version, public for 5+ days, not dismissed). The version link and View Release
// both open the GitHub Release page in the browser — nothing is auto-installed —
// and × dismisses it for this version (the parent records that via
// `update_dismiss`).

import { api, AvailableUpdate } from "../api";

export function UpdateBanner({ update, onDismiss }: {
  update: AvailableUpdate;
  onDismiss: (version: string) => void;
}) {
  const openRelease = () => { void api.openUrl(update.release_url).catch(() => {}); };
  const tag = `v${update.version}`;
  return (
    <div className="notice-banner notice-banner--update" role="status">
      <span className="notice-banner-text">
        A new version of m<span className="ai">AI</span>estro Code (
        {/* A button styled as a link: navigation goes through the backend's
            open_url (the webview has no browser to hand an href to). */}
        <button className="notice-banner-link" onClick={openRelease} title={`Open the ${tag} release page`}>
          {tag}
        </button>
        ) is available
      </span>
      <button
        className="btn-ghost notice-banner-action"
        title={`Open the ${tag} release page`}
        onClick={openRelease}
      >
        View Release
      </button>
      <button
        className="notice-banner-dismiss"
        onClick={() => onDismiss(update.version)}
        title="Dismiss for this version"
        aria-label={`Dismiss update ${update.version}`}
      >
        ×
      </button>
    </div>
  );
}

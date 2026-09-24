// The "settings.json can't be read" strip under the popover header. Shown while
// ~/.maiestro/settings.json exists but fails to parse or validate — a hand-edit
// typo, typically. While it stands the backend refuses every write to the file
// (so a typo never becomes a silent reset), which means window sizes, dismissed
// updates, and Preferences changes don't stick until it is fixed; this is where
// the user learns that. Reveal selects the file in Finder. Not dismissable: it
// clears by itself on the next popover open once the file loads again.

import { api, SettingsProblem } from "../api";

export function SettingsProblemBanner({ problem }: { problem: SettingsProblem }) {
  // The backend's message starts with the absolute path; show it as the file
  // name so the reason fits on the line (the full text is in the tooltip).
  const reason = problem.message.split(problem.path).join("settings.json");
  return (
    <div className="notice-banner notice-banner--error" role="alert" title={problem.message}>
      <span className="notice-banner-text">
        <strong>Couldn't read settings.json.</strong> {reason} Fix it by hand — nothing is saved until then.
      </span>
      <button
        className="btn-ghost notice-banner-action"
        title={`Reveal ${problem.path} in Finder`}
        onClick={() => { void api.revealPath(problem.path).catch(() => {}); }}
      >
        Reveal
      </button>
    </div>
  );
}

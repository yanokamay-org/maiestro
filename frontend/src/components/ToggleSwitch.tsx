// The on/off switch used by every boolean setting, in both the Preferences
// panel (Launch at login) and the per-repo form (the generic boolean renderer).
// Extracted so the two forms can't drift into two different-looking switches;
// the styling lives in `.toggle-switch` / `.toggle-knob` / `.toggle-field` in
// styles.css.

interface ToggleSwitchProps {
  /** Whether the switch reads as on. */
  on: boolean;
  onChange: (on: boolean) => void;
  /** Accessible name — the setting's label, since the switch has no text. */
  label: string;
  /** Help text shown beside the switch. */
  hint?: string;
}

export function ToggleSwitch({ on, onChange, label, hint }: ToggleSwitchProps) {
  return (
    <div className="toggle-field">
      <button
        type="button"
        role="switch"
        aria-checked={on}
        aria-label={label}
        className={`toggle-switch ${on ? "on" : ""}`}
        onClick={() => onChange(!on)}
      >
        <span className="toggle-knob" />
      </button>
      {hint && <p className="session-hint">{hint}</p>}
    </div>
  );
}

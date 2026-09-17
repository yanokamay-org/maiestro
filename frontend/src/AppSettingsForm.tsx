// JSON Forms rendering for the global app-settings ("Preferences") panel (issue
// #85). Mirrors RepoSettingsForm: the JSON Schema is the backend's hand-written
// spec (fetched via `app_settings_schema`); this module supplies the UI schema
// (which fields to show, in what order — `window`/`settings_window` are hidden as
// machine-managed) and two custom renderers the schema alone can't express:
//   - Theme: the segmented light/dark/system control (a plain enum would render
//     as a dropdown).
//   - Tool paths: one input per CLI (claude/git/code) with a live resolved-path
//     status line.
//   - Terminal font: a text input whose placeholder is the schema default, so an
//     empty field visibly means "use the default" (a plain string control would
//     just look unset).

import {
  ControlProps,
  rankWith,
  scopeEndsWith,
  UISchemaElement,
} from "@jsonforms/core";
import { withJsonFormsControlProps } from "@jsonforms/react";
import { vanillaRenderers, vanillaCells } from "@jsonforms/vanilla-renderers";
import { ResolvedTool, Theme } from "./api";
import { RevealButton, PathMissingHint, usePathExists } from "./PathField";
import { ToggleSwitch } from "./components/ToggleSwitch";

/** Field order; `window`/`settings_window`/`onboarding_completed` are
 *  deliberately omitted (machine-managed). */
export const appSettingsUISchema = {
  type: "VerticalLayout",
  elements: [
    { type: "Control", scope: "#/properties/theme", label: "Theme" },
    { type: "Control", scope: "#/properties/terminal_font_family", label: "Terminal font" },
    { type: "Control", scope: "#/properties/launch_at_login", label: "Launch at login" },
    { type: "Control", scope: "#/properties/tool_paths", label: "Tool paths" },
  ],
} as unknown as UISchemaElement;

/** Extra data the custom renderers read via JsonForms' `config`. */
export interface AppFormConfig {
  showUnfocusedDescription: true;
  /** How each tool currently resolves (from `tools_resolved`), for the status line. */
  resolvedTools: ResolvedTool[];
  /** The `terminal_font_family` schema default, shown as the field's placeholder. */
  terminalFontDefault: string;
}

/** Defaults the form displays, read from the schema's `default` keywords — the
 *  single source of truth (the backend resolves the same ones). Extracted from
 *  the *raw* schema, since `sanitizeSchemaForForm` strips `default` before the
 *  schema reaches JsonForms (so ajv can't inject it into the saved data). */
export interface AppFormDefaults {
  terminalFontDefault: string;
}

export function extractAppFormDefaults(
  schema: Record<string, unknown>,
): AppFormDefaults {
  const props = (schema.properties ?? {}) as Record<string, { default?: unknown }>;
  const def = props.terminal_font_family?.default;
  return { terminalFontDefault: typeof def === "string" ? def : "" };
}

// ── Theme (segmented control) ────────────────────────────────────────────────
// Stored value is "light" | "dark" | "system"; null is treated as "system".

function ThemeControl(props: ControlProps) {
  const { data, handleChange, path, description } = props;
  const value = (data ?? "system") as Theme;
  const opts: Theme[] = ["light", "dark", "system"];
  return (
    <div className="control jsf-control">
      <label className="jsf-label">Theme</label>
      {description && <div className="jsf-help">{description}</div>}
      <div className="theme-options" role="radiogroup" aria-label="Theme">
        {opts.map((opt, idx) => (
          <button
            key={opt}
            className={`theme-option ${value === opt ? "active" : ""}`}
            role="radio"
            aria-checked={value === opt}
            // Roving tabindex + arrow navigation so the group is one tab stop and
            // the arrow keys move between options like a native radio group.
            tabIndex={value === opt ? 0 : -1}
            onKeyDown={(e) => {
              const dir = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1
                : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
              if (!dir) return;
              e.preventDefault();
              const next = (idx + dir + opts.length) % opts.length;
              handleChange(path, opts[next]);
              (e.currentTarget.parentElement?.children[next] as HTMLElement | undefined)?.focus();
            }}
            onClick={() => handleChange(path, opt)}
          >
            {opt === "light" ? "Light" : opt === "dark" ? "Dark" : "System"}
          </button>
        ))}
      </div>
      <p className="session-hint" style={{ paddingTop: 2 }}>
        System follows your macOS appearance.
      </p>
    </div>
  );
}

export const themeTester = rankWith(20, scopeEndsWith("theme"));
export const ThemeRenderer = withJsonFormsControlProps(ThemeControl);

// ── Launch at login (segmented on/off switch) ────────────────────────────────
// Stored value is boolean | null; null is treated as false (issue #98).

function LaunchAtLoginControl(props: ControlProps) {
  const { data, handleChange, path } = props;
  // Title stays flush-left like the other sections (Theme, Tool paths); the switch
  // sits to the left of the help text on the row below it. Help text is
  // deliberately concise — no "change this in Preferences" note, since we're
  // already in Preferences.
  return (
    <div className="control jsf-control">
      <label className="jsf-label">Launch at login</label>
      <ToggleSwitch
        on={data === true}
        onChange={(on) => handleChange(path, on)}
        label="Launch at login"
        hint="Start mAIestro Code automatically when you log in."
      />
    </div>
  );
}

export const launchAtLoginTester = rankWith(20, scopeEndsWith("launch_at_login"));
export const LaunchAtLoginRenderer = withJsonFormsControlProps(LaunchAtLoginControl);

// ── Terminal font ────────────────────────────────────────────────────────────
// The font stack written into each spawned worktree's .vscode/settings.json as
// `terminal.integrated.fontFamily`. Empty means "use the schema default", shown
// as the placeholder — the same empty-means-default shape as the tool paths and
// the repo form's prompt overrides, so nothing here restates the default value.

function TerminalFontControl(props: ControlProps) {
  const { data, handleChange, path, label, description, config } = props;
  const fallback: string = config?.terminalFontDefault ?? "";
  const value = (data as string | null | undefined) ?? "";
  const isOverridden = value.trim() !== "";
  return (
    <div className="control jsf-control">
      <div className="jsf-prompt-head">
        <label className="jsf-label">{label}</label>
        {isOverridden && (
          <button type="button" className="jsf-prompt-reset" onClick={() => handleChange(path, null)}>
            Reset to default
          </button>
        )}
      </div>
      {description && <div className="jsf-help">{description}</div>}
      <input
        className="text-input"
        type="text"
        value={value}
        placeholder={fallback}
        onChange={(e) => handleChange(path, e.target.value.trim() === "" ? null : e.target.value)}
        spellCheck={false}
        autoCapitalize="off"
        autoCorrect="off"
      />
    </div>
  );
}

export const terminalFontTester = rankWith(20, scopeEndsWith("terminal_font_family"));
export const TerminalFontRenderer = withJsonFormsControlProps(TerminalFontControl);

// ── Tool paths ───────────────────────────────────────────────────────────────
// One input per directly-invoked CLI. Empty = auto-resolve; the resolved path (or
// "Not found") is shown beneath each input, so it's clear what an empty field
// falls back to and whether the current setting actually points at a real binary.
//
// Which tools exist, in what order, and what each one is for all come from the
// backend's schema (`tool_paths.properties`) — this renderer keeps no list of its
// own. The schema is the spec for the file format *and* the copy shown here, so a
// tool added or re-described there needs no matching frontend edit.

/** The per-tool rows to render, read off the `tool_paths` sub-schema: the
 *  property key is the tool name, its `description` the help line. Object key
 *  order is the schema's declaration order, which is the order we show. */
function toolFields(schema: ControlProps["schema"]): { key: string; help?: string }[] {
  const props = (schema?.properties ?? {}) as Record<string, { description?: string }>;
  return Object.entries(props).map(([key, sub]) => ({ key, help: sub?.description }));
}

function ToolPathsControl(props: ControlProps) {
  const { data, handleChange, path, label, description, config, schema } = props;
  const paths = (data ?? {}) as Record<string, string | null | undefined>;
  const resolved: ResolvedTool[] = config?.resolvedTools ?? [];

  const setPath = (key: string, v: string) => {
    const next = v.trim() === "" ? null : v;
    handleChange(path, { ...paths, [key]: next });
  };

  return (
    <div className="control jsf-control">
      <label className="jsf-label">{label}</label>
      {description && <div className="jsf-help">{description}</div>}
      {toolFields(schema).map((field) => (
        <ToolPathRow
          key={field.key}
          field={field}
          override={paths[field.key]}
          resolved={resolved.find((t) => t.tool === field.key)}
          onChange={(v) => setPath(field.key, v)}
          onReset={() => handleChange(path, { ...paths, [field.key]: null })}
        />
      ))}
    </div>
  );
}

/** One CLI path override: the input, a reveal button, a reset-to-auto action,
 *  and the resolved-path status. When overridden, the missing-path hint validates
 *  the override; when empty, the existing auto-resolution status line shows. */
function ToolPathRow({
  field,
  override,
  resolved,
  onChange,
  onReset,
}: {
  field: { key: string; help?: string };
  override: string | null | undefined;
  resolved: ResolvedTool | undefined;
  onChange: (v: string) => void;
  onReset: () => void;
}) {
  const { key: name, help } = field;
  const isOverridden = override != null && override !== "";
  const placeholder = resolved?.exists ? resolved.path : "auto-detect";
  // Validate the override the user typed. When empty, the auto-resolution status
  // line below covers "found / not found", so no separate hint is needed.
  const overrideExists = usePathExists(isOverridden ? (override as string) : "");
  return (
    <div className="jsf-prompt-field">
      <div className="jsf-prompt-head">
        <span className="jsf-prompt-name">{name}</span>
        {isOverridden && (
          <button type="button" className="jsf-prompt-reset" onClick={onReset}>
            Reset to auto
          </button>
        )}
      </div>
      {help && <div className="jsf-help">{help}</div>}
      <div className="path-field-row">
        <input
          className="text-input"
          type="text"
          value={override ?? ""}
          placeholder={placeholder}
          onChange={(e) => onChange(e.target.value)}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
        />
        <RevealButton
          path={isOverridden ? (override as string) : resolved?.exists ? resolved.path : ""}
          exists={isOverridden ? overrideExists : resolved?.exists ?? false}
        />
      </div>
      {isOverridden ? (
        <PathMissingHint
          path={override}
          exists={overrideExists}
          label="Not found at this path."
        />
      ) : resolved?.exists ? (
        <div className="jsf-help">
          Using <code>{resolved.path}</code>
        </div>
      ) : (
        <div className="jsf-help jsf-tool-missing">
          Not found — set a path above, or install it on your PATH.
        </div>
      )}
    </div>
  );
}

export const toolPathsTester = rankWith(20, scopeEndsWith("tool_paths"));
export const ToolPathsRenderer = withJsonFormsControlProps(ToolPathsControl);

// Custom renderers first so they out-rank the vanilla defaults for their scopes.
export const appSettingsRenderers = [
  { tester: themeTester, renderer: ThemeRenderer },
  { tester: launchAtLoginTester, renderer: LaunchAtLoginRenderer },
  { tester: terminalFontTester, renderer: TerminalFontRenderer },
  { tester: toolPathsTester, renderer: ToolPathsRenderer },
  ...vanillaRenderers,
];

export const appSettingsCells = vanillaCells;

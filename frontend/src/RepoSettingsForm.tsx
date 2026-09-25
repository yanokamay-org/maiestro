// JSON Forms rendering for the per-repo settings detail view (issue #57).
//
// The JSON Schema is the backend's hand-written spec, fetched via
// `repo_settings_schema`. This module supplies the *presentation* layer: a
// hand-written UI schema that orders the fields and hides `repo`/`hidden`, plus
// two custom renderers for the cases the schema alone can't express — the
// identity select (dynamic options) and the env-files list (Scan/Add/Remove).
//
// Everything else (cloned repo dir, worktree prefix) falls through to the vanilla
// string-input renderer, styled in styles.css.

import { useState } from "react";
import {
  ControlProps,
  isBooleanControl,
  rankWith,
  scopeEndsWith,
  UISchemaElement,
} from "@jsonforms/core";
import { withJsonFormsControlProps } from "@jsonforms/react";
import { vanillaRenderers, vanillaCells } from "@jsonforms/vanilla-renderers";
import { Agent, api } from "./api";
import { AGENTS, AGENT_PRODUCTS, asAgent } from "./lib/agents";
import { PathField, RevealButton, usePathExists } from "./PathField";
import { ToggleSwitch } from "./components/ToggleSwitch";

/** Field order, with `repo` and `hidden` deliberately omitted (the latter is
 *  managed from the popover, not this form). */
export const repoSettingsUISchema = {
  type: "VerticalLayout",
  elements: [
    { type: "Control", scope: "#/properties/cloned_repo_dir", label: "Cloned repo" },
    { type: "Control", scope: "#/properties/worktree_prefix", label: "Worktree prefix" },
    { type: "Control", scope: "#/properties/identity_id", label: "Identity" },
    { type: "Control", scope: "#/properties/env_files", label: "Environment files" },
    { type: "Control", scope: "#/properties/post_spawn_commands", label: "Post-spawn commands" },
    { type: "Control", scope: "#/properties/comment_on_spawn", label: "Comment on the issue when spawning" },
    { type: "Control", scope: "#/properties/delete_remote_on_teardown", label: "Delete remote branch on teardown" },
    { type: "Control", scope: "#/properties/agent", label: "Agentic Coding CLI" },
    { type: "Control", scope: "#/properties/prompt_models", label: "Prompt model" },
    { type: "Control", scope: "#/properties/prompts" },
  ],
} as unknown as UISchemaElement;

/** Extra data the custom renderers need, threaded through JsonForms' `config`. */
export interface RepoFormConfig {
  /** Identities offered by the identity select. */
  knownIdentities: string[];
  /** Current cloned repo dir, so the env-files Scan knows where to look. */
  clonedRepoDir: string | null;
  /** Always show schema descriptions as help text, not only on focus. */
  showUnfocusedDescription: true;
  /** The global `agent` (Preferences, else its schema default) — what a repo
   *  whose own `agent` is null uses. Named in the "Use global default" option. */
  globalAgent: Agent;
  /** This repo's effective agent (its own, else `globalAgent`): picks which
   *  `prompt_models` entry the Prompt model field edits. */
  repoAgent: Agent;
  /** Each agent's drafting-model schema default ("" = none). */
  promptModelDefaults: Record<Agent, string>;
}

// ── Identity select ─────────────────────────────────────────────────────────
// Options come from the live identity list, which a static schema enum can't
// express, so this replaces the vanilla string input for `identity_id`.

/** Shared header: bold label over a muted description (VS Code settings style),
 *  with the control rendered below by the caller. */
function FieldHeading({ label, description }: { label?: string; description?: string }) {
  return (
    <>
      <label className="jsf-label">{label}</label>
      {description && <div className="jsf-help">{description}</div>}
    </>
  );
}

function IdentityControl(props: ControlProps) {
  const { data, handleChange, path, label, description, config } = props;
  const identities: string[] = config?.knownIdentities ?? [];
  return (
    <div className="control jsf-control">
      <FieldHeading label={label} description={description} />
      {identities.length === 0 ? (
        <p className="session-hint" style={{ paddingTop: 2 }}>
          No identities configured. Go to the Identity tab first.
        </p>
      ) : (
        <select
          className="text-input profile-select"
          value={data ?? ""}
          onChange={(e) => handleChange(path, e.target.value || null)}
        >
          <option value="">— none —</option>
          {identities.map((iid) => (
            <option key={iid} value={iid}>{iid}</option>
          ))}
        </select>
      )}
    </div>
  );
}

export const identityTester = rankWith(20, scopeEndsWith("identity_id"));
export const IdentityRenderer = withJsonFormsControlProps(IdentityControl);

// ── Plain text fields (cloned repo dir, worktree prefix) ────────────────────
// A text input with the label/description heading above it. `worktree_prefix`
// surfaces its schema default as a disabled-looking placeholder so it's clear
// what an empty field resolves to.

function ClonedRepoDirControl(props: ControlProps) {
  const { data, handleChange, path, label, description } = props;
  return (
    <div className="control jsf-control">
      <FieldHeading label={label} description={description} />
      <PathField
        value={data ?? ""}
        placeholder="~/src/repo-name"
        onChange={(v) => handleChange(path, v || null)}
        missingLabel="Directory not found"
      />
    </div>
  );
}

export const clonedRepoDirTester = rankWith(20, scopeEndsWith("cloned_repo_dir"));
export const ClonedRepoDirRenderer = withJsonFormsControlProps(ClonedRepoDirControl);

/** The directory a worktree prefix lives in. The prefix itself is a string
 *  base (`~/src/work-`) that's never a real path — the parent (`~/src`) is what
 *  must exist, so that's what we validate and reveal. */
export function prefixParentDir(prefix: string): string {
  const slash = prefix.lastIndexOf("/");
  return slash >= 0 ? prefix.slice(0, slash) : "";
}

function WorktreePrefixControl(props: ControlProps) {
  const { data, handleChange, path, label, description, config } = props;
  const value: string = data ?? "";
  return (
    <div className="control jsf-control">
      <FieldHeading label={label} description={description} />
      <PathField
        value={value}
        placeholder={config?.worktreePrefixDefault ?? ""}
        onChange={(v) => handleChange(path, v || null)}
        checkPath={prefixParentDir(value)}
        missingLabel="Parent directory not found"
        className="jsf-default-hint"
      />
    </div>
  );
}

export const worktreePrefixTester = rankWith(20, scopeEndsWith("worktree_prefix"));
export const WorktreePrefixRenderer = withJsonFormsControlProps(WorktreePrefixControl);

// ── Booleans (on/off switches) ───────────────────────────────────────────────
// One renderer for every boolean in the schema rather than one per field, so a
// boolean added to the schema needs no frontend edit (the same rule the Tool
// paths rows follow). It replaces the vanilla checkbox so these read like the
// Launch-at-login switch in Preferences.
//
// These fields are `boolean | null`, where null means "use the schema default" —
// and `sanitizeSchemaForForm` strips `default` before JsonForms sees it, so a
// null would otherwise render as *off* while the effective value is *on*. The
// switch therefore falls back to the extracted default, passed via `config`.

function BooleanControl(props: ControlProps) {
  const { data, handleChange, path, label, description, config } = props;
  const fallback: boolean = config?.booleanDefaults?.[path] ?? false;
  const on = data ?? fallback;
  return (
    <div className="control jsf-control">
      <label className="jsf-label">{label}</label>
      <ToggleSwitch
        on={on}
        onChange={(next) => handleChange(path, next)}
        label={label ?? path}
        hint={description}
      />
    </div>
  );
}

export const booleanTester = rankWith(10, isBooleanControl);
export const BooleanRenderer = withJsonFormsControlProps(BooleanControl);

// ── Agent select ─────────────────────────────────────────────────────────────
// null means "use the global default", which a plain enum dropdown can't label
// with the value it resolves to — so the first option names the global agent.

function AgentControl(props: ControlProps) {
  const { data, handleChange, path, label, description, config } = props;
  const global: Agent = config?.globalAgent ?? "claude";
  return (
    <div className="control jsf-control">
      <FieldHeading label={label} description={description} />
      <select
        className="text-input profile-select"
        value={data ?? ""}
        onChange={(e) => handleChange(path, e.target.value === "" ? null : e.target.value)}
      >
        <option value="">Use global default ({AGENT_PRODUCTS[global]})</option>
        {AGENTS.map((a) => (
          <option key={a} value={a}>
            {AGENT_PRODUCTS[a]}
          </option>
        ))}
      </select>
    </div>
  );
}

export const agentTester = rankWith(20, scopeEndsWith("agent"));
export const AgentRenderer = withJsonFormsControlProps(AgentControl);

// ── Prompt model combobox ────────────────────────────────────────────────────
// Which model runs the headless drafting prompts. The value is stored per agent
// (`prompt_models.claude` / `.codex` / `.antigravity`) and this field edits the entry for the
// repo's *effective* agent, so switching the Agent select swaps which entry
// shows. Deliberately NOT a closed select: the value is passed verbatim to
// `claude --model` / `codex exec --model` / `agy --model`, which accept any
// alias or model id they know (`agy` has no aliases: ids carry an effort suffix). A datalist offers each agent's known models as suggestions while still
// accepting a typed-in value, so a new model needs no mAIestro Code update. Empty
// falls back to the entry's schema default, surfaced as the placeholder.

/** Suggested model names per agent. Hints only — any value is accepted, so
 *  adding a newly released model here is optional and non-breaking. The Codex
 *  list is the user-selectable (`visibility: "list"`) slugs from Codex's own
 *  catalog (`codex debug models`); empty still uses Codex's configured default.
 *  The Antigravity list is from `agy models`. */
const PROMPT_MODEL_SUGGESTIONS: Record<Agent, string[]> = {
  claude: ["haiku", "sonnet", "opus", "fable"],
  codex: ["gpt-6-luna", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5"],
  antigravity: ["gemini-3.8-flash-low", "gemini-3.8-flash-medium", "gemini-3.8-flash-high", "gemini-3.1-pro-low"],
};

/** Placeholder for an empty drafting-model field on `agent`. */
export function promptModelPlaceholder(agent: Agent, schemaDefault: string): string {
  if (schemaDefault) return schemaDefault;
  return agent === "codex" ? "Codex's configured default" : "";
}

function PromptModelsControl(props: ControlProps) {
  const { data, handleChange, path, label, description, config } = props;
  const agent: Agent = asAgent(config?.repoAgent, "claude");
  const models = (data ?? {}) as Partial<Record<Agent, string | null>>;
  const listId = `prompt-model-suggestions-${agent}`;
  const suggestions = PROMPT_MODEL_SUGGESTIONS[agent];
  return (
    <div className="control jsf-control">
      <FieldHeading label={label} description={description} />
      <input
        className="text-input jsf-default-hint"
        type="text"
        list={suggestions.length ? listId : undefined}
        value={models[agent] ?? ""}
        placeholder={promptModelPlaceholder(agent, config?.promptModelDefaults?.[agent] ?? "")}
        onChange={(e) => handleChange(path, { ...models, [agent]: e.target.value.trim() || null })}
        aria-label={`${AGENT_PRODUCTS[agent]} drafting model`}
        spellCheck={false}
        autoCapitalize="off"
        autoCorrect="off"
      />
      <div className="jsf-help">Editing the model for {AGENT_PRODUCTS[agent]}, this repo&apos;s agentic coding CLI.</div>
      {suggestions.length > 0 && (
        <datalist id={listId}>
          {suggestions.map((m) => (
            <option key={m} value={m} />
          ))}
        </datalist>
      )}
    </div>
  );
}

export const promptModelsTester = rankWith(20, scopeEndsWith("prompt_models"));
export const PromptModelsRenderer = withJsonFormsControlProps(PromptModelsControl);

// ── Env files list ──────────────────────────────────────────────────────────
// A list with Scan / Add / Remove, preserving the existing scan flow. The array
// is written back wholesale via handleChange.

function EnvFilesControl(props: ControlProps) {
  const { data, handleChange, path, label, description, config } = props;
  const files: string[] = Array.isArray(data) ? data : [];
  const clonedRepoDir: string | null = config?.clonedRepoDir ?? null;

  const [adding, setAdding] = useState(false);
  const [input, setInput] = useState("");
  const [scan, setScan] = useState<"idle" | "scanning" | "done">("idle");

  const setFiles = (next: string[]) => handleChange(path, next);

  async function doScan() {
    if (!clonedRepoDir) return;
    setScan("scanning");
    try {
      const found = await api.scanEnvFiles(clonedRepoDir);
      setFiles(found);
      setScan("done");
      setTimeout(() => setScan("idle"), 2000);
    } catch {
      setScan("idle");
    }
  }

  function addFile() {
    const p = input.trim();
    if (!p) return;
    if (!files.includes(p)) setFiles([...files, p]);
    setInput("");
    setAdding(false);
  }

  function removeFile(p: string) {
    setFiles(files.filter((f) => f !== p));
  }

  return (
    <div className="control jsf-control">
      <div className="settings-group-header">
        <span className="jsf-label" style={{ marginBottom: 0 }}>{label}</span>
        <div style={{ display: "flex", gap: 4 }}>
          {clonedRepoDir && (
            <button
              className={`btn-add ${scan === "scanning" ? "btn-busy" : ""}`}
              disabled={scan === "scanning"}
              onClick={doScan}
            >
              {scan === "scanning" ? "…" : scan === "done" ? "✓ Scanned" : "Scan"}
            </button>
          )}
          <button className="btn-add" onClick={() => setAdding(true)}>+ Add</button>
        </div>
      </div>

      {description && <div className="jsf-help">{description}</div>}

      {adding && (
        <div className="cred-controls">
          <input
            className="text-input"
            type="text"
            aria-label="Environment file path"
            placeholder=".env or subdir/.env"
            value={input}
            autoFocus
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") addFile();
              if (e.key === "Escape") { setAdding(false); setInput(""); }
            }}
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
          />
          <button className="btn-save" disabled={!input.trim()} onClick={addFile}>Add</button>
          <button className="btn-clear" onClick={() => { setAdding(false); setInput(""); }}>✕</button>
        </div>
      )}

      {files.length === 0 && !adding ? (
        <p className="session-hint" style={{ paddingTop: 2 }}>
          No env files. Use Scan to find .env files in the cloned repo directory.
        </p>
      ) : (
        <div className="env-file-list">
          {files.map((f) => (
            <EnvFileRow
              key={f}
              file={f}
              clonedRepoDir={clonedRepoDir}
              onRemove={() => removeFile(f)}
            />
          ))}
        </div>
      )}
    </div>
  );
}

/** Resolve a cloned-repo-relative env-file entry (e.g. `.env` or `sub/.env`)
 *  against the repo's cloned repo dir — that's where the backend reads and copies
 *  them from. Returns "" when there's no cloned repo dir to resolve against
 *  (can't validate). */
export function resolveEnvFile(clonedRepoDir: string | null, rel: string): string {
  if (!clonedRepoDir) return "";
  return `${clonedRepoDir.replace(/\/+$/, "")}/${rel}`;
}

/** One env-file entry: the path, a reveal-in-Finder button, a remove button,
 *  and a "not found" hint when the file is missing. The entry is relative to the
 *  cloned repo dir, so validation/reveal resolves it there. */
function EnvFileRow({
  file,
  clonedRepoDir,
  onRemove,
}: {
  file: string;
  clonedRepoDir: string | null;
  onRemove: () => void;
}) {
  const full = resolveEnvFile(clonedRepoDir, file);
  const exists = usePathExists(full);
  return (
    <div className="env-file-row">
      <span className={`env-file-path ${exists === false ? "jsf-tool-missing" : ""}`}>
        {file}
      </span>
      <RevealButton path={full} exists={exists} />
      <button className="btn-clear" onClick={onRemove} title="Remove">✕</button>
    </div>
  );
}

export const envFilesTester = rankWith(20, scopeEndsWith("env_files"));
export const EnvFilesRenderer = withJsonFormsControlProps(EnvFilesControl);

// ── Post-spawn commands ─────────────────────────────────────────────────────
// An ordered, editable list of shell commands run in a new worktree after it's
// created (e.g. `pnpm install`). Empty by default.

function PostSpawnCommandsControl(props: ControlProps) {
  const { data, handleChange, path, label, description } = props;
  const commands: string[] = Array.isArray(data) ? data : [];

  const setCommands = (next: string[]) => handleChange(path, next);
  const updateAt = (i: number, value: string) =>
    setCommands(commands.map((c, idx) => (idx === i ? value : c)));
  const removeAt = (i: number) => setCommands(commands.filter((_, idx) => idx !== i));
  const add = () => setCommands([...commands, ""]);

  return (
    <div className="control jsf-control">
      <div className="settings-group-header">
        <span className="jsf-label" style={{ marginBottom: 0 }}>{label}</span>
        <button className="btn-add" onClick={add}>+ Add</button>
      </div>

      {description && <div className="jsf-help">{description}</div>}

      {commands.length === 0 ? (
        <p className="session-hint" style={{ paddingTop: 2 }}>
          No post-spawn commands. Add one (e.g. <code>pnpm install</code>) to run it in each new worktree.
        </p>
      ) : (
        <div className="env-file-list">
          {commands.map((c, i) => (
            // Index key: the list is small and edited in place; order is stable.
            <div key={i} className="cred-controls">
              <input
                className="text-input jsf-cmd-input"
                type="text"
                aria-label={`Post-spawn command ${i + 1}`}
                placeholder="e.g. pnpm install"
                value={c}
                onChange={(e) => updateAt(i, e.target.value)}
                spellCheck={false}
                autoCapitalize="off"
                autoCorrect="off"
              />
              <button className="btn-clear" onClick={() => removeAt(i)} title="Remove">✕</button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export const postSpawnCommandsTester = rankWith(20, scopeEndsWith("post_spawn_commands"));
export const PostSpawnCommandsRenderer = withJsonFormsControlProps(PostSpawnCommandsControl);

// ── AI prompt overrides ─────────────────────────────────────────────────────
// One paragraph (textarea) per AI prompt. Each shows the built-in default text
// (the schema's `default` keywords, extracted by `extractFormDefaults` and
// passed via config); editing it stores a
// per-repo override. Clearing the field or matching the default again removes
// the override. The runtime context (idea / issue / diff) is appended by the
// backend, so an override only needs the instruction.

type PromptKey = "draft_issue" | "short_label" | "draft_pr";

const PROMPT_FIELDS: { key: PromptKey; label: string; help: string }[] = [
  {
    key: "draft_issue",
    label: "Draft issue from idea",
    help: "Turns your free-text idea into a GitHub issue. Your idea is appended automatically.",
  },
  {
    key: "short_label",
    label: "Summarize issue as label",
    help: "Compresses an existing issue into a short workspace label. The issue title and body are appended automatically.",
  },
  {
    key: "draft_pr",
    label: "Draft pull request",
    help: "Writes a PR description from your changes. The issue and the diff are appended automatically.",
  },
];

function PromptsControl(props: ControlProps) {
  const { data, handleChange, path, config } = props;
  const defaults: Partial<Record<PromptKey, string>> = config?.promptDefaults ?? {};
  const overrides = (data ?? {}) as Partial<Record<PromptKey, string | null>>;

  function setOverride(key: PromptKey, text: string) {
    const def = defaults[key] ?? "";
    // Empty or back-to-default means "no override".
    const next = text.trim() === "" || text === def ? null : text;
    handleChange(path, { ...overrides, [key]: next });
  }

  return (
    <div className="control jsf-control jsf-prompts">
      <label className="jsf-label">AI prompts</label>
      <div className="jsf-help">
        Instructions sent to this repo&apos;s agentic coding CLI. Leave as the default, or override with
        your own text (a custom prompt, or a /skill invocation).
      </div>
      {PROMPT_FIELDS.map(({ key, label, help }) => {
        const override = overrides[key];
        const isOverridden = override != null && override !== "";
        const value = override ?? defaults[key] ?? "";
        return (
          <div key={key} className="jsf-prompt-field">
            <div className="jsf-prompt-head">
              <span className="jsf-prompt-name">{label}</span>
              {isOverridden && (
                <button
                  type="button"
                  className="jsf-prompt-reset"
                  onClick={() => handleChange(path, { ...overrides, [key]: null })}
                >
                  Reset to default
                </button>
              )}
            </div>
            <div className="jsf-help">{help}</div>
            <textarea
              className={`text-input jsf-textarea ${isOverridden ? "" : "jsf-textarea-default"}`}
              value={value}
              rows={4}
              spellCheck={false}
              onChange={(e) => setOverride(key, e.target.value)}
            />
          </div>
        );
      })}
    </div>
  );
}

export const promptsTester = rankWith(20, scopeEndsWith("prompts"));
export const PromptsRenderer = withJsonFormsControlProps(PromptsControl);

// Custom renderers first so they out-rank the vanilla defaults for their scopes.
export const repoSettingsRenderers = [
  { tester: identityTester, renderer: IdentityRenderer },
  { tester: clonedRepoDirTester, renderer: ClonedRepoDirRenderer },
  { tester: worktreePrefixTester, renderer: WorktreePrefixRenderer },
  { tester: envFilesTester, renderer: EnvFilesRenderer },
  { tester: postSpawnCommandsTester, renderer: PostSpawnCommandsRenderer },
  { tester: agentTester, renderer: AgentRenderer },
  { tester: promptModelsTester, renderer: PromptModelsRenderer },
  { tester: promptsTester, renderer: PromptsRenderer },
  { tester: booleanTester, renderer: BooleanRenderer },
  ...vanillaRenderers,
];

export const repoSettingsCells = vanillaCells;

/** Prepare the fetched schema for JsonForms. Strips `$schema`/`$id` (the bundled
 *  draft-07 ajv doesn't recognize the draft 2020-12 meta-schema; the backend
 *  still validates with the real one), and strips every `default` keyword
 *  recursively so JsonForms doesn't auto-inject defaults into our null-means-use-
 *  default fields. The defaults still live in the schema (their single source);
 *  the form reads them via `extractFormDefaults` and renders them itself. */
export function sanitizeSchemaForForm(
  schema: Record<string, unknown>,
): Record<string, unknown> {
  const strip = (node: unknown): unknown => {
    if (Array.isArray(node)) return node.map(strip);
    if (node && typeof node === "object") {
      const out: Record<string, unknown> = {};
      for (const [k, v] of Object.entries(node as Record<string, unknown>)) {
        if (k === "$schema" || k === "$id" || k === "default") continue;
        out[k] = strip(v);
      }
      return out;
    }
    return node;
  };
  return strip(schema) as Record<string, unknown>;
}

/** Default values to display in the form, read from the schema's `default`
 *  keywords — the single source of truth for these defaults (the backend reads
 *  the same schema). Passed to the custom renderers via JsonForms `config`. */
export interface RepoFormDefaults {
  worktreePrefixDefault: string;
  /** Each agent's drafting-model default ("" = none, e.g. Codex's own model). */
  promptModelDefaults: Record<Agent, string>;
  /** Every boolean property's default, keyed by property name. Collected
   *  generically so a boolean added to the schema needs no change here. */
  booleanDefaults: Record<string, boolean>;
  promptDefaults: Record<PromptKey, string>;
}

export function extractFormDefaults(
  schema: Record<string, unknown>,
): RepoFormDefaults {
  const props = (schema.properties ?? {}) as Record<string, { default?: unknown }>;
  const promptProps =
    ((props.prompts as { properties?: Record<string, { default?: unknown }> })?.properties) ?? {};
  const modelProps =
    ((props.prompt_models as { properties?: Record<string, { default?: unknown }> })?.properties) ?? {};
  const str = (v: unknown) => (typeof v === "string" ? v : "");
  const booleanDefaults: Record<string, boolean> = {};
  for (const [key, prop] of Object.entries(props)) {
    if (typeof prop?.default === "boolean") booleanDefaults[key] = prop.default;
  }
  return {
    worktreePrefixDefault: str(props.worktree_prefix?.default),
    promptModelDefaults: {
      claude: str(modelProps.claude?.default),
      codex: str(modelProps.codex?.default),
      antigravity: str(modelProps.antigravity?.default),
    },
    booleanDefaults,
    promptDefaults: {
      draft_issue: str(promptProps.draft_issue?.default),
      short_label: str(promptProps.short_label?.default),
      draft_pr: str(promptProps.draft_pr?.default),
    },
  };
}

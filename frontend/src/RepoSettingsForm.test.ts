import { describe, it, expect } from "vitest";
import {
  extractFormDefaults,
  sanitizeSchemaForForm,
  prefixParentDir,
  resolveEnvFile,
  promptModelPlaceholder,
} from "./RepoSettingsForm";

// A trimmed shape of the real repo-settings schema, enough to exercise default
// extraction and sanitization without pulling the whole file in.
const SCHEMA = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "repo-settings",
  type: "object",
  properties: {
    worktree_prefix: { type: ["string", "null"], default: "~/src/work-" },
    prompt_models: {
      type: ["object", "null"],
      properties: {
        claude: { type: ["string", "null"], default: "haiku" },
        codex: { type: ["string", "null"], default: null },
        antigravity: { type: ["string", "null"], default: "gemini-3.8-flash-low" },
      },
    },
    cloned_repo_dir: { type: ["string", "null"] },
    comment_on_spawn: { type: ["boolean", "null"], default: true },
    delete_remote_on_teardown: { type: ["boolean", "null"], default: true },
    prompts: {
      type: "object",
      properties: {
        draft_issue: { type: ["string", "null"], default: "Draft an issue." },
        short_label: { type: ["string", "null"], default: "Short label." },
        draft_pr: { type: ["string", "null"], default: "Draft a PR." },
      },
    },
  },
};

describe("extractFormDefaults", () => {
  it("reads the top-level and nested prompt defaults from the schema", () => {
    const d = extractFormDefaults(SCHEMA);
    expect(d.worktreePrefixDefault).toBe("~/src/work-");
    // Per agent: Claude's alias default, no default at all for Codex, and a
    // Gemini Flash id for Antigravity.
    expect(d.promptModelDefaults).toEqual({ claude: "haiku", codex: "", antigravity: "gemini-3.8-flash-low" });
    expect(d.promptDefaults).toEqual({
      draft_issue: "Draft an issue.",
      short_label: "Short label.",
      draft_pr: "Draft a PR.",
    });
  });

  // Collected generically from the schema, so a boolean added there needs no
  // change to the form — and a `true` default must survive, since that's what a
  // null (= "use the default") field renders as.
  it("collects every boolean property's default, keyed by name", () => {
    const d = extractFormDefaults(SCHEMA);
    expect(d.booleanDefaults).toEqual({
      comment_on_spawn: true,
      delete_remote_on_teardown: true,
    });
  });

  it("falls back to empty strings when defaults are missing", () => {
    const d = extractFormDefaults({ properties: {} });
    expect(d.worktreePrefixDefault).toBe("");
    expect(d.promptModelDefaults).toEqual({ claude: "", codex: "", antigravity: "" });
    expect(d.booleanDefaults).toEqual({});
    expect(d.promptDefaults).toEqual({ draft_issue: "", short_label: "", draft_pr: "" });
  });

  it("tolerates a schema with no properties at all", () => {
    const d = extractFormDefaults({});
    expect(d.worktreePrefixDefault).toBe("");
    expect(d.promptDefaults.draft_pr).toBe("");
  });
});

describe("sanitizeSchemaForForm", () => {
  it("strips $schema, $id, and every default recursively", () => {
    const clean = sanitizeSchemaForForm(SCHEMA) as any;
    expect(clean.$schema).toBeUndefined();
    expect(clean.$id).toBeUndefined();
    expect(clean.properties.worktree_prefix.default).toBeUndefined();
    expect(clean.properties.prompts.properties.draft_issue.default).toBeUndefined();
    // Non-default keys survive.
    expect(clean.properties.worktree_prefix.type).toEqual(["string", "null"]);
    expect(clean.type).toBe("object");
  });

  it("does not mutate the input schema", () => {
    const before = JSON.stringify(SCHEMA);
    sanitizeSchemaForForm(SCHEMA);
    expect(JSON.stringify(SCHEMA)).toBe(before);
  });
});

describe("prefixParentDir", () => {
  it("returns everything before the last slash (the dir the prefix lives in)", () => {
    expect(prefixParentDir("~/src/work-")).toBe("~/src");
    expect(prefixParentDir("/abs/path/work-")).toBe("/abs/path");
  });

  it("returns empty when there is no slash", () => {
    expect(prefixParentDir("work-")).toBe("");
    expect(prefixParentDir("")).toBe("");
  });
});

describe("resolveEnvFile", () => {
  it("joins a relative entry onto the cloned repo dir", () => {
    expect(resolveEnvFile("~/src/widget", ".env")).toBe("~/src/widget/.env");
    expect(resolveEnvFile("~/src/widget", "frontend/.env.local")).toBe(
      "~/src/widget/frontend/.env.local",
    );
  });

  it("collapses a trailing slash on the cloned repo dir", () => {
    expect(resolveEnvFile("~/src/widget/", ".env")).toBe("~/src/widget/.env");
    expect(resolveEnvFile("~/src/widget///", ".env")).toBe("~/src/widget/.env");
  });

  it("returns empty when there is no cloned repo dir to resolve against", () => {
    expect(resolveEnvFile(null, ".env")).toBe("");
  });
});

describe("promptModelPlaceholder", () => {
  // An empty field means "the default": Claude's schema alias, or — with no
  // schema default — Codex's own configured model, which the placeholder says.
  it("shows the schema default, or Codex's own default when there is none", () => {
    expect(promptModelPlaceholder("claude", "haiku")).toBe("haiku");
    expect(promptModelPlaceholder("codex", "")).toBe("Codex's configured default");
    expect(promptModelPlaceholder("codex", "gpt-5-codex")).toBe("gpt-5-codex");
  });
});

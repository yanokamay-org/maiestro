import { describe, it, expect } from "vitest";
import { extractAppFormDefaults, layoutHostName, visibleToolFields } from "./AppSettingsForm";

describe("extractAppFormDefaults", () => {
  it("reads the terminal font default from the schema", () => {
    const d = extractAppFormDefaults({
      properties: {
        theme: { type: ["string", "null"] },
        terminal_font_family: { type: ["string", "null"], default: "Menlo, monospace" },
      },
    });
    expect(d.terminalFontDefault).toBe("Menlo, monospace");
  });

  // The form renders the default as a placeholder, so a schema without one (or a
  // non-string default) must degrade to an empty placeholder, not "undefined".
  it("degrades to an empty string when the default is missing or not a string", () => {
    expect(extractAppFormDefaults({ properties: { terminal_font_family: {} } }).terminalFontDefault).toBe("");
    expect(
      extractAppFormDefaults({ properties: { terminal_font_family: { default: 42 } } }).terminalFontDefault,
    ).toBe("");
    expect(extractAppFormDefaults({}).terminalFontDefault).toBe("");
  });

  // The Agent control selects the schema default when the setting is null; an
  // unknown or missing default degrades to Claude (the pre-#162 behavior).
  it("reads the agent default, falling back to claude", () => {
    expect(extractAppFormDefaults({ properties: { agent: { default: "codex" } } }).agentDefault).toBe("codex");
    expect(extractAppFormDefaults({ properties: { agent: { default: "gemini" } } }).agentDefault).toBe("claude");
    expect(extractAppFormDefaults({}).agentDefault).toBe("claude");
  });

  // The Default terminal host control selects the schema default when the
  // setting is null; anything unknown degrades to VS Code.
  it("reads the terminal host default, falling back to VS Code", () => {
    expect(extractAppFormDefaults({ properties: { terminal_host: { default: "terminal_app" } } }).terminalHostDefault).toBe(
      "terminal_app",
    );
    expect(extractAppFormDefaults({ properties: { terminal_host: { default: "vscode" } } }).terminalHostDefault).toBe("vscode");
    expect(extractAppFormDefaults({ properties: { terminal_host: { default: "cmux" } } }).terminalHostDefault).toBe("cmux");
    expect(extractAppFormDefaults({}).terminalHostDefault).toBe("vscode");
  });

  // The Terminal layout control selects the schema default when the setting
  // is null; anything unknown degrades to per-repo.
  it("reads the terminal layout default, falling back to per-repo", () => {
    expect(extractAppFormDefaults({ properties: { terminal_layout: { default: "tabs" } } }).terminalLayoutDefault).toBe("tabs");
    expect(extractAppFormDefaults({ properties: { terminal_layout: { default: "grid" } } }).terminalLayoutDefault).toBe(
      "per-repo",
    );
    expect(extractAppFormDefaults({}).terminalLayoutDefault).toBe("per-repo");
  });
});

describe("terminal layout", () => {
  // The layout applies to the platform's grouping host.
  it("names the platform's grouping terminal host", () => {
    expect(layoutHostName("macos")).toBe("cmux");
    expect(layoutHostName("windows")).toBe("Windows Terminal");
    expect(layoutHostName("linux")).toBeNull();
  });
});

describe("tool path rows", () => {
  const fields = ["claude", "git", "code", "cmux", "wt"].map((key) => ({ key }));
  const keys = (rows: { key: string }[]) => rows.map((r) => r.key);

  // Rows follow what the backend reports for this OS: no cmux on Windows,
  // no wt on macOS.
  it("shows only the tools this OS resolves", () => {
    const windows = ["claude", "git", "code", "wt"].map((tool) => ({ tool }));
    expect(keys(visibleToolFields(fields, windows, {}))).toEqual(["claude", "git", "code", "wt"]);
    const macos = ["claude", "git", "code", "cmux"].map((tool) => ({ tool }));
    expect(keys(visibleToolFields(fields, macos, {}))).toEqual(["claude", "git", "code", "cmux"]);
  });

  it("keeps a row whose path the file sets", () => {
    const windows = ["claude", "git", "code", "wt"].map((tool) => ({ tool }));
    expect(keys(visibleToolFields(fields, windows, { cmux: "/opt/cmux", git: null }))).toEqual([
      "claude",
      "git",
      "code",
      "cmux",
      "wt",
    ]);
    expect(keys(visibleToolFields(fields, windows, { cmux: "" }))).not.toContain("cmux");
  });
});

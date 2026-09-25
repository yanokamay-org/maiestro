import { describe, it, expect } from "vitest";
import { extractAppFormDefaults } from "./AppSettingsForm";

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
});

import { describe, it, expect } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { PROMPT_MODEL_SUGGESTIONS, useAgentModels } from "./RepoSettingsForm";

describe("useAgentModels", () => {
  it("swaps the built-in hints for the CLI's own list", async () => {
    mockIPC((cmd, args) =>
      cmd === "agent_models" && (args as { agent: string }).agent === "antigravity"
        ? ["gemini-9-flash-low", "gemini-9-pro-high"]
        : null,
    );
    const { result } = renderHook(() => useAgentModels("antigravity"));
    expect(result.current).toEqual(PROMPT_MODEL_SUGGESTIONS.antigravity);
    await waitFor(() => expect(result.current).toEqual(["gemini-9-flash-low", "gemini-9-pro-high"]));
  });

  it("keeps the built-in hints when the CLI can't list models", async () => {
    let asked = false;
    mockIPC((cmd) => {
      if (cmd === "agent_models") asked = true;
      return null;
    });
    const { result } = renderHook(() => useAgentModels("claude"));
    await waitFor(() => expect(asked).toBe(true));
    expect(result.current).toEqual(PROMPT_MODEL_SUGGESTIONS.claude);
  });

  it("doesn't show one agent's list after switching to another", async () => {
    mockIPC((cmd, args) =>
      cmd === "agent_models" && (args as { agent: string }).agent === "codex" ? ["gpt-9"] : null,
    );
    const { result, rerender } = renderHook(({ a }) => useAgentModels(a), {
      initialProps: { a: "codex" as "codex" | "claude" },
    });
    await waitFor(() => expect(result.current).toEqual(["gpt-9"]));
    rerender({ a: "claude" });
    expect(result.current).toEqual(PROMPT_MODEL_SUGGESTIONS.claude);
  });
});

import { describe, it, expect } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { Onboarding, initialAgent } from "./Onboarding";
import type { Agent } from "./api";

describe("initialAgent (#189)", () => {
  const only = (...a: Agent[]) => (x: Agent) => a.includes(x);

  it("keeps a saved agent, installed or not", () => {
    expect(initialAgent("codex", "claude", only())).toBe("codex");
  });

  it("prefers the schema default when it is installed", () => {
    expect(initialAgent(null, "claude", only("claude", "codex"))).toBe("claude");
  });

  it("falls back to the first installed agent, then the schema default", () => {
    expect(initialAgent(null, "claude", only("antigravity"))).toBe("antigravity");
    expect(initialAgent(undefined, "claude", only())).toBe("claude");
  });
});

/** Fake the backend: `identities` controls whether the GitHub step shows. */
function mockBackend(identities: string[], calls: { cmd: string; args: unknown }[] = []) {
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    switch (cmd) {
      case "identities_list": return identities.map((id) => ({ id }));
      case "tools_resolved": return [
        { tool: "claude", path: "claude", exists: false },
        { tool: "codex", path: "/opt/homebrew/bin/codex", exists: true },
        { tool: "agy", path: "agy", exists: false },
      ];
      case "app_settings_get": return {};
      case "app_settings_schema": return { properties: { agent: { default: "claude" } } };
      default: return undefined;
    }
  });
  return calls;
}

describe("Onboarding wizard (#189)", () => {
  it("walks GitHub → defaults in one window, with agent and launch at login together", async () => {
    const calls = mockBackend([]);
    render(<Onboarding />);

    expect(await screen.findByRole("heading", { name: "Connect GitHub" })).toBeInTheDocument();
    expect(screen.getByText("1. Connect GitHub").closest("li")).toHaveAttribute("aria-current", "step");
    fireEvent.click(screen.getByRole("button", { name: "Skip for now" }));

    expect(await screen.findByRole("heading", { name: "Choose defaults" })).toBeInTheDocument();
    // Only Codex is installed, so it is preselected over the schema default.
    expect(screen.getByRole("radio", { name: /Codex CLI/ })).toBeChecked();
    expect(screen.getByRole("switch", { name: "Launch at login" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("radio", { name: /Claude Code/ }));
    expect(screen.getByText(/before you start/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("switch", { name: "Launch at login" }));
    fireEvent.click(screen.getByRole("button", { name: "Get started" }));

    expect(calls.find((c) => c.cmd === "onboarding_complete")?.args).toEqual({
      launchAtLogin: true,
      agent: "claude",
    });
  });

  it("goes straight to defaults, with no step rail or Back, when an identity exists", async () => {
    mockBackend(["work"]);
    render(<Onboarding />);

    expect(await screen.findByRole("heading", { name: "Choose defaults" })).toBeInTheDocument();
    expect(screen.queryByRole("list", { name: "Setup steps" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Back" })).not.toBeInTheDocument();
  });
});

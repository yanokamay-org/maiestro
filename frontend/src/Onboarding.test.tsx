import { describe, it, expect } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { Onboarding, initialAgent, initialTerminalHost } from "./Onboarding";
import type { Agent, Platform, TerminalHost } from "./api";

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

describe("initialTerminalHost", () => {
  const mac: TerminalHost[] = ["vscode", "terminal_app", "cmux"];

  it("keeps a saved host this OS has", () => {
    expect(initialTerminalHost("cmux", "vscode", mac)).toBe("cmux");
  });

  it("falls back to the schema default for a host from another OS", () => {
    expect(initialTerminalHost("windows_terminal", "terminal_app", mac)).toBe("terminal_app");
    expect(initialTerminalHost(null, "vscode", mac)).toBe("vscode");
  });

  it("falls back to VS Code when the schema default isn't offered here", () => {
    expect(initialTerminalHost(null, "cmux", ["vscode", "windows_terminal"])).toBe("vscode");
    expect(initialTerminalHost(undefined, undefined, mac)).toBe("vscode");
  });
});

/** Fake the backend: `identities` controls whether the GitHub step shows. */
function mockBackend(
  identities: string[],
  calls: { cmd: string; args: unknown }[] = [],
  {
    platform = "macos",
    tray = () => null,
    settings = {},
  }: { platform?: Platform; tray?: () => boolean | null; settings?: Record<string, unknown> } = {},
) {
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    switch (cmd) {
      case "identities_list": return identities.map((id) => ({ id }));
      case "tools_resolved": return [
        { tool: "claude", path: "claude", exists: false },
        { tool: "codex", path: "/opt/homebrew/bin/codex", exists: true },
        { tool: "agy", path: "agy", exists: false },
      ];
      case "app_settings_get": return settings;
      case "app_settings_schema":
        return { properties: { agent: { default: "claude" }, terminal_host: { default: "vscode" } } };
      case "platform": return platform;
      case "tray_icon_promoted": return tray();
      case "terminal_hosts_installed":
        return platform === "windows"
          ? [
              { terminal_host: "vscode", installed: true, path: "C:\\code.cmd" },
              { terminal_host: "windows_terminal", installed: false, path: null },
            ]
          : [
              { terminal_host: "vscode", installed: true, path: "/usr/local/bin/code" },
              { terminal_host: "terminal_app", installed: true, path: "/System/Applications/Utilities/Terminal.app" },
              { terminal_host: "cmux", installed: false, path: null },
            ];
      default: return undefined;
    }
  });
  return calls;
}

describe("Onboarding wizard (#189)", () => {
  it("walks GitHub → terminal host → defaults in one window", async () => {
    const calls = mockBackend([]);
    render(<Onboarding />);

    expect(await screen.findByRole("heading", { name: "Connect GitHub" })).toBeInTheDocument();
    expect(screen.getByText("1. Connect GitHub").closest("li")).toHaveAttribute("aria-current", "step");
    fireEvent.click(screen.getByRole("button", { name: "Skip for now" }));

    expect(await screen.findByRole("heading", { name: "Choose a terminal host" })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /Visual Studio Code/ })).toBeChecked();
    expect(screen.getAllByRole("radio").map((r) => (r as HTMLInputElement).value)).toEqual([
      "vscode",
      "terminal_app",
      "cmux",
    ]);
    expect(screen.getByRole("radio", { name: /Visual Studio Code/ }).closest("label")).toHaveTextContent("Installed");
    fireEvent.click(screen.getByRole("radio", { name: /Terminal\.app/ }));
    expect(screen.getByText(/control Terminal/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("radio", { name: /cmux/ }));
    expect(screen.getByRole("radio", { name: /cmux/ }).closest("label")).toHaveTextContent("Not installed");
    expect(screen.getByText(/Install cmux before you start a session/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

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
      terminalHost: "cmux",
    });
  });

  it("skips GitHub when an identity exists, and sends VS Code when left alone", async () => {
    const calls = mockBackend(["work"]);
    render(<Onboarding />);

    expect(await screen.findByRole("heading", { name: "Choose a terminal host" })).toBeInTheDocument();
    expect(screen.queryByText(/Connect GitHub/)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Back" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(await screen.findByRole("heading", { name: "Choose defaults" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Get started" }));

    expect(calls.find((c) => c.cmd === "onboarding_complete")?.args).toMatchObject({ terminalHost: "vscode" });
  });

  it("lists only Windows hosts on Windows, ignoring a host synced from a Mac", async () => {
    mockBackend(["work"], [], { platform: "windows", tray: () => true, settings: { terminal_host: "cmux" } });
    render(<Onboarding />);

    expect(await screen.findByRole("heading", { name: "Choose a terminal host" })).toBeInTheDocument();
    expect(screen.getAllByRole("radio").map((r) => (r as HTMLInputElement).value)).toEqual([
      "vscode",
      "windows_terminal",
    ]);
    expect(screen.getByRole("radio", { name: /Visual Studio Code/ })).toBeChecked();
  });
});

describe("Onboarding on Windows (#198)", () => {
  it("says menu bar on macOS, with no tray step", async () => {
    mockBackend(["work"]);
    render(<Onboarding />);

    fireEvent.click(await screen.findByRole("button", { name: "Continue" }));
    expect(await screen.findByText(/in your menu bar after/)).toBeInTheDocument();
    expect(screen.queryByText(/taskbar/)).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Find the brain icon" })).not.toBeInTheDocument();
  });

  it("opens with the tray step while the icon is hidden, and notices it being dragged out", async () => {
    let promoted = false;
    mockBackend(["work"], [], { platform: "windows", tray: () => promoted });
    render(<Onboarding />);

    expect(await screen.findByRole("heading", { name: "Find the brain icon" })).toBeInTheDocument();
    expect(screen.getByText("1. Find the brain icon").closest("li")).toHaveAttribute("aria-current", "step");
    expect(screen.getByText(/Waiting for you to drag it out/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Skip for now" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Back" })).not.toBeInTheDocument();

    promoted = true;
    expect(await screen.findByText("The brain icon is on your taskbar.", {}, { timeout: 2500 })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(await screen.findByRole("heading", { name: "Choose a terminal host" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(await screen.findByRole("heading", { name: "Choose defaults" })).toBeInTheDocument();
    expect(screen.getByText(/in your taskbar after/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(await screen.findByRole("heading", { name: "Choose a terminal host" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(await screen.findByRole("heading", { name: "Find the brain icon" })).toBeInTheDocument();
  });

  it("goes tray → GitHub → defaults for a new user", async () => {
    mockBackend([], [], { platform: "windows", tray: () => false });
    render(<Onboarding />);

    expect(await screen.findByRole("heading", { name: "Find the brain icon" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Skip for now" }));
    expect(await screen.findByRole("heading", { name: "Connect GitHub" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(await screen.findByRole("heading", { name: "Find the brain icon" })).toBeInTheDocument();
  });

  it("skips the tray step when the icon is already on the taskbar", async () => {
    mockBackend(["work"], [], { platform: "windows", tray: () => true });
    render(<Onboarding />);

    expect(await screen.findByRole("heading", { name: "Choose a terminal host" })).toBeInTheDocument();
    expect(screen.queryByText("Find the brain icon")).not.toBeInTheDocument();
  });
});

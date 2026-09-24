import { describe, it, expect } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { SettingsProblemBanner } from "./SettingsProblemBanner";

const path = "/home/someone/.maiestro/settings.json";
const problem = { path, message: `${path} is not valid JSON: trailing comma at line 12 column 3` };

describe("SettingsProblemBanner", () => {
  it("shows the reason with the path shortened, and reveals the file", async () => {
    const calls: { cmd: string; args?: Record<string, unknown> }[] = [];
    mockIPC((cmd, args) => { calls.push({ cmd, args: args as Record<string, unknown> }); });

    render(<SettingsProblemBanner problem={problem} />);
    const banner = screen.getByRole("alert");
    expect(banner).toHaveTextContent("Couldn't read settings.json.");
    expect(banner).toHaveTextContent("settings.json is not valid JSON: trailing comma at line 12 column 3");
    expect(banner).not.toHaveTextContent("/home/someone");
    expect(banner).toHaveAttribute("title", problem.message);

    screen.getByRole("button", { name: "Reveal" }).click();
    await waitFor(() => expect(calls).toEqual([{ cmd: "reveal_path", args: { path } }]));
  });
});

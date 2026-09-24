import { describe, it, expect, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { UpdateBanner } from "./UpdateBanner";

const update = {
  version: "0.4.0",
  release_url: "https://github.com/yanokamay-org/maiestro/releases/tag/v0.4.0",
  published_at: "2026-09-10T18:30:00Z",
};

describe("UpdateBanner (#182)", () => {
  it("names the version, opens the release page, and dismisses by version", async () => {
    const calls: { cmd: string; args?: Record<string, unknown> }[] = [];
    mockIPC((cmd, args) => { calls.push({ cmd, args: args as Record<string, unknown> }); });
    const onDismiss = vi.fn();

    render(<UpdateBanner update={update} onDismiss={onDismiss} />);
    expect(screen.getByRole("status")).toHaveTextContent(
      "A new version of mAIestro Code (v0.4.0) is available",
    );

    const open = { cmd: "open_url", args: { url: update.release_url } };
    screen.getByRole("button", { name: "View Release" }).click();
    await waitFor(() => expect(calls).toEqual([open]));
    // The version itself is a link to the same page.
    screen.getByRole("button", { name: "v0.4.0" }).click();
    await waitFor(() => expect(calls).toEqual([open, open]));

    screen.getByRole("button", { name: "Dismiss update 0.4.0" }).click();
    expect(onDismiss).toHaveBeenCalledWith("0.4.0");
  });
});

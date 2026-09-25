import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { CodexHooksDialog } from "./CodexHooksDialog";

describe("CodexHooksDialog (#162)", () => {
  it("explains the trust all step and continues or cancels", () => {
    const onContinue = vi.fn();
    const onClose = vi.fn();
    render(<CodexHooksDialog onContinue={onContinue} onClose={onClose} />);

    expect(screen.getByRole("dialog")).toHaveTextContent("trust all");

    screen.getByRole("button", { name: "Open in VS Code" }).click();
    expect(onContinue).toHaveBeenCalledTimes(1);
    screen.getByRole("button", { name: "Cancel" }).click();
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

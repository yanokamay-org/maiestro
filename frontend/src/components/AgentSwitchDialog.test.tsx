import { describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { AgentSwitchDialog } from "./AgentSwitchDialog";

describe("AgentSwitchDialog (#186)", () => {
  it("lists every agent, explains the switch, and keeps the current one unselectable", () => {
    const onConfirm = vi.fn();
    render(<AgentSwitchDialog title="t" current="claude" onConfirm={onConfirm} onClose={() => {}} />);
    expect(screen.getByRole("dialog", { name: "AI Agent · t" })).toBeInTheDocument();
    expect(screen.getByText(/conversation doesn.t carry over/)).toBeInTheDocument();

    const claude = screen.getByRole("button", { name: /Claude Code/ });
    expect(claude).toBeDisabled();
    expect(claude).toHaveTextContent("Current");
    expect(claude).toHaveAttribute("aria-current", "true");

    fireEvent.click(screen.getByRole("button", { name: /Codex CLI/ }));
    expect(onConfirm).toHaveBeenCalledWith("codex");
  });
});

import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { AgentPill } from "./AgentPill";
import type { StatusRecord } from "../api";

const status = (over: Partial<StatusRecord> = {}): StatusRecord => ({
  workspace: "162-x",
  state: "busy",
  ts: "2026-09-24T00:00:00Z",
  ...over,
});

// The first path command of each mark, enough to tell the two logos apart.
const markPath = () => screen.getByRole("button").querySelector("svg path")?.getAttribute("d") ?? "";

describe("AgentPill (#162)", () => {
  it("shows the Claude logo and names Claude for a Claude session", () => {
    render(<AgentPill agent="claude" status={status()} onClick={() => {}} />);
    expect(screen.getByRole("button")).toHaveAttribute("title", "Claude · Working");
    expect(markPath()).toMatch(/^m4\.7144/);
  });

  it("shows the OpenAI logo and names Codex for a Codex session", () => {
    render(
      <AgentPill
        agent="codex"
        status={status({ state: "needs_you", detail: "Permission requested: `Bash`" })}
        onClick={() => {}}
      />,
    );
    expect(screen.getByRole("button")).toHaveAttribute(
      "title",
      "Codex · Needs you — Permission requested: `Bash`",
    );
    expect(markPath()).toMatch(/^M22\.2819/);
  });

  it("shows Idle before a status exists, once the session ended, and while creating", () => {
    const { rerender } = render(<AgentPill agent="codex" onClick={() => {}} />);
    expect(screen.getByRole("button")).toHaveAttribute("title", "Codex · Idle");
    expect(screen.getByRole("button")).toHaveClass("agent-pill--idle");
    for (const state of ["ended", "creating"] as const) {
      rerender(<AgentPill agent="codex" status={status({ state, last_error: { message: "x", surfaced: true } as never })} onClick={() => {}} />);
      expect(screen.getByRole("button")).toHaveAttribute("title", "Codex · Idle");
      expect(screen.getByRole("button")).not.toHaveClass("agent-pill--error");
    }
  });
});

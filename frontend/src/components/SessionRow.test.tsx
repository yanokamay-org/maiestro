import { describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { SessionRow, SessionRowProps } from "./SessionRow";
import type { Session } from "../api";

const session: Session = {
  id: "186-x", repo: "a/b", issue_number: 186, issue_url: "u", branch: "feature/186-x",
  default_branch: "main", work_dir: "/w", cloned_repo_dir: "/c", session_title: "t",
  color: "#c46686", emoji: "e", agent: "claude", hidden: null,
};

function props(over: Partial<SessionRowProps> = {}): SessionRowProps {
  const noop = () => {};
  return {
    session, status: undefined, pr: null, checks: null, prCreate: undefined, prMerge: undefined,
    teardownBusy: false, teardownConfirm: null, agentPrompt: null, agentBusy: false,
    cmdOpen: true, repoHidden: false, now: 0, openErr: undefined,
    busyCls: () => "", busyRingCls: () => "",
    onToggleCommands: noop, onOpenInEditor: noop, onOpenPath: noop, onOpenUrl: noop,
    onOpenAccessibilitySettings: noop, onCreatePr: noop, onStartMerge: noop, onTearDown: noop,
    onChooseAgent: noop, onFocusEditor: noop, onRestartEditor: noop, onDismissAgentPrompt: noop, onRunTeardown: noop,
    onHide: noop, onUnhide: noop, onCancelTeardownConfirm: noop, onDismissPrCreateError: noop,
    onDismissPrMergeError: noop, onDismissToolError: noop, onDismissOpenError: noop, onDismissNotice: noop,
    ...over,
  };
}

describe("SessionRow agent switch (#186)", () => {
  it("opens the agent picker from the command strip", () => {
    const onChooseAgent = vi.fn();
    render(<SessionRow {...props({ onChooseAgent })} />);
    fireEvent.click(screen.getByRole("button", { name: "Agentic Coding CLI…" }));
    expect(onChooseAgent).toHaveBeenCalled();
  });

  it("is disabled while the workspace is being torn down", () => {
    render(<SessionRow {...props({ teardownBusy: true })} />);
    expect(screen.getByRole("button", { name: "Agentic Coding CLI…" })).toBeDisabled();
  });

  it("asks to restart a window still running the old agent", () => {
    const onRestartEditor = vi.fn();
    render(
      <SessionRow
        {...props({
          session: { ...session, agent: "codex" },
          agentPrompt: { id: "186-x", kind: "restart", reason: "open", agent: "codex", editorAgent: "claude" },
          onRestartEditor,
        })}
      />,
    );
    expect(screen.getByText(/switched to Codex, but its VS Code window is still running Claude/)).toBeInTheDocument();
    expect(screen.getByText(/Claude conversation won't carry over/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Restart VS Code" }));
    expect(onRestartEditor).toHaveBeenCalled();
  });

  it("ignores another row's prompt", () => {
    render(
      <SessionRow
        {...props({ agentPrompt: { id: "other", kind: "restart", reason: "switched", agent: "codex", editorAgent: "claude" } })}
      />,
    );
    expect(screen.queryByRole("button", { name: "Restart VS Code" })).toBeNull();
  });

  it("offers to open VS Code when the restart couldn't close the window", () => {
    const onFocusEditor = vi.fn();
    render(
      <SessionRow
        {...props({
          agentPrompt: { id: "186-x", kind: "blocked", message: "The Visual Studio Code window didn't close.", accessibility: false },
          onFocusEditor,
        })}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Open VS Code" }));
    expect(onFocusEditor).toHaveBeenCalled();
  });
});

describe("SessionRow notice (#185)", () => {
  it("shows a session's notice until dismissed, and nothing without one", () => {
    const onDismissNotice = vi.fn();
    const notice = "Added `.agents/hooks.json` to this worktree's `.gitignore`.";
    const { rerender } = render(<SessionRow {...props({ session: { ...session, notice }, onDismissNotice })} />);
    expect(screen.getByText("mAIestro Code changed this worktree")).toBeInTheDocument();
    expect(screen.getByText(notice)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(onDismissNotice).toHaveBeenCalled();
    rerender(<SessionRow {...props()} />);
    expect(screen.queryByText("mAIestro Code changed this worktree")).toBeNull();
  });
});

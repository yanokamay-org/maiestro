import { describe, it, expect } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { PathProbeGenerationProvider, usePathExists } from "./PathField";

// A minimal consumer that renders `usePathExists(path)`'s current answer, so
// the tests can assert on it via the DOM rather than `renderHook` (whose
// `wrapper` option can't take per-render props, and the generation must vary
// independently of `path` here).
function Probe({ path }: { path: string }) {
  const exists = usePathExists(path);
  return <div data-testid="exists">{String(exists)}</div>;
}

function Harness({ path, generation }: { path: string; generation: number }) {
  return (
    <PathProbeGenerationProvider value={generation}>
      <Probe path={path} />
    </PathProbeGenerationProvider>
  );
}

describe("usePathExists + PathProbeGenerationContext (#147)", () => {
  // The health-check modal closing bumps the ambient generation so a fix made
  // while it was open (e.g. cloning the repo) is reflected without the field's
  // own value changing.
  it("re-probes when the generation bumps, even though the path is unchanged", async () => {
    let calls = 0;
    let answer = false;
    mockIPC((cmd) => {
      if (cmd === "path_exists") {
        calls++;
        return answer;
      }
      return undefined;
    });

    const { rerender } = render(<Harness path="~/src/widget" generation={0} />);
    await waitFor(() => expect(screen.getByTestId("exists")).toHaveTextContent("false"));
    expect(calls).toBe(1);

    answer = true;
    rerender(<Harness path="~/src/widget" generation={1} />);

    await waitFor(() => expect(screen.getByTestId("exists")).toHaveTextContent("true"));
    expect(calls).toBe(2);
  });

  // Guards against an accidental re-probe on every render (e.g. from a
  // reference-unstable context value), which would defeat the field's
  // debounce and hammer the backend.
  it("does not re-probe when the generation is unchanged", async () => {
    let calls = 0;
    mockIPC((cmd) => {
      if (cmd === "path_exists") {
        calls++;
        return true;
      }
      return undefined;
    });

    const { rerender } = render(<Harness path="~/src/widget" generation={0} />);
    await waitFor(() => expect(screen.getByTestId("exists")).toHaveTextContent("true"));
    expect(calls).toBe(1);

    rerender(<Harness path="~/src/widget" generation={0} />);
    // Give the 300ms debounce a chance to fire if it were (wrongly) retriggered.
    await new Promise((r) => setTimeout(r, 400));
    expect(calls).toBe(1);
  });

  // Absent a provider, the hook must keep working exactly as before (e.g. in
  // tests or windows other than Settings) — the context's default is `0`.
  it("works without a provider (default generation)", async () => {
    mockIPC((cmd) => (cmd === "path_exists" ? true : undefined));
    render(<Probe path="~/src/widget" />);
    await waitFor(() => expect(screen.getByTestId("exists")).toHaveTextContent("true"));
  });
});

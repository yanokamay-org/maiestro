// Shared path-field affordances for the Settings forms (issue #88): a soft
// existence check (an inline "not found" hint, never a hard/save-blocking error)
// and a "reveal in Finder" button, for every field that references a filesystem
// path — repo cloned repo dir / worktree prefix / env files, and app tool paths.
//
// Validation is deliberately advisory: it polls the backend `path_exists`
// (tilde-aware) and shows a hint, but does not participate in ajv/schema
// validation, so it can never block the debounced autosave.

import { createContext, useContext, useEffect, useState } from "react";
import FolderIcon from "./icons/folder.svg?react";
import { api } from "./api";

/** Bumped by `Settings.tsx` whenever something outside a path field's own value
 *  may have fixed the filesystem underneath it (#147) — currently, closing the
 *  health-check modal. `usePathExists` re-probes on every bump regardless of
 *  whether the path text itself changed. Defaults to `0` so the hook works
 *  unchanged wherever the provider is absent (tests, other windows). */
export const PathProbeGenerationContext = createContext(0);

export const PathProbeGenerationProvider = PathProbeGenerationContext.Provider;

/** Debounced existence check for `path`. Returns `null` while unknown/checking,
 *  `true`/`false` once resolved. Empty/whitespace paths resolve to `null` (an
 *  empty field is "use default", never a validation error). Re-runs whenever
 *  the ambient probe generation (see `PathProbeGenerationContext`) bumps, even
 *  if `path` itself hasn't changed. */
export function usePathExists(path: string | null | undefined): boolean | null {
  const value = (path ?? "").trim();
  const generation = useContext(PathProbeGenerationContext);
  const [exists, setExists] = useState<boolean | null>(null);

  useEffect(() => {
    if (value === "") {
      setExists(null);
      return;
    }
    let cancelled = false;
    setExists(null);
    const t = setTimeout(() => {
      api
        .pathExists(value)
        .then((ok) => {
          if (!cancelled) setExists(ok);
        })
        .catch(() => {
          if (!cancelled) setExists(null);
        });
    }, 300);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [value, generation]);

  return exists;
}

/** Folder-icon button that reveals `path` in Finder. Disabled (greyed) when the
 *  path is empty or known-missing — there's nothing to reveal. */
export function RevealButton({
  path,
  exists,
  title = "Reveal in Finder",
}: {
  path: string | null | undefined;
  exists: boolean | null;
  title?: string;
}) {
  const value = (path ?? "").trim();
  const disabled = value === "" || exists === false;
  return (
    <button
      type="button"
      className="path-reveal-btn"
      disabled={disabled}
      title={disabled ? "Path not found" : title}
      aria-label={title}
      onClick={() => {
        if (!disabled) void api.revealPath(value).catch(() => {});
      }}
    >
      <FolderIcon width={14} height={14} />
    </button>
  );
}

/** Inline "not found" hint, shown only for a non-empty path that's missing.
 *  Reuses the `.jsf-tool-missing` style already used by the tool-paths status. */
export function PathMissingHint({
  path,
  exists,
  label = "Path not found",
}: {
  path: string | null | undefined;
  exists: boolean | null;
  label?: string;
}) {
  const value = (path ?? "").trim();
  if (value === "" || exists !== false) return null;
  return <div className="jsf-help jsf-tool-missing">{label}</div>;
}

/** A text input paired with a reveal button and a missing-path hint, for the
 *  single-path fields (cloned repo dir, worktree prefix). `checkPath` lets a field
 *  validate something other than its own value — the worktree prefix checks its
 *  parent directory, since the prefix itself is never a real path. */
export function PathField({
  value,
  onChange,
  placeholder,
  checkPath,
  missingLabel,
  className,
}: {
  value: string;
  onChange: (next: string) => void;
  placeholder?: string;
  /** Path to validate/reveal; defaults to `value`. */
  checkPath?: string;
  missingLabel?: string;
  className?: string;
}) {
  const target = checkPath ?? value;
  const exists = usePathExists(target);
  return (
    <>
      <div className="path-field-row">
        <input
          className={`text-input ${className ?? ""}`}
          type="text"
          value={value}
          placeholder={placeholder}
          onChange={(e) => onChange(e.target.value)}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
        />
        <RevealButton path={target} exists={exists} />
      </div>
      <PathMissingHint path={target} exists={exists} label={missingLabel} />
    </>
  );
}

import { api, CredentialTypeDto } from "../api";

// Anchor into the README's "GitHub token" section (its permissions table and
// fine-grained-vs-classic guidance), shared by the onboarding wizard and this
// Settings row so both point at the one place that documents it.
export const GITHUB_PAT_SETUP_URL = "https://github.com/emisch0/maiestro#github-token";

export type SaveStatus = "idle" | "saving" | "saved" | "clearing" | "error";

export interface CredState {
  isSet: boolean;
  input: string;
  status: SaveStatus;
  error?: string;
}

// Module-level (not defined inside Settings' render): a nested definition gets a
// new function identity every render, which React treats as a different element
// type — remounting the subtree and dropping the token input's focus per keystroke.
export function CredRows({ credTypes, credStates, patchCred, onSave, onClear }: {
  credTypes: CredentialTypeDto[];
  credStates: Record<string, CredState>;
  patchCred: (typeId: string, patch: Partial<CredState>) => void;
  onSave: (typeId: string) => void;
  onClear: (typeId: string) => void;
}) {
  return (
    <>
      {credTypes.map((t) => {
        const state = credStates[t.type_id] ?? { isSet: false, input: "", status: "idle" as SaveStatus };
        const isBusy = state.status === "saving" || state.status === "clearing";

        return (
          <div key={t.type_id} className="cred-row">
            <div className="cred-header">
              <span className="cred-name">{t.display_name}</span>
              <span className={`cred-badge ${state.isSet ? "is-set" : "not-set"}`}>
                {state.isSet ? "set" : "not set"}
              </span>
            </div>
            {t.type_id === "github_token" && (
              <button
                type="button"
                className="help-link"
                onClick={() => { void api.openUrl(GITHUB_PAT_SETUP_URL).catch(() => {}); }}
              >
                How to create a token →
              </button>
            )}
            <div className="cred-controls">
              <input
                className="text-input secret-input"
                type="password"
                aria-label={`${t.display_name} value`}
                placeholder={state.isSet ? "Update value…" : "Enter value…"}
                value={state.input}
                disabled={isBusy}
                onChange={(e) => patchCred(t.type_id, { input: e.target.value, status: "idle" })}
                onKeyDown={(e) => e.key === "Enter" && onSave(t.type_id)}
              />
              <button
                className={`btn-save ${state.status === "saving" ? "btn-busy" : ""}`}
                disabled={!state.input.trim() || isBusy}
                onClick={() => onSave(t.type_id)}
              >
                {state.status === "saving" ? "…" : state.status === "saved" ? "✓" : "Save"}
              </button>
              <button
                className={`btn-clear ${!state.isSet ? "hidden" : ""}`}
                disabled={!state.isSet || isBusy}
                onClick={() => onClear(t.type_id)}
                title="Remove credential"
              >
                ✕
              </button>
            </div>
            {state.status === "error" && <p className="cred-error">{state.error}</p>}
          </div>
        );
      })}
    </>
  );
}

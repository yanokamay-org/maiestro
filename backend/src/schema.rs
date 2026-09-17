//! Shared JSON-Schema helpers for the hand-written settings schemas.
//!
//! Both `repo_settings.rs` and `app_settings.rs` embed a hand-written JSON Schema
//! and validate on-disk files against it. The parse + validate logic was
//! byte-identical in both; it lives here now so there is one implementation.

/// Parse an embedded schema string into a JSON value. Infallible in practice —
/// each caller's `schema_parses` test guarantees the embedded string is valid
/// JSON, so a panic here is a build bug. `label` names the schema in the panic
/// message (e.g. "repo-settings").
pub fn parse(schema_json: &str, label: &str) -> serde_json::Value {
    serde_json::from_str(schema_json)
        .unwrap_or_else(|e| panic!("embedded {label} schema is not valid JSON: {e}"))
}

/// Validate a JSON value against a parsed schema, returning a message naming the
/// failing field(s) on error.
pub fn validate(schema: &serde_json::Value, value: &serde_json::Value) -> Result<(), String> {
    let validator =
        jsonschema::validator_for(schema).map_err(|e| format!("internal schema error: {e}"))?;
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| {
            let at = e.instance_path().to_string();
            let at = if at.is_empty() { "/".to_string() } else { at };
            format!("at `{at}`: {e}")
        })
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// A string `default` from a parsed schema, addressed by JSON Pointer (e.g.
/// `/properties/worktree_prefix/default`). The schemas are the single source of
/// truth for these defaults, so backend resolution and the Settings form both
/// read them from there rather than hardcoding a copy. Returns `""` if absent.
pub fn default_str(schema: &serde_json::Value, pointer: &str) -> String {
    schema
        .pointer(pointer)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// A boolean `default` from a parsed schema, addressed by JSON Pointer (e.g.
/// `/properties/comment_on_spawn/default`). The boolean counterpart to
/// [`default_str`], for the same reason: a `null`/absent field in a settings
/// file means "use the default", and that default is declared in the schema
/// only — never as a literal in Rust. Returns `false` if absent.
pub fn default_bool(schema: &serde_json::Value, pointer: &str) -> bool {
    schema
        .pointer(pointer)
        .and_then(|v| v.as_bool())
        .unwrap_or_default()
}

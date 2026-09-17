use serde::Serialize;

/// Metadata a plugin declares for a credential type it owns.
#[derive(Debug, Clone)]
pub struct CredentialTypeInfo {
    /// Stable identifier used as the `type_id` in the credential store (e.g. `"github_token"`).
    pub type_id: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
}

/// A plugin contributes one or more credential types it owns. (mAIestro Code does not
/// inject these into launched sessions — see the architecture notes in CLAUDE.md
/// on ambient environment — so a plugin is currently just a credential-type
/// declaration consumed by `plugins_list_credential_types`.)
pub trait Plugin: Send + Sync {
    fn credential_types(&self) -> &[CredentialTypeInfo];
}

/// Registry of all active plugins. Constructed at app startup and stored as
/// immutable Tauri managed state — no mutex needed after init.
pub struct PluginRegistry {
    plugins: Vec<Box<dyn Plugin>>,
}

impl PluginRegistry {
    pub fn builder() -> PluginRegistryBuilder {
        PluginRegistryBuilder { plugins: Vec::new() }
    }

    pub fn all_credential_types(&self) -> impl Iterator<Item = &CredentialTypeInfo> {
        self.plugins.iter().flat_map(|p| p.credential_types())
    }
}

pub struct PluginRegistryBuilder {
    plugins: Vec<Box<dyn Plugin>>,
}

impl PluginRegistryBuilder {
    pub fn register(mut self, plugin: impl Plugin + 'static) -> Self {
        self.plugins.push(Box::new(plugin));
        self
    }

    pub fn build(self) -> PluginRegistry {
        PluginRegistry { plugins: self.plugins }
    }
}

// ── Tauri command ─────────────────────────────────────────────────────────────

/// Serializable DTO for the JS bridge.
#[derive(Serialize)]
pub struct CredentialTypeDto {
    pub type_id: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
}

#[tauri::command]
pub fn plugins_list_credential_types(
    registry: tauri::State<'_, PluginRegistry>,
) -> Vec<CredentialTypeDto> {
    crate::log_invoke_debug!("plugins_list_credential_types");
    registry
        .all_credential_types()
        .map(|t| CredentialTypeDto {
            type_id: t.type_id,
            display_name: t.display_name,
            description: t.description,
        })
        .collect()
}

use crate::settings::{auth_command_for_provider, auth_flag_for_provider, Settings};
use serde::Serialize;
use std::fs;

/// A provider with runtime connection status.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRuntime {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub file_hints: Vec<String>,
    pub connected: bool,
    pub auth_available: bool,
    pub auth_command: String,
}

/// Collection of providers returned to the frontend.
#[derive(Debug, Serialize)]
pub struct ProvidersResponse {
    pub providers: Vec<ProviderRuntime>,
}

/// Scan the auth directory and determine which providers are connected.
pub fn detect_providers(settings: &Settings) -> Vec<ProviderRuntime> {
    let filenames: Vec<String> = fs::read_dir(&settings.auth_dir)
        .ok()
        .into_iter()
        .flat_map(|iter| iter.filter_map(Result::ok))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .map(|name| name.to_lowercase())
        .collect();

    settings
        .providers
        .iter()
        .map(|provider| {
            let hints: Vec<String> = provider
                .file_hints
                .iter()
                .map(|h| h.to_lowercase())
                .collect();
            let connected = filenames
                .iter()
                .any(|name| hints.iter().any(|hint| name.contains(hint)));

            ProviderRuntime {
                id: provider.id.clone(),
                name: provider.name.clone(),
                enabled: provider.enabled,
                file_hints: provider.file_hints.clone(),
                connected,
                auth_available: auth_flag_for_provider(&provider.id).is_some(),
                auth_command: auth_command_for_provider(settings, &provider.id).unwrap_or_default(),
            }
        })
        .collect()
}

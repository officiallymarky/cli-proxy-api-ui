use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Maximum number of log lines retained in memory.
pub const MAX_LOG_LINES: usize = 450;

/// An AI provider that can be authenticated via cli-proxy-api.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub file_hints: Vec<String>,
    #[serde(default)]
    pub reasoning_effort: String,
}

/// Persistent application settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub binary_path: String,
    pub auth_dir: String,
    pub config_path: String,
    pub start_proxy_automatically: bool,
    pub providers: Vec<Provider>,
    pub vercel_gateway_enabled: bool,
    pub vercel_gateway_api_key: String,
    #[serde(default)]
    pub codex_instructions_enabled: bool,
}

/// Partial settings for lenient deserialization (missing fields use defaults).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialSettings {
    pub binary_path: Option<String>,
    /// Legacy field from old config format; used only for migration.
    pub args: Option<String>,
    pub auth_dir: Option<String>,
    pub config_path: Option<String>,
    pub start_proxy_automatically: Option<bool>,
    pub providers: Option<Vec<Provider>>,
    pub vercel_gateway_enabled: Option<bool>,
    pub vercel_gateway_api_key: Option<String>,
    pub codex_instructions_enabled: Option<bool>,
}

/// A single log entry from the proxy process.
#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub ts: String,
    pub source: String,
    pub line: String,
}

/// Determine the app config directory, migrating from legacy path if needed.
pub fn app_config_dir() -> Result<PathBuf, String> {
    let preferred = ProjectDirs::from("com", "cliproxyapi", "cli-proxy-api-ui")
        .map(|dirs| dirs.config_dir().to_path_buf())
        .ok_or_else(|| "Unable to determine config directory".to_string())?;

    if !preferred.exists() {
        if let Some(legacy) = ProjectDirs::from("com", "cliproxyapi", "ui")
            .map(|dirs| dirs.config_dir().to_path_buf())
        {
            if legacy.exists() {
                if let Some(parent) = preferred.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let _ = fs::rename(&legacy, &preferred);
            }
        }
    }

    Ok(preferred)
}

/// Path to the settings.json file.
pub fn settings_path() -> Result<PathBuf, String> {
    Ok(app_config_dir()?.join("settings.json"))
}

/// Default auth directory inside the config dir.
pub fn default_auth_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("auth")
}

/// Default config file path inside the config dir.
pub fn default_config_path(config_dir: &Path) -> PathBuf {
    config_dir.join("config.yaml")
}

/// Get the user's home directory from environment variables.
fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// Expand `~` prefix to the user's home directory.
pub fn expand_tilde(raw: &str) -> String {
    if let Some(home) = user_home_dir() {
        if raw == "~" {
            return home.to_string_lossy().to_string();
        }
        if let Some(rest) = raw.strip_prefix("~/") {
            return home.join(rest).to_string_lossy().to_string();
        }
        if let Some(rest) = raw.strip_prefix("~\\") {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    raw.to_string()
}

/// Parse a `-config` or `--config` flag value from a legacy args string.
pub fn parse_config_path_from_args(args: &str) -> Option<String> {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let mut i = 0usize;

    while i < tokens.len() {
        let token = tokens[i];
        if (token == "-config" || token == "--config") && i + 1 < tokens.len() {
            return Some(
                tokens[i + 1]
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string(),
            );
        }
        if let Some(value) = token.strip_prefix("-config=") {
            return Some(value.trim_matches('"').trim_matches('\'').to_string());
        }
        if let Some(value) = token.strip_prefix("--config=") {
            return Some(value.trim_matches('"').trim_matches('\'').to_string());
        }
        i += 1;
    }

    None
}

/// Normalize an auth directory path, expanding tildes and making relative paths absolute.
pub fn normalize_auth_dir_path(raw: &str, fallback: &Path) -> PathBuf {
    let value = if raw.trim().is_empty() {
        fallback.to_path_buf()
    } else {
        PathBuf::from(expand_tilde(raw.trim()))
    };

    if value.is_absolute() {
        value
    } else {
        fallback.parent().unwrap_or(fallback).join(value)
    }
}

/// Normalize a config file path, appending config.yaml if the path is a directory.
pub fn normalize_config_file_path(raw: &str, fallback: &Path) -> PathBuf {
    let mut value = if raw.trim().is_empty() {
        fallback.to_path_buf()
    } else {
        PathBuf::from(expand_tilde(raw.trim()))
    };

    if !value.is_absolute() {
        value = fallback.parent().unwrap_or(fallback).join(value);
    }

    let ends_with_sep = raw.ends_with('/') || raw.ends_with('\\');
    if ends_with_sep || value.is_dir() {
        value = value.join("config.yaml");
    }

    value
}

/// Normalize all settings fields, filling defaults and resolving paths.
pub fn normalize_settings(
    settings: &mut Settings,
    legacy_args: Option<&str>,
) -> Result<(), String> {
    let config_dir = app_config_dir()?;
    let default_auth = default_auth_dir(&config_dir);
    let default_cfg = default_config_path(&config_dir);

    if settings.binary_path.trim().is_empty() {
        settings.binary_path = "cli-proxy-api".to_string();
    } else {
        settings.binary_path = settings.binary_path.trim().to_string();
    }

    let raw_auth_dir = settings.auth_dir.clone();
    settings.auth_dir = normalize_auth_dir_path(&raw_auth_dir, &default_auth)
        .to_string_lossy()
        .to_string();

    let config_candidate = if settings.config_path.trim().is_empty() {
        legacy_args
            .and_then(parse_config_path_from_args)
            .unwrap_or_else(|| default_cfg.to_string_lossy().to_string())
    } else {
        settings.config_path.clone()
    };

    settings.config_path = normalize_config_file_path(&config_candidate, &default_cfg)
        .to_string_lossy()
        .to_string();

    if settings.providers.is_empty() {
        settings.providers = default_providers();
    } else {
        settings
            .providers
            .retain(|provider| auth_flag_for_provider(&provider.id).is_some());

        if settings.providers.is_empty() {
            settings.providers = default_providers();
        }
    }

    Ok(())
}

/// The built-in list of supported AI providers.
pub fn default_providers() -> Vec<Provider> {
    vec![
        Provider {
            id: "codex".into(),
            name: "Codex".into(),
            enabled: true,
            file_hints: vec!["codex".into(), "openai".into()],
            reasoning_effort: String::new(),
        },
        Provider {
            id: "claude".into(),
            name: "Claude".into(),
            enabled: true,
            file_hints: vec!["claude".into(), "anthropic".into()],
            reasoning_effort: String::new(),
        },
        Provider {
            id: "gemini".into(),
            name: "Gemini".into(),
            enabled: true,
            file_hints: vec!["gemini".into(), "google".into()],
            reasoning_effort: String::new(),
        },
        Provider {
            id: "qwen".into(),
            name: "Qwen".into(),
            enabled: true,
            file_hints: vec!["qwen".into()],
            reasoning_effort: String::new(),
        },
    ]
}

/// Create default settings with paths resolved from the config directory.
pub fn default_settings() -> Result<Settings, String> {
    let config_dir = app_config_dir()?;
    let auth_dir = default_auth_dir(&config_dir);
    let config_path = default_config_path(&config_dir);

    Ok(Settings {
        binary_path: "cli-proxy-api".to_string(),
        auth_dir: auth_dir.to_string_lossy().into_owned(),
        config_path: config_path.to_string_lossy().into_owned(),
        start_proxy_automatically: false,
        providers: default_providers(),
        vercel_gateway_enabled: false,
        vercel_gateway_api_key: String::new(),
        codex_instructions_enabled: false,
    })
}

/// Generate a default YAML config file content.
pub fn default_config_yaml(auth_dir: &str) -> String {
    [
        "# CLI Proxy API UI generated config".to_string(),
        "# Update this file as needed for your environment.".to_string(),
        format!("auth-dir: \"{}\"", auth_dir),
        "debug: false".to_string(),
        "usage-statistics-enabled: false".to_string(),
        "".to_string(),
        "# Remote management (not yet supported in UI)".to_string(),
        "remote-management:".to_string(),
        "  allow-remote: false".to_string(),
        "  disable-control-panel: true".to_string(),
        "".to_string(),
        "# Codex instructions injection (Codex only)".to_string(),
        "codex-instructions-enabled: false".to_string(),
        "".to_string(),
        "# Vercel AI Gateway".to_string(),
        "vercel-gateway-enabled: false".to_string(),
        "vercel-gateway-endpoint: \"https://ai-gateway.vercel.sh\"".to_string(),
        "vercel-gateway-api-key: \"\"".to_string(),
        "".to_string(),
    ]
    .join("\n")
}

/// Ensure the config file contains the correct auth-dir line.
pub fn ensure_config_has_auth_dir(config_path: &Path, auth_dir: &str) -> Result<(), String> {
    let auth_line = format!("auth-dir: \"{}\"", auth_dir);

    let raw = match fs::read_to_string(config_path) {
        Ok(s) => s,
        Err(_) => {
            fs::write(config_path, default_config_yaml(auth_dir)).map_err(|e| e.to_string())?;
            return Ok(());
        }
    };

    let mut replaced = false;
    let mut next_lines = Vec::new();

    for line in raw.lines() {
        if line.trim_start().starts_with("auth-dir:") {
            next_lines.push(auth_line.clone());
            replaced = true;
        } else {
            next_lines.push(line.to_string());
        }
    }

    if !replaced {
        next_lines.push(auth_line);
    }

    let next = format!("{}\n", next_lines.join("\n"));
    if next != raw {
        fs::write(config_path, next).map_err(|e| e.to_string())?;
    }

    Ok(())
}

/// Ensure the config file contains gateway-related lines matching settings.
pub fn ensure_config_has_gateway(
    config_path: &Path,
    enabled: bool,
    api_key: &str,
) -> Result<(), String> {
    let raw = fs::read_to_string(config_path).unwrap_or_default();

    let gateway_keys = [
        "vercel-gateway-enabled:",
        "vercel-gateway-endpoint:",
        "vercel-gateway-api-key:",
    ];

    let mut next_lines: Vec<String> = raw
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            if gateway_keys.iter().any(|k| trimmed.starts_with(k)) {
                return false;
            }
            if trimmed == "# Vercel AI Gateway" {
                return false;
            }
            true
        })
        .map(|s| s.to_string())
        .collect();

    // Remove trailing empty lines to avoid accumulating blanks
    while next_lines.last().is_some_and(|l| l.trim().is_empty()) {
        next_lines.pop();
    }

    next_lines.push(String::new());
    next_lines.push("# Vercel AI Gateway".to_string());
    next_lines.push(format!("vercel-gateway-enabled: {}", enabled));
    next_lines.push("vercel-gateway-endpoint: \"https://ai-gateway.vercel.sh\"".to_string());
    if !api_key.is_empty() {
        next_lines.push(format!("vercel-gateway-api-key: \"{}\"", api_key));
    } else {
        next_lines.push("vercel-gateway-api-key: \"\"".to_string());
    }
    next_lines.push(String::new());

    let next = format!("{}\n", next_lines.join("\n"));
    if next != raw {
        fs::write(config_path, next).map_err(|e| e.to_string())?;
    }

    Ok(())
}

/// Ensure the config file contains the codex-instructions-enabled line.
pub fn ensure_config_has_codex_instructions(
    config_path: &Path,
    enabled: bool,
) -> Result<(), String> {
    let raw = fs::read_to_string(config_path).unwrap_or_default();

    let mut next_lines: Vec<String> = raw
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("codex-instructions-enabled:")
        })
        .map(|s| s.to_string())
        .collect();

    // Remove trailing empty lines
    while next_lines.last().is_some_and(|l| l.trim().is_empty()) {
        next_lines.pop();
    }

    next_lines.push(String::new());
    next_lines.push("# Codex instructions injection (Codex only)".to_string());
    next_lines.push(format!("codex-instructions-enabled: {}", enabled));
    next_lines.push(String::new());

    let next = format!("{}\n", next_lines.join("\n"));
    if next != raw {
        fs::write(config_path, next).map_err(|e| e.to_string())?;
    }

    Ok(())
}

/// Lookup table mapping reasoning level names to token budgets (for providers needing numeric values).
fn reasoning_level_to_budget(provider_id: &str, level: &str) -> Option<u64> {
    match provider_id {
        "claude" => match level {
            "low" => Some(1024),
            "medium" => Some(8192),
            "high" => Some(24576),
            "xhigh" => Some(32768),
            "max" => Some(64000),
            _ => None,
        },
        "gemini" => match level {
            "low" => Some(1024),
            "medium" => Some(8192),
            "high" => Some(24576),
            _ => None,
        },
        _ => None,
    }
}

/// Return the protocol name used in payload override rules for a provider.
fn provider_protocol(provider_id: &str) -> &'static str {
    match provider_id {
        "codex" => "codex",
        "claude" => "claude",
        "gemini" => "gemini",
        "qwen" => "qwen",
        _ => "openai",
    }
}

/// Return the payload param key for reasoning effort for a given provider.
fn provider_reasoning_param(provider_id: &str) -> &'static str {
    match provider_id {
        "codex" | "qwen" => "reasoning.effort",
        "gemini" => "generationConfig.thinkingConfig.thinkingBudget",
        "claude" => "thinking.budget_tokens",
        _ => "reasoning.effort",
    }
}

/// Ensure the config YAML contains payload.override entries for enabled providers
/// that have a non-empty reasoning_effort configured.
pub fn ensure_config_has_reasoning_overrides(
    config_path: &Path,
    providers: &[Provider],
) -> Result<(), String> {
    let raw = fs::read_to_string(config_path).unwrap_or_default();
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&raw)
        .unwrap_or(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));

    let mut overrides = serde_yaml::Sequence::new();

    for provider in providers {
        let effort = provider.reasoning_effort.trim();
        if !provider.enabled || effort.is_empty() {
            continue;
        }

        let protocol = provider_protocol(&provider.id);
        let param_key = provider_reasoning_param(&provider.id);

        // Codex/Qwen use string-level reasoning.effort; Gemini/Claude need numeric token budgets.
        let param_value: serde_yaml::Value = if provider.id == "codex" || provider.id == "qwen" {
            serde_yaml::Value::String(effort.to_string())
        } else if let Some(budget) = reasoning_level_to_budget(&provider.id, effort) {
            serde_yaml::Value::Number(serde_yaml::Number::from(budget))
        } else {
            // Unknown level; skip.
            continue;
        };

        let mut params = serde_yaml::Mapping::new();
        params.insert(serde_yaml::Value::String(param_key.into()), param_value);

        let mut model_entry = serde_yaml::Mapping::new();
        model_entry.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("*".into()),
        );
        model_entry.insert(
            serde_yaml::Value::String("protocol".into()),
            serde_yaml::Value::String(protocol.into()),
        );

        let mut entry = serde_yaml::Mapping::new();
        entry.insert(
            serde_yaml::Value::String("models".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(model_entry)]),
        );
        entry.insert(
            serde_yaml::Value::String("params".into()),
            serde_yaml::Value::Mapping(params),
        );

        overrides.push(serde_yaml::Value::Mapping(entry));
    }

    // Build the payload section preserving any existing content
    let existing_payload = doc
        .as_mapping()
        .and_then(|m| m.get(serde_yaml::Value::String("payload".into())));

    if overrides.is_empty() {
        if let Some(mapping) = doc.as_mapping_mut() {
            mapping.remove(serde_yaml::Value::String("payload".into()));
        }
    } else {
        let payload = if let Some(serde_yaml::Value::Mapping(existing)) = existing_payload {
            let mut p = existing.clone();
            p.insert(
                serde_yaml::Value::String("override".into()),
                serde_yaml::Value::Sequence(overrides),
            );
            p
        } else {
            let mut p = serde_yaml::Mapping::new();
            p.insert(
                serde_yaml::Value::String("override".into()),
                serde_yaml::Value::Sequence(overrides),
            );
            p
        };

        if let Some(mapping) = doc.as_mapping_mut() {
            mapping.insert(
                serde_yaml::Value::String("payload".into()),
                serde_yaml::Value::Mapping(payload),
            );
        }
    }

    let next = serde_yaml::to_string(&doc).map_err(|e| e.to_string())?;
    if next != raw {
        fs::write(config_path, next).map_err(|e| e.to_string())?;
    }

    Ok(())
}

/// Create directories and default config file if they don't exist.
pub fn ensure_storage_layout(settings: &Settings) -> Result<(), String> {
    let config_dir = app_config_dir()?;
    fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(Path::new(&settings.auth_dir)).map_err(|e| e.to_string())?;

    if let Some(parent) = Path::new(&settings.config_path).parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    if !Path::new(&settings.config_path).exists() {
        fs::write(
            &settings.config_path,
            default_config_yaml(&settings.auth_dir),
        )
        .map_err(|e| e.to_string())?;
    }

    ensure_config_has_auth_dir(Path::new(&settings.config_path), &settings.auth_dir)?;
    ensure_config_has_gateway(
        Path::new(&settings.config_path),
        settings.vercel_gateway_enabled,
        &settings.vercel_gateway_api_key,
    )?;
    ensure_config_has_codex_instructions(
        Path::new(&settings.config_path),
        settings.codex_instructions_enabled,
    )?;
    ensure_config_has_reasoning_overrides(Path::new(&settings.config_path), &settings.providers)?;
    Ok(())
}

/// Persist settings to disk, normalizing first.
pub fn save_settings_file(settings: &Settings) -> Result<(), String> {
    let mut next = settings.clone();
    normalize_settings(&mut next, None)?;
    ensure_storage_layout(&next)?;
    let path = settings_path()?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let content = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
    fs::write(path, content).map_err(|e| e.to_string())
}

/// Load settings from disk, applying defaults and legacy migration. Caches the result.
pub fn load_settings(cache: &std::sync::Mutex<Option<Settings>>) -> Result<Settings, String> {
    if let Some(cached) = cache.lock().unwrap().clone() {
        return Ok(cached);
    }

    let defaults = default_settings()?;
    let path = settings_path()?;
    let loaded = fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<PartialSettings>(&raw).ok());

    let mut settings = defaults.clone();

    let mut legacy_args: Option<String> = None;

    if let Some(partial) = loaded {
        if let Some(v) = partial.binary_path.filter(|v| !v.trim().is_empty()) {
            settings.binary_path = v.trim().to_string();
        }
        if let Some(v) = partial.args.filter(|v| !v.trim().is_empty()) {
            legacy_args = Some(v.trim().to_string());
        }
        if let Some(v) = partial.auth_dir.filter(|v| !v.trim().is_empty()) {
            settings.auth_dir = v.trim().to_string();
        }
        if let Some(v) = partial.config_path.filter(|v| !v.trim().is_empty()) {
            settings.config_path = v.trim().to_string();
        }
        if let Some(v) = partial.start_proxy_automatically {
            settings.start_proxy_automatically = v;
        }
        if let Some(v) = partial.providers.filter(|v| !v.is_empty()) {
            settings.providers = v;
        }
        if let Some(v) = partial.vercel_gateway_enabled {
            settings.vercel_gateway_enabled = v;
        }
        if let Some(v) = partial.vercel_gateway_api_key {
            settings.vercel_gateway_api_key = v;
        }
        if let Some(v) = partial.codex_instructions_enabled {
            settings.codex_instructions_enabled = v;
        }
    }

    normalize_settings(&mut settings, legacy_args.as_deref())?;

    *cache.lock().unwrap() = Some(settings.clone());
    Ok(settings)
}

/// Build the runtime CLI arguments for launching cli-proxy-api.
pub fn build_runtime_args(settings: &Settings) -> Vec<String> {
    vec!["-config".to_string(), settings.config_path.clone()]
}

/// Return the auth CLI flag for a known provider, or None if unsupported.
pub fn auth_flag_for_provider(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "codex" => Some("-codex-login"),
        "claude" => Some("-claude-login"),
        "gemini" => Some("-login"),
        "qwen" => Some("-qwen-login"),
        _ => None,
    }
}

/// Build the full authentication command string for a provider.
pub fn auth_command_for_provider(settings: &Settings, provider_id: &str) -> Option<String> {
    let flag = auth_flag_for_provider(provider_id)?;
    Some(format!(
        "{} -config \"{}\" {}",
        settings.binary_path, settings.config_path, flag
    ))
}

/// Check if the config file is valid (exists and has auth-dir).
pub fn config_is_valid(settings: &Settings) -> bool {
    let path = Path::new(&settings.config_path);

    if !path.exists() {
        if let Some(parent) = path.parent() {
            if fs::create_dir_all(parent).is_err() {
                return false;
            }
        }
        if fs::write(path, default_config_yaml(&settings.auth_dir)).is_err() {
            return false;
        }
    }

    if ensure_config_has_auth_dir(path, &settings.auth_dir).is_err() {
        return false;
    }

    let raw = match fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return false,
    };

    raw.lines()
        .any(|line| line.trim_start().starts_with("auth-dir:"))
}

/// Read the port number from a YAML config file.
pub fn read_config_port(config_path: &str) -> Option<u16> {
    let raw = fs::read_to_string(config_path).ok()?;
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("port:") {
            if let Ok(port) = rest.trim().parse::<u16>() {
                if port > 0 {
                    return Some(port);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tilde_home() {
        let expanded = expand_tilde("~");
        assert!(!expanded.contains('~'));
        assert!(expanded.starts_with('/'));
    }

    #[test]
    fn test_expand_tilde_path() {
        let expanded = expand_tilde("~/foo/bar");
        assert!(!expanded.contains('~'));
        assert!(expanded.ends_with("/foo/bar"));
    }

    #[test]
    fn test_expand_tilde_no_tilde() {
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
        assert_eq!(expand_tilde("relative/path"), "relative/path");
    }

    #[test]
    fn test_parse_config_path_from_args() {
        assert_eq!(
            parse_config_path_from_args("-config /etc/proxy.yaml"),
            Some("/etc/proxy.yaml".to_string())
        );
        assert_eq!(
            parse_config_path_from_args("--config=/etc/proxy.yaml"),
            Some("/etc/proxy.yaml".to_string())
        );
        assert_eq!(
            parse_config_path_from_args("-config=\"/etc/proxy.yaml\""),
            Some("/etc/proxy.yaml".to_string())
        );
        assert_eq!(parse_config_path_from_args("--other-flag value"), None);
        assert_eq!(parse_config_path_from_args(""), None);
    }

    #[test]
    fn test_normalize_auth_dir_path_empty() {
        let fallback = PathBuf::from("/home/user/.config/cli-proxy-api-ui/auth");
        let result = normalize_auth_dir_path("", &fallback);
        assert_eq!(result, fallback);
    }

    #[test]
    fn test_normalize_auth_dir_path_absolute() {
        let fallback = PathBuf::from("/default/auth");
        let result = normalize_auth_dir_path("/custom/auth", &fallback);
        assert_eq!(result, PathBuf::from("/custom/auth"));
    }

    #[test]
    fn test_normalize_config_file_path_appends_yaml() {
        let fallback = PathBuf::from("/default/config.yaml");
        let result = normalize_config_file_path("/some/dir/", &fallback);
        assert_eq!(result, PathBuf::from("/some/dir/config.yaml"));
    }

    #[test]
    fn test_auth_flag_for_provider() {
        assert_eq!(auth_flag_for_provider("codex"), Some("-codex-login"));
        assert_eq!(auth_flag_for_provider("claude"), Some("-claude-login"));
        assert_eq!(auth_flag_for_provider("gemini"), Some("-login"));
        assert_eq!(auth_flag_for_provider("qwen"), Some("-qwen-login"));
        assert_eq!(auth_flag_for_provider("unknown"), None);
    }

    #[test]
    fn test_default_providers_not_empty() {
        let providers = default_providers();
        assert!(!providers.is_empty());
        assert!(providers.iter().any(|p| p.id == "codex"));
        assert!(providers.iter().any(|p| p.id == "claude"));
        assert!(providers.iter().any(|p| p.id == "gemini"));
        assert!(providers.iter().any(|p| p.id == "qwen"));
    }

    #[test]
    fn test_default_config_yaml_contains_auth_dir() {
        let yaml = default_config_yaml("/test/auth");
        assert!(yaml.contains("auth-dir: \"/test/auth\""));
    }

    #[test]
    fn test_read_config_port() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("cli-proxy-api-ui-test-port");
        let _ = fs::create_dir_all(&dir);
        let config_path = dir.join("config.yaml");
        let mut f = fs::File::create(&config_path).unwrap();
        writeln!(f, "port: 8080").unwrap();
        writeln!(f, "debug: false").unwrap();

        assert_eq!(read_config_port(config_path.to_str().unwrap()), Some(8080));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_config_port_missing() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("cli-proxy-api-ui-test-noport");
        let _ = fs::create_dir_all(&dir);
        let config_path = dir.join("config.yaml");
        let mut f = fs::File::create(&config_path).unwrap();
        writeln!(f, "debug: false").unwrap();

        assert_eq!(read_config_port(config_path.to_str().unwrap()), None);

        let _ = fs::remove_dir_all(&dir);
    }
}

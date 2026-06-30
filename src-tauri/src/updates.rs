use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::settings::app_config_dir;

/// State for tracking release notifications across sessions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UpdateCheckState {
    #[serde(default)]
    last_notified_version: String,
}

fn state_path() -> Result<PathBuf, String> {
    Ok(app_config_dir()?.join("update-check.json"))
}

fn load_state() -> UpdateCheckState {
    let path = match state_path() {
        Ok(p) => p,
        Err(_) => return UpdateCheckState::default(),
    };
    fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<UpdateCheckState>(&raw).ok())
        .unwrap_or_default()
}

fn save_last_notified(version: &str) {
    let state = UpdateCheckState {
        last_notified_version: version.to_string(),
    };
    if let Ok(path) = state_path() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(content) = serde_json::to_string(&state) {
            let _ = fs::write(path, content);
        }
    }
}

/// Result returned to the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub release_url: String,
    /// Whether this is a new notification (user hasn't dismissed it before).
    pub is_new: bool,
}

/// GitHub release API response (only the fields we care about).
#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
}

/// Simple numeric version comparison.
/// Returns `true` if `latest` is strictly greater than `current`.
fn is_newer_version(latest: &str, current: &str) -> bool {
    let latest = latest.trim_start_matches('v');
    let current = current.trim_start_matches('v');

    let latest_parts: Vec<u64> = latest
        .split('.')
        .filter_map(|s| s.parse::<u64>().ok())
        .collect();
    let current_parts: Vec<u64> = current
        .split('.')
        .filter_map(|s| s.parse::<u64>().ok())
        .collect();

    for i in 0..latest_parts.len().max(current_parts.len()) {
        let l = latest_parts.get(i).copied().unwrap_or(0);
        let c = current_parts.get(i).copied().unwrap_or(0);
        if l > c {
            return true;
        }
        if l < c {
            return false;
        }
    }
    false
}

/// Check the latest GitHub release against the currently running version.
/// Returns `UpdateInfo` if a newer version exists, or `None` if up to date.
/// If the newer version was already notified before, `is_new` will be `false`.
pub fn check_latest_release(last_notified: &Mutex<String>) -> Result<Option<UpdateInfo>, String> {
    let current_version = env!("CARGO_PKG_VERSION");

    let url = "https://api.github.com/repos/officiallymarky/cli-proxy-api-ui/releases/latest";

    let response = ureq::get(url)
        .header("User-Agent", "cli-proxy-api-ui/1.0")
        .header("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| format!("Failed to fetch latest release: {e}"))?;

    if response.status() != 200 {
        return Err(format!("GitHub API returned status {}", response.status()));
    }

    let release: GithubRelease = response
        .into_body()
        .read_json()
        .map_err(|e| format!("Failed to parse GitHub response: {e}"))?;

    let latest_tag = release.tag_name.trim_start_matches('v').to_string();
    let current_clean = current_version.trim_start_matches('v');

    if !is_newer_version(&latest_tag, current_clean) {
        return Ok(None);
    }

    let mut guard = last_notified
        .lock()
        .map_err(|e| format!("Lock error: {e}"))?;

    let is_new = guard.as_str() != latest_tag;

    // Persist the notification if it's new so we don't re-notify.
    if is_new {
        *guard = latest_tag.clone();
        save_last_notified(&latest_tag);
    }

    Ok(Some(UpdateInfo {
        current_version: current_version.to_string(),
        latest_version: release.tag_name,
        release_url: release.html_url,
        is_new,
    }))
}

/// Initialize the last-notified state from disk.
pub fn load_last_notified_state() -> String {
    load_state().last_notified_version
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer_version() {
        assert!(is_newer_version("0.1.12", "0.1.11"));
        assert!(is_newer_version("0.2.0", "0.1.11"));
        assert!(is_newer_version("1.0.0", "0.9.99"));
        assert!(!is_newer_version("0.1.11", "0.1.11"));
        assert!(!is_newer_version("0.1.10", "0.1.11"));
        assert!(!is_newer_version("0.1.11", "0.1.12"));
        assert!(is_newer_version("v0.2.0", "0.1.11"));
        assert!(!is_newer_version("v0.1.10", "0.1.11"));
        assert!(is_newer_version("0.1.11", "0.1.10"));
        assert!(!is_newer_version("0.10.0", "0.10.0"));
    }
}

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use chrono::Utc;
use directories::ProjectDirs;
use image::ImageReader;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, State, WindowEvent};

const MAX_LOG_LINES: usize = 450;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Provider {
    id: String,
    name: String,
    enabled: bool,
    file_hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    binary_path: String,
    auth_dir: String,
    config_path: String,
    start_proxy_automatically: bool,
    providers: Vec<Provider>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartialSettings {
    binary_path: Option<String>,
    args: Option<String>,
    auth_dir: Option<String>,
    config_path: Option<String>,
    start_proxy_automatically: Option<bool>,
    providers: Option<Vec<Provider>>,
}

#[derive(Debug, Clone, Serialize)]
struct LogEntry {
    ts: String,
    source: String,
    line: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusResponse {
    running: bool,
    pid: Option<u32>,
    started_at: Option<String>,
    binary_available: bool,
    binary_resolved: Option<String>,
    config_valid: bool,
    command: String,
    listen_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProvidersResponse {
    providers: Vec<ProviderRuntime>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderRuntime {
    id: String,
    name: String,
    enabled: bool,
    file_hints: Vec<String>,
    connected: bool,
    auth_available: bool,
    auth_command: String,
}

#[derive(Debug, Serialize)]
struct LogsResponse {
    logs: Vec<LogEntry>,
}

#[derive(Default)]
struct AppState {
    settings_cache: Mutex<Option<Settings>>,
    server_child: Mutex<Option<Child>>,
    started_at: Mutex<Option<String>>,
    logs: Arc<Mutex<Vec<LogEntry>>>,
}

fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

fn app_config_dir() -> Result<PathBuf, String> {
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

fn settings_path() -> Result<PathBuf, String> {
    Ok(app_config_dir()?.join("settings.json"))
}

fn default_auth_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("auth")
}

fn default_config_path(config_dir: &Path) -> PathBuf {
    config_dir.join("config.yaml")
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

fn expand_tilde(raw: &str) -> String {
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

fn parse_config_path_from_args(args: &str) -> Option<String> {
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

fn normalize_auth_dir_path(raw: &str, fallback: &Path) -> PathBuf {
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

fn normalize_config_file_path(raw: &str, fallback: &Path) -> PathBuf {
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

fn normalize_settings(settings: &mut Settings, legacy_args: Option<&str>) -> Result<(), String> {
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

fn default_providers() -> Vec<Provider> {
    vec![
        Provider {
            id: "codex".into(),
            name: "Codex".into(),
            enabled: true,
            file_hints: vec!["codex".into(), "openai".into()],
        },
        Provider {
            id: "claude".into(),
            name: "Claude".into(),
            enabled: true,
            file_hints: vec!["claude".into(), "anthropic".into()],
        },
        Provider {
            id: "gemini".into(),
            name: "Gemini".into(),
            enabled: true,
            file_hints: vec!["gemini".into(), "google".into()],
        },
        Provider {
            id: "qwen".into(),
            name: "Qwen".into(),
            enabled: true,
            file_hints: vec!["qwen".into()],
        },
    ]
}

fn default_settings() -> Result<Settings, String> {
    let config_dir = app_config_dir()?;
    let auth_dir = default_auth_dir(&config_dir);
    let config_path = default_config_path(&config_dir);

    Ok(Settings {
        binary_path: "cli-proxy-api".to_string(),
        auth_dir: auth_dir.to_string_lossy().into_owned(),
        config_path: config_path.to_string_lossy().into_owned(),
        start_proxy_automatically: false,
        providers: default_providers(),
    })
}

fn default_config_yaml(auth_dir: &str) -> String {
    [
        "# CLI Proxy API UI generated config".to_string(),
        "# Update this file as needed for your environment.".to_string(),
        format!("auth-dir: \"{}\"", auth_dir),
        "debug: false".to_string(),
        "usage-statistics-enabled: false".to_string(),
        "".to_string(),
    ]
    .join("\n")
}

fn ensure_config_has_auth_dir(config_path: &Path, auth_dir: &str) -> Result<(), String> {
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

fn ensure_storage_layout(settings: &Settings) -> Result<(), String> {
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

    ensure_config_has_auth_dir(Path::new(&settings.config_path), &settings.auth_dir)
}

fn save_settings_file(settings: &Settings) -> Result<(), String> {
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

fn load_settings(state: &AppState) -> Result<Settings, String> {
    if let Some(cached) = state.settings_cache.lock().unwrap().clone() {
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
    }

    normalize_settings(&mut settings, legacy_args.as_deref())?;

    save_settings_file(&settings)?;
    *state.settings_cache.lock().unwrap() = Some(settings.clone());
    Ok(settings)
}

fn build_runtime_args(settings: &Settings) -> Vec<String> {
    vec!["-config".to_string(), settings.config_path.clone()]
}

fn push_log_arc(logs: &Arc<Mutex<Vec<LogEntry>>>, source: &str, text: &str) {
    let mut guard = logs.lock().unwrap();
    for line in text
        .lines()
        .map(|line| line.trim_end())
        .filter(|line| !line.is_empty())
    {
        guard.push(LogEntry {
            ts: now_iso(),
            source: source.to_string(),
            line: line.to_string(),
        });
        if guard.len() > MAX_LOG_LINES {
            guard.remove(0);
        }
    }
}

fn push_log(state: &AppState, source: &str, text: &str) {
    push_log_arc(&state.logs, source, text);
}

fn spawn_log_reader(
    logs: Arc<Mutex<Vec<LogEntry>>>,
    source: &'static str,
    pipe: impl Read + Send + 'static,
) {
    std::thread::spawn(move || {
        let reader = BufReader::new(pipe);
        for line in reader.lines().map_while(Result::ok) {
            push_log_arc(&logs, source, &line);
        }
    });
}

fn refresh_process_state(state: &AppState) {
    let mut child_guard = state.server_child.lock().unwrap();
    if let Some(child) = child_guard.as_mut() {
        if let Ok(Some(status)) = child.try_wait() {
            push_log(state, "system", &format!("Process exited ({status})"));
            *child_guard = None;
            *state.started_at.lock().unwrap() = None;
        }
    }
}

fn detect_providers(settings: &Settings) -> Vec<ProviderRuntime> {
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

fn config_is_valid(settings: &Settings) -> bool {
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

fn read_config_port(config_path: &str) -> Option<u16> {
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

fn auth_flag_for_provider(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "codex" => Some("-codex-login"),
        "claude" => Some("-claude-login"),
        "gemini" => Some("-login"),
        "qwen" => Some("-qwen-login"),
        _ => None,
    }
}

fn auth_command_for_provider(settings: &Settings, provider_id: &str) -> Option<String> {
    let flag = auth_flag_for_provider(provider_id)?;
    Some(format!(
        "{} -config \"{}\" {}",
        settings.binary_path, settings.config_path, flag
    ))
}

#[cfg(target_os = "linux")]
fn open_terminal_and_run(command: &str) -> Result<(), String> {
    let script = format!("{}; exec ${{SHELL:-/bin/sh}}", command);

    let run = |bin: &str, args: &[&str]| -> Result<bool, String> {
        if which::which(bin).is_err() {
            return Ok(false);
        }
        Command::new(bin)
            .args(args)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(true)
    };

    if run("x-terminal-emulator", &["-e", "sh", "-lc", &script])? {
        return Ok(());
    }
    if run("gnome-terminal", &["--", "sh", "-lc", &script])? {
        return Ok(());
    }
    if run("konsole", &["-e", "sh", "-lc", &script])? {
        return Ok(());
    }
    if run(
        "xfce4-terminal",
        &[
            "--command",
            &format!("sh -lc '{}'", script.replace('\'', "'\"'\"'")),
        ],
    )? {
        return Ok(());
    }
    if run("kitty", &["sh", "-lc", &script])? {
        return Ok(());
    }
    if run("alacritty", &["-e", "sh", "-lc", &script])? {
        return Ok(());
    }
    if run("wezterm", &["start", "--", "sh", "-lc", &script])? {
        return Ok(());
    }
    if run("xterm", &["-e", "sh", "-lc", &script])? {
        return Ok(());
    }

    Err("No supported terminal emulator found on PATH.".to_string())
}

#[cfg(target_os = "macos")]
fn open_terminal_and_run(command: &str) -> Result<(), String> {
    let escaped = command.replace('\\', "\\\\").replace('"', "\\\"");

    Command::new("osascript")
        .args([
            "-e",
            &format!("tell application \"Terminal\" to do script \"{}\"", escaped),
            "-e",
            "tell application \"Terminal\" to activate",
        ])
        .spawn()
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[cfg(target_os = "windows")]
fn open_terminal_and_run(command: &str) -> Result<(), String> {
    Command::new("cmd")
        .args(["/C", "start", "", "cmd", "/K", command])
        .spawn()
        .map_err(|e| e.to_string())?;

    Ok(())
}

fn resolve_binary(binary_path: &str) -> Option<PathBuf> {
    if binary_path.contains('/') || binary_path.contains('\\') {
        let candidate = PathBuf::from(binary_path);
        return candidate.exists().then_some(candidate);
    }

    which::which(binary_path).ok()
}

fn server_start_inner(state: &AppState) -> Result<(), String> {
    let settings = load_settings(state)?;
    refresh_process_state(state);

    if state.server_child.lock().unwrap().is_some() {
        return Err("Server is already running.".to_string());
    }

    let args = build_runtime_args(&settings);
    let mut child = Command::new(&settings.binary_path)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;

    if let Some(stdout) = child.stdout.take() {
        spawn_log_reader(state.logs.clone(), "stdout", stdout);
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_log_reader(state.logs.clone(), "stderr", stderr);
    }

    *state.started_at.lock().unwrap() = Some(now_iso());
    push_log(
        state,
        "system",
        &format!("Started: {} {}", settings.binary_path, args.join(" ")),
    );
    *state.server_child.lock().unwrap() = Some(child);
    Ok(())
}

fn server_stop_inner(state: &AppState) -> Result<(), String> {
    refresh_process_state(state);

    let mut child = match state.server_child.lock().unwrap().take() {
        Some(child) => child,
        None => return Err("Server is not running.".to_string()),
    };

    let pid = child.id();
    let _ = child.kill();
    let _ = child.wait();
    *state.started_at.lock().unwrap() = None;
    push_log(state, "system", &format!("Stopped process {pid}"));
    Ok(())
}

fn focus_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Result<Settings, String> {
    load_settings(&state)
}

#[tauri::command]
fn save_settings(state: State<AppState>, settings: Settings) -> Result<Settings, String> {
    let mut next = settings;
    normalize_settings(&mut next, None)?;
    save_settings_file(&next)?;
    *state.settings_cache.lock().unwrap() = Some(next.clone());
    Ok(next)
}

#[tauri::command]
fn get_status(state: State<AppState>) -> Result<StatusResponse, String> {
    let settings = load_settings(&state)?;
    refresh_process_state(&state);

    let child_guard = state.server_child.lock().unwrap();
    let command = format!(
        "{} {}",
        settings.binary_path,
        build_runtime_args(&settings).join(" ")
    );
    let binary_resolved =
        resolve_binary(&settings.binary_path).map(|p| p.to_string_lossy().to_string());
    let binary_available = binary_resolved.is_some();
    let config_valid = config_is_valid(&settings);
    let listen_url =
        read_config_port(&settings.config_path).map(|port| format!("http://localhost:{port}/v1"));

    Ok(StatusResponse {
        running: child_guard.is_some(),
        pid: child_guard.as_ref().map(|child| child.id()),
        started_at: state.started_at.lock().unwrap().clone(),
        binary_available,
        binary_resolved,
        config_valid,
        command,
        listen_url,
    })
}

#[tauri::command]
fn get_logs(state: State<AppState>) -> Result<LogsResponse, String> {
    Ok(LogsResponse {
        logs: state.logs.lock().unwrap().clone(),
    })
}

#[tauri::command]
fn get_providers(state: State<AppState>) -> Result<ProvidersResponse, String> {
    let settings = load_settings(&state)?;
    Ok(ProvidersResponse {
        providers: detect_providers(&settings),
    })
}

#[tauri::command]
fn server_start(state: State<AppState>) -> Result<(), String> {
    server_start_inner(&state)
}

#[tauri::command]
fn server_stop(state: State<AppState>) -> Result<(), String> {
    server_stop_inner(&state)
}

#[tauri::command]
fn server_restart(state: State<AppState>) -> Result<(), String> {
    let _ = server_stop_inner(&state);
    server_start_inner(&state)
}

#[tauri::command]
fn run_provider_auth(state: State<AppState>, provider_id: String) -> Result<(), String> {
    let settings = load_settings(&state)?;
    let provider_exists = settings
        .providers
        .iter()
        .any(|provider| provider.id == provider_id);

    if !provider_exists {
        return Err(format!("Unknown provider: {provider_id}"));
    }

    let command = auth_command_for_provider(&settings, &provider_id).ok_or_else(|| {
        format!(
            "Provider '{}' does not expose a direct auth flag in cli-proxy-api.",
            provider_id
        )
    })?;
    open_terminal_and_run(&command)?;
    Ok(())
}

fn decode_tray_icon() -> (u32, u32, Vec<u8>) {
    let bytes = include_bytes!("../icons/icon.png");
    let reader = ImageReader::new(std::io::Cursor::new(bytes.as_slice()))
        .with_guessed_format()
        .expect("icon format");
    let img = reader.decode().expect("icon decode");
    let rgba = img.to_rgba8();
    let w = rgba.width();
    let h = rgba.height();
    (w, h, rgba.into_raw())
}

fn install_desktop_entry() {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return,
    };
    let apps_dir = format!("{}/.local/share/applications", home);
    let icon_dir = format!("{}/.local/share/icons/hicolor/256x256/apps", home);
    let desktop_path = format!("{}/com.cliproxyapi.ui.desktop", apps_dir);
    let icon_path = format!("{}/com.cliproxyapi.ui.png", icon_dir);

    if Path::new(&desktop_path).exists() {
        return;
    }

    let _ = fs::create_dir_all(&apps_dir);
    let _ = fs::create_dir_all(&icon_dir);

    // Save icon PNG
    let png_bytes = include_bytes!("../icons/icon.png");
    let _ = fs::write(&icon_path, png_bytes);

    let entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=CLI Proxy API\n\
         Exec={}\n\
         Icon={}\n\
         StartupWMClass=com-cliproxyapi-ui\n\
         Categories=Development;\n\
         Terminal=false\n",
        std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "cli-proxy-api-ui".to_string()),
        icon_path
    );
    let _ = fs::write(&desktop_path, entry);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            get_status,
            get_logs,
            get_providers,
            server_start,
            server_stop,
            server_restart,
            run_provider_auth
        ])
        .setup(|app| {
            let open = MenuItemBuilder::with_id("open", "Open Control Panel").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&open, &quit]).build()?;

            let (w, h, rgba) = decode_tray_icon();
            let window_icon = tauri::image::Image::new_owned(rgba.clone(), w, h);
            let icon = app
                .default_window_icon()
                .cloned()
                .unwrap_or_else(|| tauri::image::Image::new_owned(rgba, w, h));

            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_icon(window_icon);
            }

            install_desktop_entry();

            let tray_builder = TrayIconBuilder::new()
                .menu(&menu)
                .icon(icon)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => focus_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        focus_main_window(tray.app_handle());
                    }
                });

            let _tray = tray_builder.build(app)?;

            let state = app.state::<AppState>();
            if let Ok(settings) = load_settings(&state) {
                if settings.start_proxy_automatically {
                    if let Err(err) = server_start_inner(&state) {
                        push_log(&state, "system", &format!("Auto-start failed: {err}"));
                    }
                }
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("failed to run tauri app");
}

fn main() {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
                std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
            }
            if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
                std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
            }
        }
        std::env::set_var("WAYLAND_APP_ID", "com.cliproxyapi.ui");
        std::env::set_var("GDK_APP_ID", "com.cliproxyapi.ui");
    }

    run();
}

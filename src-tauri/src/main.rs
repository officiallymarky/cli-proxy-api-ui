#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod process;
mod providers;
mod settings;
mod terminal;
mod tray;
mod updates;

use process::{
    force_stop, listener_reachable, push_log, refresh_process_state, resolve_binary,
    server_start_inner, server_stop_inner, supervise_proxy, AppState, LogsResponse, ProxyPhase,
    StatusResponse,
};
use providers::{detect_providers, ProvidersResponse};
use settings::{
    auth_command_for_provider, build_runtime_args, config_is_valid, load_settings,
    normalize_settings, read_config_port, save_settings_file, Settings,
};
use terminal::open_terminal_and_run;
use tray::decode_tray_icon;

use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, State, WindowEvent};

fn focus_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

// ── Tauri Commands ──────────────────────────────────────────────────────────

#[tauri::command]
fn get_settings(state: State<AppState>) -> Result<Settings, String> {
    load_settings(&state.settings_cache)
}

#[tauri::command]
fn save_settings(state: State<AppState>, settings: Settings) -> Result<Settings, String> {
    let mut next = settings;
    normalize_settings(&mut next, None)?;
    save_settings_file(&next)?;
    *state.settings_cache.lock() = Some(next.clone());
    Ok(next)
}

#[tauri::command]
fn get_status(state: State<AppState>) -> Result<StatusResponse, String> {
    let settings = load_settings(&state.settings_cache)?;
    refresh_process_state(&state);

    let child_guard = state.server_child.lock();
    let command = format!(
        "{} {}",
        settings.binary_path,
        build_runtime_args(&settings).join(" ")
    );
    let binary_resolved =
        resolve_binary(&settings.binary_path).map(|p| p.to_string_lossy().to_string());
    let binary_available = binary_resolved.is_some();
    let config_valid = config_is_valid(&settings);
    let port = read_config_port(&settings.config_path);
    let listen_url = port.map(|port| format!("http://127.0.0.1:{port}/v1"));
    let running = child_guard.is_some();
    let listener_reachable = running && port.map(listener_reachable).unwrap_or(false);
    let lifecycle_phase = state.proxy_lifecycle.lock().phase;
    let connection_message = if listener_reachable {
        "Connected".to_string()
    } else {
        match lifecycle_phase {
            ProxyPhase::Starting => "Starting proxy".to_string(),
            ProxyPhase::Failed => "Proxy startup failed".to_string(),
            ProxyPhase::Ready if port.is_none() => "Missing endpoint port".to_string(),
            ProxyPhase::Ready if running => "Connection unavailable".to_string(),
            ProxyPhase::Stopped | ProxyPhase::Ready => "Stopped".to_string(),
        }
    };

    drop(child_guard);

    Ok(StatusResponse {
        running,
        pid: state.server_child.lock().as_ref().map(|child| child.id()),
        started_at: state.started_at.lock().clone(),
        binary_available,
        binary_resolved,
        config_valid,
        command,
        listen_url,
        listener_reachable,
        connection_message,
    })
}

#[tauri::command]
fn get_logs(state: State<AppState>) -> Result<LogsResponse, String> {
    let logs = state.logs.lock();
    let start = logs.len().saturating_sub(80);
    Ok(LogsResponse {
        logs: logs[start..].to_vec(),
    })
}

#[tauri::command]
fn get_providers(state: State<AppState>) -> Result<ProvidersResponse, String> {
    let settings = load_settings(&state.settings_cache)?;
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
    let settings = load_settings(&state.settings_cache)?;
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

    // Split command into binary + args to avoid shell injection.
    let parts: Vec<&str> = command.split_whitespace().collect();
    let binary = parts.first().unwrap_or(&"");
    let args: Vec<&str> = parts[1..].to_vec();
    open_terminal_and_run(binary, &args)?;
    Ok(())
}

#[tauri::command]
fn check_updates(state: State<AppState>) -> Result<Option<updates::UpdateInfo>, String> {
    updates::check_latest_release(&state.last_notified_version)
}

/// Perform an update check and emit a `new-release` event to the frontend if a new version is found.
fn perform_update_check(app_handle: &tauri::AppHandle) {
    let state = app_handle.state::<AppState>();
    match updates::check_latest_release(&state.last_notified_version) {
        Ok(Some(info)) => {
            let _ = app_handle.emit("new-release", &info);
        }
        Ok(None) => {}
        Err(e) => {
            // Silently ignore errors in background checks (network flakiness, etc.)
            let state = app_handle.state::<AppState>();
            crate::process::push_log(&state, "system", &format!("Update check failed: {e}"));
        }
    }
}

// ── App Entry Point ─────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            focus_main_window(app);
        }))
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
            run_provider_auth,
            check_updates
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

            let tray_builder = TrayIconBuilder::new()
                .menu(&menu)
                .icon(icon)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => focus_main_window(app),
                    "quit" => {
                        let state = app.state::<AppState>();
                        force_stop(&state);
                        app.exit(0);
                    }
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
            if let Ok(settings) = load_settings(&state.settings_cache) {
                // Minimize to tray on startup if enabled
                if settings.start_minimized {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.hide();
                    }
                }

                if settings.start_proxy_automatically {
                    if let Err(err) = server_start_inner(&state) {
                        push_log(&state, "system", &format!("Auto-start failed: {err}"));
                    }
                }
            }

            // Supervise readiness and retries independently of the webview's status polling.
            let supervisor_handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                let state = supervisor_handle.state::<AppState>();
                supervise_proxy(&state);
            });

            // Initialize update notification state from disk.
            {
                *state.last_notified_version.lock() = updates::load_last_notified_state();
            }

            // Spawn a background thread to periodically check for new releases.
            let updater_handle = app.handle().clone();
            std::thread::spawn(move || {
                // Initial check after a short delay.
                std::thread::sleep(std::time::Duration::from_secs(5));
                perform_update_check(&updater_handle);

                // Repeat once per day while the application remains open.
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(24 * 3600));
                    perform_update_check(&updater_handle);
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build tauri app")
        .run(|app_handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let state = app_handle.state::<AppState>();
                force_stop(&state);
            }
        });
}

fn main() {
    #[cfg(target_os = "linux")]
    {
        let is_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").ok().as_deref() == Some("wayland");

        if is_wayland {
            if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
                std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
            }
            if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
                std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
            }
        }

        if std::env::var_os("WAYLAND_APP_ID").is_none() {
            std::env::set_var("WAYLAND_APP_ID", "com.cliproxyapi.ui");
        }
        if std::env::var_os("GDK_APP_ID").is_none() {
            std::env::set_var("GDK_APP_ID", "com.cliproxyapi.ui");
        }
    }

    run();
}

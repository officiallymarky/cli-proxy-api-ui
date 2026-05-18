use crate::settings::{
    build_runtime_args, load_settings, read_config_port, LogEntry, Settings, MAX_LOG_LINES,
};
use chrono::Utc;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

/// Shared application state managed by Tauri.
#[derive(Default)]
pub struct AppState {
    pub settings_cache: Mutex<Option<Settings>>,
    pub server_child: Mutex<Option<Child>>,
    pub started_at: Mutex<Option<String>>,
    pub logs: Arc<Mutex<Vec<LogEntry>>>,
}

/// Current status of the proxy process.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusResponse {
    pub running: bool,
    pub pid: Option<u32>,
    pub started_at: Option<String>,
    pub binary_available: bool,
    pub binary_resolved: Option<String>,
    pub config_valid: bool,
    pub command: String,
    pub listen_url: Option<String>,
}

/// Collection of log entries returned to the frontend.
#[derive(Debug, serde::Serialize)]
pub struct LogsResponse {
    pub logs: Vec<LogEntry>,
}

fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// Push log lines into the shared log buffer, trimming old entries.
pub fn push_log_arc(logs: &Arc<Mutex<Vec<LogEntry>>>, source: &str, text: &str) {
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
    }
    let over = guard.len().saturating_sub(MAX_LOG_LINES);
    if over > 0 {
        guard.drain(0..over);
        guard.shrink_to_fit();
    }
}

/// Push log lines from the AppState.
pub fn push_log(state: &AppState, source: &str, text: &str) {
    push_log_arc(&state.logs, source, text);
}

/// Spawn a background thread to read lines from a pipe into the log buffer.
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

/// Check if the child process has exited and clean up if so.
pub fn refresh_process_state(state: &AppState) {
    let mut child_guard = state.server_child.lock().unwrap();
    if let Some(child) = child_guard.as_mut() {
        if let Ok(Some(status)) = child.try_wait() {
            push_log(state, "system", &format!("Process exited ({status})"));
            *child_guard = None;
            *state.started_at.lock().unwrap() = None;
        }
    }
}

/// Resolve a binary path: check if it's an absolute/relative path, otherwise search PATH.
/// Falls back to searching common user directories if the desktop environment's PATH is minimal.
pub fn resolve_binary(binary_path: &str) -> Option<PathBuf> {
    if binary_path.contains('/') || binary_path.contains('\\') {
        let candidate = PathBuf::from(binary_path);
        return candidate.exists().then_some(candidate);
    }

    which::which(binary_path).ok().or_else(|| {
        // Fallback: search common directories that may not be in the desktop PATH.
        let home = std::env::var("HOME").ok();
        let dirs: Vec<String> = home
            .map(|h| {
                vec![
                    format!("{h}/.local/bin"),
                    format!("{h}/.cargo/bin"),
                    format!("{h}/bin"),
                    "/usr/local/bin".into(),
                ]
            })
            .unwrap_or_else(|| vec!["/usr/local/bin".into()]);

        dirs.iter()
            .map(|d| PathBuf::from(d).join(binary_path))
            .find(|p| p.exists())
    })
}

/// Kill any stale cli-proxy-api process listening on the given port.
fn kill_stale_on_port(port: u16, logs: &std::sync::Arc<std::sync::Mutex<Vec<LogEntry>>>) {
    // Use lsof to find PIDs listening on the port.
    let output = match Command::new("lsof")
        .args(["-i", &format!(":{port}"), "-t", "-sTCP:LISTEN"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return,
    };

    if !output.status.success() {
        return;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for pid_str in stdout.lines() {
        let pid_str = pid_str.trim();
        if pid_str.is_empty() {
            continue;
        }
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };

        // Verify it's a cli-proxy-api process.
        let cmdline_path = format!("/proc/{pid}/cmdline");
        let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) else {
            continue;
        };
        let cmd = cmdline.replace('\0', " ");
        if !cmd.contains("cli-proxy-api") {
            push_log_arc(
                logs,
                "system",
                &format!("Port {port} used by another process (PID {pid}), skipping"),
            );
            continue;
        }

        push_log_arc(
            logs,
            "system",
            &format!("Killing stale cli-proxy-api (PID {pid}) on port {port}"),
        );
        let _ = Command::new("kill").arg(pid_str).status();
        // Give the OS a moment to release the port.
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// Start the cli-proxy-api process, capturing stdout/stderr into logs.
///
/// Enhances the inherited PATH with common user directories so the binary
/// can be found even when the app is launched from a desktop entry (which
/// inherits a minimal desktop-environment PATH).
pub fn server_start_inner(state: &AppState) -> Result<(), String> {
    let settings = load_settings(&state.settings_cache)?;
    refresh_process_state(state);

    if state.server_child.lock().unwrap().is_some() {
        return Err("Server is already running.".to_string());
    }

    // Kill any stale process on the configured port.
    if let Some(port) = read_config_port(&settings.config_path) {
        kill_stale_on_port(port, &state.logs);
    }

    let args = build_runtime_args(&settings);
    let env_path = enhanced_path();

    let mut child = Command::new(&settings.binary_path)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PATH", &env_path)
        .spawn()
        .map_err(|e| {
            format!(
                "Failed to spawn '{}': {e}\nPATH={env_path}",
                settings.binary_path
            )
        })?;

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

/// Build an enhanced PATH that includes common user binary directories.
///
/// Desktop environments launched from a display manager (SDDM, GDM, etc.)
/// often have a minimal PATH that doesn't include directories added by
/// shell init files like `.bashrc` or `.zshrc`. This function ensures
/// those directories are available.
fn enhanced_path() -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    let home = std::env::var("HOME").ok();

    let mut extras: Vec<String> = Vec::new();
    if let Some(ref h) = home {
        extras.push(format!("{h}/.local/bin"));
        extras.push(format!("{h}/.cargo/bin"));
        extras.push(format!("{h}/bin"));
    }
    extras.push("/usr/local/bin".into());
    extras.push("/usr/local/sbin".into());

    let mut parts: Vec<String> = current.split(':').map(String::from).collect();
    for dir in extras {
        if !parts.iter().any(|p| p == &dir) {
            parts.push(dir);
        }
    }
    parts.join(":")
}

/// Stop the cli-proxy-api process, killing it if still running.
pub fn server_stop_inner(state: &AppState) -> Result<(), String> {
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

/// Force-kill the child process without error (used during app shutdown).
pub fn force_stop(state: &AppState) {
    if let Some(mut child) = state.server_child.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

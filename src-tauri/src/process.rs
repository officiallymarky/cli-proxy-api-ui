use crate::settings::{
    build_runtime_args, load_settings, read_config_port, LogEntry, Settings, MAX_LOG_LINES,
};
use chrono::Utc;
use parking_lot::Mutex;
use std::io::{BufRead, BufReader, Read};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const LISTENER_FAILURE_GRACE: Duration = Duration::from_secs(10);
const MAX_AUTO_RETRIES: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyPhase {
    Stopped,
    Starting,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Copy)]
pub struct ProxyLifecycle {
    pub phase: ProxyPhase,
    phase_started_at: Option<Instant>,
    unreachable_since: Option<Instant>,
    retry_attempts: u8,
    next_retry_at: Option<Instant>,
}

impl Default for ProxyLifecycle {
    fn default() -> Self {
        Self {
            phase: ProxyPhase::Stopped,
            phase_started_at: None,
            unreachable_since: None,
            retry_attempts: 0,
            next_retry_at: None,
        }
    }
}

/// Shared application state managed by Tauri.
#[derive(Default)]
pub struct AppState {
    pub settings_cache: Mutex<Option<Settings>>,
    pub server_child: Mutex<Option<Child>>,
    pub started_at: Mutex<Option<String>>,
    pub logs: Arc<Mutex<Vec<LogEntry>>>,
    pub proxy_lifecycle: Mutex<ProxyLifecycle>,
    process_operation: Mutex<()>,
    pub last_notified_version: Mutex<String>,
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
    pub listener_reachable: bool,
    pub connection_message: String,
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
    let mut guard = logs.lock();
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

/// Check if the child process has exited and update its lifecycle.
pub fn refresh_process_state(state: &AppState) {
    let exit_status = {
        let mut child_guard = state.server_child.lock();
        let status = child_guard
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten());
        if status.is_some() {
            *child_guard = None;
        }
        status
    };

    if let Some(status) = exit_status {
        push_log(state, "system", &format!("Process exited ({status})"));
        *state.started_at.lock() = None;
        record_process_failure(state);
    }
}

/// Check whether the configured proxy listener is accepting local TCP connections.
pub fn listener_reachable(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok()
}

#[derive(Debug, PartialEq, Eq)]
enum SupervisionAction {
    None,
    MarkReady,
    ClearListenerGrace,
    StartListenerGrace,
    FailStartup,
    FailListener,
}

fn supervision_action(
    lifecycle: ProxyLifecycle,
    listener_reachable: bool,
    now: Instant,
) -> SupervisionAction {
    if listener_reachable {
        return match (lifecycle.phase, lifecycle.unreachable_since) {
            (ProxyPhase::Ready, Some(_)) => SupervisionAction::ClearListenerGrace,
            (ProxyPhase::Ready, None) => SupervisionAction::None,
            _ => SupervisionAction::MarkReady,
        };
    }

    match lifecycle.phase {
        ProxyPhase::Starting
            if lifecycle
                .phase_started_at
                .is_some_and(|started| now.duration_since(started) >= STARTUP_TIMEOUT) =>
        {
            SupervisionAction::FailStartup
        }
        ProxyPhase::Ready => match lifecycle.unreachable_since {
            Some(unreachable_since)
                if now.duration_since(unreachable_since) >= LISTENER_FAILURE_GRACE =>
            {
                SupervisionAction::FailListener
            }
            Some(_) => SupervisionAction::None,
            None => SupervisionAction::StartListenerGrace,
        },
        _ => SupervisionAction::None,
    }
}

fn retry_delay(retry_attempts: u8) -> Duration {
    Duration::from_secs(1_u64 << u32::from(retry_attempts.min(5)))
}

fn auto_retry_due(lifecycle: ProxyLifecycle, auto_start_enabled: bool, now: Instant) -> bool {
    auto_start_enabled
        && lifecycle.phase == ProxyPhase::Failed
        && lifecycle.retry_attempts < MAX_AUTO_RETRIES
        && lifecycle
            .next_retry_at
            .is_some_and(|retry_at| now >= retry_at)
}

fn record_process_failure(state: &AppState) {
    let mut lifecycle = state.proxy_lifecycle.lock();
    lifecycle.phase = ProxyPhase::Failed;
    lifecycle.phase_started_at = None;
    lifecycle.unreachable_since = None;
    lifecycle.next_retry_at = Some(Instant::now() + retry_delay(lifecycle.retry_attempts));
}

fn terminate_failed_process(state: &AppState, reason: &str) {
    let _operation_guard = state.process_operation.lock();
    let child = state.server_child.lock().take();
    let Some(mut child) = child else {
        return;
    };

    let pid = child.id();
    let _ = child.kill();
    let _ = child.wait();
    *state.started_at.lock() = None;
    record_process_failure(state);
    push_log(state, "system", &format!("{reason}; stopped process {pid}"));
}

/// Monitor listener readiness independently of frontend status requests.
///
/// A newly spawned process receives a full startup grace period. Unexpected
/// exits and listener failures are retried with bounded exponential backoff
/// only when proxy auto-start is enabled.
pub fn supervise_proxy(state: &AppState) {
    refresh_process_state(state);

    let Ok(settings) = load_settings(&state.settings_cache) else {
        return;
    };
    let running = state.server_child.lock().is_some();
    let now = Instant::now();
    let lifecycle = *state.proxy_lifecycle.lock();

    if running {
        let reachable = read_config_port(&settings.config_path)
            .map(listener_reachable)
            .unwrap_or(false);

        match supervision_action(lifecycle, reachable, now) {
            SupervisionAction::None => {}
            SupervisionAction::MarkReady => {
                let mut lifecycle = state.proxy_lifecycle.lock();
                lifecycle.phase = ProxyPhase::Ready;
                lifecycle.phase_started_at = None;
                lifecycle.unreachable_since = None;
                lifecycle.retry_attempts = 0;
                lifecycle.next_retry_at = None;
                drop(lifecycle);
                push_log(state, "system", "Proxy listener is ready");
            }
            SupervisionAction::ClearListenerGrace => {
                state.proxy_lifecycle.lock().unreachable_since = None;
            }
            SupervisionAction::StartListenerGrace => {
                let mut lifecycle = state.proxy_lifecycle.lock();
                if lifecycle.phase == ProxyPhase::Ready && lifecycle.unreachable_since.is_none() {
                    lifecycle.unreachable_since = Some(now);
                }
            }
            SupervisionAction::FailStartup => {
                terminate_failed_process(state, "Proxy did not become ready within 30 seconds");
            }
            SupervisionAction::FailListener => {
                terminate_failed_process(
                    state,
                    "Proxy listener remained unreachable for 10 seconds",
                );
            }
        }
        return;
    }

    if auto_retry_due(lifecycle, settings.start_proxy_automatically, now) {
        let attempt = {
            let mut lifecycle = state.proxy_lifecycle.lock();
            if !auto_retry_due(*lifecycle, settings.start_proxy_automatically, now) {
                return;
            }
            lifecycle.retry_attempts += 1;
            lifecycle.next_retry_at = None;
            lifecycle.retry_attempts
        };

        push_log(
            state,
            "system",
            &format!("Retrying proxy startup ({attempt}/{MAX_AUTO_RETRIES})"),
        );
        if let Err(err) = server_start_retry_inner(state) {
            push_log(state, "system", &format!("Automatic retry failed: {err}"));
        }
    } else if settings.start_proxy_automatically
        && lifecycle.phase == ProxyPhase::Failed
        && lifecycle.retry_attempts >= MAX_AUTO_RETRIES
        && lifecycle.next_retry_at.is_some()
    {
        state.proxy_lifecycle.lock().next_retry_at = None;
        push_log(
            state,
            "system",
            "Proxy startup retry limit reached; manual action is required",
        );
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
fn kill_stale_on_port(port: u16, logs: &Arc<Mutex<Vec<LogEntry>>>) {
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
fn server_start_with_retry_state(state: &AppState, reset_retries: bool) -> Result<(), String> {
    let _operation_guard = state.process_operation.lock();
    let settings = load_settings(&state.settings_cache)?;
    refresh_process_state(state);

    if state.server_child.lock().is_some() {
        return Err("Server is already running.".to_string());
    }

    {
        let mut lifecycle = state.proxy_lifecycle.lock();
        lifecycle.phase = ProxyPhase::Starting;
        lifecycle.phase_started_at = Some(Instant::now());
        lifecycle.unreachable_since = None;
        lifecycle.next_retry_at = None;
        if reset_retries {
            lifecycle.retry_attempts = 0;
        }
    }

    // Kill any stale process on the configured port.
    if let Some(port) = read_config_port(&settings.config_path) {
        kill_stale_on_port(port, &state.logs);
    }

    let args = build_runtime_args(&settings);
    let env_path = enhanced_path();

    let spawn_result = Command::new(&settings.binary_path)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PATH", &env_path)
        .spawn();
    let mut child = match spawn_result {
        Ok(child) => child,
        Err(err) => {
            record_process_failure(state);
            return Err(format!(
                "Failed to spawn '{}': {err}\nPATH={env_path}",
                settings.binary_path
            ));
        }
    };

    if let Some(stdout) = child.stdout.take() {
        spawn_log_reader(state.logs.clone(), "stdout", stdout);
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_log_reader(state.logs.clone(), "stderr", stderr);
    }

    *state.started_at.lock() = Some(now_iso());
    push_log(
        state,
        "system",
        &format!("Started: {} {}", settings.binary_path, args.join(" ")),
    );
    *state.server_child.lock() = Some(child);
    Ok(())
}

pub fn server_start_inner(state: &AppState) -> Result<(), String> {
    server_start_with_retry_state(state, true)
}

fn server_start_retry_inner(state: &AppState) -> Result<(), String> {
    server_start_with_retry_state(state, false)
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
    let _operation_guard = state.process_operation.lock();
    refresh_process_state(state);

    let child = state.server_child.lock().take();
    {
        let mut lifecycle = state.proxy_lifecycle.lock();
        *lifecycle = ProxyLifecycle::default();
    }
    *state.started_at.lock() = None;

    let Some(mut child) = child else {
        return Err("Server is not running.".to_string());
    };

    let pid = child.id();
    let _ = child.kill();
    let _ = child.wait();
    push_log(state, "system", &format!("Stopped process {pid}"));
    Ok(())
}

/// Force-kill the child process without error (used during app shutdown).
pub fn force_stop(state: &AppState) {
    let _operation_guard = state.process_operation.lock();
    if let Some(mut child) = state.server_child.lock().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    *state.started_at.lock() = None;
    *state.proxy_lifecycle.lock() = ProxyLifecycle::default();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starting_process_gets_full_readiness_grace_period() {
        let started = Instant::now();
        let lifecycle = ProxyLifecycle {
            phase: ProxyPhase::Starting,
            phase_started_at: Some(started),
            ..ProxyLifecycle::default()
        };

        assert_eq!(
            supervision_action(lifecycle, false, started + Duration::from_secs(29)),
            SupervisionAction::None
        );
        assert_eq!(
            supervision_action(lifecycle, false, started + STARTUP_TIMEOUT),
            SupervisionAction::FailStartup
        );
        assert_eq!(
            supervision_action(lifecycle, true, started),
            SupervisionAction::MarkReady
        );
    }

    #[test]
    fn ready_process_gets_listener_failure_grace_period() {
        let unreachable_since = Instant::now();
        let lifecycle = ProxyLifecycle {
            phase: ProxyPhase::Ready,
            unreachable_since: Some(unreachable_since),
            ..ProxyLifecycle::default()
        };

        assert_eq!(
            supervision_action(lifecycle, false, unreachable_since + Duration::from_secs(9)),
            SupervisionAction::None
        );
        assert_eq!(
            supervision_action(lifecycle, true, unreachable_since + Duration::from_secs(9)),
            SupervisionAction::ClearListenerGrace
        );
        assert_eq!(
            supervision_action(lifecycle, false, unreachable_since + LISTENER_FAILURE_GRACE),
            SupervisionAction::FailListener
        );
    }

    #[test]
    fn automatic_retries_are_delayed_and_bounded() {
        let now = Instant::now();
        let mut lifecycle = ProxyLifecycle {
            phase: ProxyPhase::Failed,
            next_retry_at: Some(now),
            ..ProxyLifecycle::default()
        };

        assert!(auto_retry_due(lifecycle, true, now));
        assert!(!auto_retry_due(lifecycle, false, now));

        lifecycle.retry_attempts = MAX_AUTO_RETRIES;
        assert!(!auto_retry_due(lifecycle, true, now));
        assert_eq!(retry_delay(0), Duration::from_secs(1));
        assert_eq!(retry_delay(1), Duration::from_secs(2));
        assert_eq!(retry_delay(2), Duration::from_secs(4));
    }

    #[cfg(unix)]
    #[test]
    fn exited_auto_started_process_is_retried() {
        let mut settings = crate::settings::default_settings().unwrap();
        settings.binary_path = "/bin/false".to_string();
        settings.config_path = "/nonexistent/cli-proxy-api-ui-smoke.yaml".to_string();
        settings.start_proxy_automatically = true;

        let state = AppState::default();
        *state.settings_cache.lock() = Some(settings);
        server_start_inner(&state).unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            refresh_process_state(&state);
            if state.proxy_lifecycle.lock().phase == ProxyPhase::Failed {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            state.proxy_lifecycle.lock().phase,
            ProxyPhase::Failed,
            "the child should be observed exiting"
        );

        state.proxy_lifecycle.lock().next_retry_at = Some(Instant::now());
        supervise_proxy(&state);

        let lifecycle = *state.proxy_lifecycle.lock();
        assert_eq!(lifecycle.phase, ProxyPhase::Starting);
        assert_eq!(lifecycle.retry_attempts, 1);
        assert!(state
            .logs
            .lock()
            .iter()
            .any(|entry| entry.line.contains("Retrying proxy startup (1/3)")));

        force_stop(&state);
    }
}

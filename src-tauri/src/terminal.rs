use std::process::Command;

/// Open a terminal emulator and run a command, using explicit args to avoid shell injection.
///
/// On Linux, tries several common terminal emulators.
/// On macOS, uses osascript to open Terminal.app.
/// On Windows, uses cmd /K.
#[cfg(target_os = "linux")]
pub fn open_terminal_and_run(binary: &str, args: &[&str]) -> Result<(), String> {
    let try_launch = |bin: &str, launch_args: &[&str]| -> Result<bool, String> {
        if which::which(bin).is_err() {
            return Ok(false);
        }
        Command::new(bin)
            .args(launch_args)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(true)
    };

    // Build the command string safely: only known binary and args are interpolated.
    let script = format!(
        "{} {}; exec ${{SHELL:-/bin/sh}}",
        shell_escape(binary),
        args.iter()
            .map(|a| shell_escape(a))
            .collect::<Vec<_>>()
            .join(" ")
    );

    if try_launch("x-terminal-emulator", &["-e", "sh", "-lc", &script])? {
        return Ok(());
    }
    if try_launch("gnome-terminal", &["--", "sh", "-lc", &script])? {
        return Ok(());
    }
    if try_launch("konsole", &["-e", "sh", "-lc", &script])? {
        return Ok(());
    }
    if try_launch(
        "xfce4-terminal",
        &[
            "--command",
            &format!("sh -lc '{}'", script.replace('\'', "'\"'\"'")),
        ],
    )? {
        return Ok(());
    }
    if try_launch("kitty", &["sh", "-lc", &script])? {
        return Ok(());
    }
    if try_launch("alacritty", &["-e", "sh", "-lc", &script])? {
        return Ok(());
    }
    if try_launch("wezterm", &["start", "--", "sh", "-lc", &script])? {
        return Ok(());
    }
    if try_launch("xterm", &["-e", "sh", "-lc", &script])? {
        return Ok(());
    }

    Err("No supported terminal emulator found on PATH.".to_string())
}

#[cfg(target_os = "macos")]
pub fn open_terminal_and_run(binary: &str, args: &[&str]) -> Result<(), String> {
    let full_cmd = format!("{} {}", binary, args.join(" "));
    let escaped = full_cmd.replace('\\', "\\\\").replace('"', "\\\"");

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
pub fn open_terminal_and_run(binary: &str, args: &[&str]) -> Result<(), String> {
    let full_cmd = format!("{} {}", binary, args.join(" "));
    Command::new("cmd")
        .args(["/C", "start", "", "cmd", "/K", &full_cmd])
        .spawn()
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Simple shell escape: wraps in single quotes and escapes embedded single quotes.
fn shell_escape(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.chars().all(|c| {
        c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '/' || c == ':'
    }) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_escape_safe() {
        assert_eq!(shell_escape("hello"), "hello");
        assert_eq!(shell_escape("/usr/bin/foo"), "/usr/bin/foo");
    }

    #[test]
    fn test_shell_escape_spaces() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
    }

    #[test]
    fn test_shell_escape_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_shell_escape_empty() {
        assert_eq!(shell_escape(""), "''");
    }
}

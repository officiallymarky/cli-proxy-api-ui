use image::ImageReader;
use std::fs;
use std::path::PathBuf;

/// Decode the embedded tray icon PNG into raw RGBA bytes.
pub fn decode_tray_icon() -> (u32, u32, Vec<u8>) {
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

/// Resolve the desktop entry Exec path.
///
/// In dev mode (`target/debug/`), swaps to the release binary path so the
/// menu entry works without a dev server. In production, uses the binary directly.
fn resolve_desktop_exec() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let exe_str = exe.to_string_lossy();

    // Dev build: target/debug/cli-proxy-api-ui → target/release/cli-proxy-api-ui
    if exe_str.contains("/target/debug/") {
        let release = PathBuf::from(exe_str.replace("/target/debug/", "/target/release/"));
        // Return release path if it exists, otherwise skip creating the entry.
        if release.exists() {
            return Some(release.to_string_lossy().to_string());
        }
        eprintln!("[desktop] release binary not found at {release:?}");
        eprintln!("[desktop] run: npm run build:binary");
        return None;
    }

    // Production or installed binary — use directly.
    Some(exe_str.to_string())
}

/// Install or update a .desktop entry and icon for Linux desktop integration.
///
/// Always uses the release binary path so the menu entry works standalone.
pub fn install_desktop_entry() {
    #[cfg(not(target_os = "linux"))]
    {
        return;
    }

    #[cfg(target_os = "linux")]
    install_desktop_entry_linux();
}

#[cfg(target_os = "linux")]
fn install_desktop_entry_linux() {
    let home = match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => h,
        _ => {
            eprintln!("[desktop] HOME not set, skipping desktop entry");
            return;
        }
    };

    let apps_dir = format!("{}/.local/share/applications", home);
    let icon_dir = format!("{}/.local/share/icons/hicolor/256x256/apps", home);
    let desktop_path = format!("{}/com.cliproxyapi.ui.desktop", apps_dir);
    let icon_path = format!("{}/com.cliproxyapi.ui.png", icon_dir);

    if let Err(e) = fs::create_dir_all(&apps_dir) {
        eprintln!("[desktop] cannot create {}: {e}", apps_dir);
        return;
    }
    if let Err(e) = fs::create_dir_all(&icon_dir) {
        eprintln!("[desktop] cannot create {}: {e}", icon_dir);
        return;
    }

    let png_bytes = include_bytes!("../icons/icon.png");
    if let Err(e) = fs::write(&icon_path, png_bytes) {
        eprintln!("[desktop] cannot write icon to {icon_path}: {e}");
    }

    let exec_path = match resolve_desktop_exec() {
        Some(p) => p,
        None => return,
    };

    let entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=CLI Proxy API UI\n\
         Exec={}\n\
         Icon={}\n\
         StartupWMClass=com-cliproxyapi-ui\n\
         Categories=Development;\n\
         Terminal=false\n",
        exec_path, icon_path
    );

    match fs::write(&desktop_path, &entry) {
        Ok(()) => {
            eprintln!("[desktop] wrote {}", desktop_path);
            eprintln!("[desktop] Exec={}", exec_path);
        }
        Err(e) => {
            eprintln!("[desktop] cannot write {}: {e}", desktop_path);
        }
    }
}

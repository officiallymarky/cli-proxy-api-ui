# CLI Proxy API UI

A desktop control panel for managing `cli-proxy-api`, built with Tauri v2 for native tray behavior on Linux, macOS, and Windows.

## What it does

- Start, stop, and restart your local `cli-proxy-api` process
- Toggle runtime preferences (`start proxy automatically`)
- Detect connected providers by scanning auth files in your configured auth directory
- Show live process logs directly in the UI
- Provide one-click copy for provider auth commands
- Uses native IPC commands (no local HTTP endpoint or embedded web server)

## Run desktop app

```bash
npm install
npm run desktop
```

This launches the desktop app and keeps it available in your system tray/menu bar. Closing the window hides it to tray; use the tray menu to reopen or quit.

Wayland sessions are auto-detected and webkit compatibility flags are applied automatically at startup.

## Build installers

```bash
npm run dist
```

## macOS note

On Apple Silicon Macs, the app may show "is damaged and cannot be opened" after downloading from a browser. This is caused by macOS Gatekeeper quarantine on ad-hoc signed apps. To fix it, run:

```bash
xattr -d com.apple.quarantine /Applications/CLI\ Proxy\ API\ UI.app
```

Right-click → **Open** in Finder also works on first launch as an alternative.

## Platform defaults

- Settings are saved in the platform config directory (`ProjectDirs`) under `cli-proxy-api-ui/settings.json`
- Auth directory is managed automatically at `<platform-config>/cli-proxy-api-ui/auth`
- Config file is managed automatically at `<platform-config>/cli-proxy-api-ui/config.yaml`
- Relative paths and `~/...` paths are normalized automatically
- If config path is set to a directory, `config.yaml` is appended automatically
- Launch command is generated as `cli-proxy-api -config <resolved-config-path>`
- Tray integration depends on your desktop environment supporting StatusNotifier/AppIndicator

## Suggested setup

1. Install `cli-proxy-api` and make sure it is in your `PATH`.
2. Optional: enable **Start proxy automatically**.
3. Click **Start**.
4. Use **Authenticate** for a provider to open a terminal and run login.

## Project layout

```
.
├── public/
│   ├── index.html
│   ├── styles.css
│   └── app.js
├── src-tauri/
│   ├── src/main.rs
│   ├── Cargo.toml
│   └── tauri.conf.json
└── package.json
```

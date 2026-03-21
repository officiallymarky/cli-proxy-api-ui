#!/usr/bin/env node

import { execSync, spawn } from "node:child_process";
import { existsSync, chmodSync, mkdirSync, createWriteStream } from "node:fs";
import { resolve, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { arch, platform } from "node:os";
import { get } from "node:https";

const __dirname = dirname(fileURLToPath(import.meta.url));
const pkgRoot = resolve(__dirname, "..");

// ── Config ───────────────────────────────────────────────────────────────────
// Set this after creating a GitHub repo and publishing a release.
const GITHUB_OWNER = "OWNER"; // TODO: replace with your GitHub username/org
const GITHUB_REPO = "cli-proxy-api-ui";
const RELEASE_TAG = "v0.1.0";
// ─────────────────────────────────────────────────────────────────────────────

function getPlatformKey() {
  const p = platform();
  const a = arch();
  if (p === "linux") return a === "arm64" ? "linux-arm64" : "linux-x64";
  if (p === "darwin") return a === "arm64" ? "macos-arm64" : "macos-x64";
  if (p === "win32") return "windows-x64";
  throw new Error(`Unsupported platform: ${p} ${a}`);
}

function binaryName() {
  return platform() === "win32" ? "cli-proxy-api-ui.exe" : "cli-proxy-api-ui";
}

// ── Download ─────────────────────────────────────────────────────────────────

function downloadToFile(url, dest) {
  return new Promise((resolve, reject) => {
    get(url, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        return downloadToFile(res.headers.location, dest).then(resolve, reject);
      }
      if (res.statusCode !== 200) {
        return reject(new Error(`HTTP ${res.statusCode}: ${url}`));
      }
      const file = createWriteStream(dest);
      res.pipe(file);
      file.on("finish", () => file.close(resolve));
      file.on("error", reject);
    }).on("error", reject);
  });
}

async function downloadFromGitHub(destDir) {
  const platKey = getPlatformKey();
  const tag = RELEASE_TAG;
  const url = `https://github.com/${GITHUB_OWNER}/${GITHUB_REPO}/releases/download/${tag}/cli-proxy-api-ui-${platKey}`;

  const dest = join(destDir, binaryName());
  console.log(`Downloading ${platKey} binary...`);
  console.log(`  ${url}`);
  await downloadToFile(url, dest);
  chmodSync(dest, 0o755);
  return dest;
}

// ── Find binary ──────────────────────────────────────────────────────────────

function findBinary() {
  const name = binaryName();
  const candidates = [
    join(pkgRoot, "bin", name),                          // downloaded by postinstall
    join(pkgRoot, "src-tauri", "target", "release", name), // local dev build
  ];
  return candidates.find((p) => existsSync(p));
}

// ── Dev mode ─────────────────────────────────────────────────────────────────

function tryDevMode() {
  if (!existsSync(join(pkgRoot, "src-tauri", "Cargo.toml"))) return false;

  try {
    execSync("rustc --version", { stdio: "ignore" });
  } catch {
    return false;
  }

  console.log("Starting CLI Proxy API UI (dev mode)...\n");
  const cmd = (() => {
    try {
      execSync("cargo tauri --version", { stdio: "ignore" });
      return { bin: "cargo", args: ["tauri", "dev"] };
    } catch {
      return { bin: "npx", args: ["tauri", "dev"] };
    }
  })();

  const child = spawn(cmd.bin, cmd.args, { cwd: pkgRoot, stdio: "inherit" });
  child.on("exit", (code) => process.exit(code ?? 1));
  return true;
}

// ── Main ─────────────────────────────────────────────────────────────────────

async function main() {
  // 1. Dev clone with Rust → tauri dev
  if (tryDevMode()) return;

  // 2. Prebuilt binary (local or downloaded)
  let binary = findBinary();

  if (!binary) {
    // Try downloading from GitHub Releases
    const binDir = join(pkgRoot, "bin");
    mkdirSync(binDir, { recursive: true });
    try {
      binary = await downloadFromGitHub(binDir);
    } catch (err) {
      console.error(
        [
          "Could not find or download a prebuilt binary.",
          "",
          `Error: ${err.message}`,
          "",
          "Options:",
          "  1. Install Rust (https://rustup.rs) and run from source",
          "  2. Download manually from GitHub Releases",
          "",
        ].join("\n")
      );
      process.exit(1);
    }
  }

  chmodSync(binary, 0o755);
  const child = spawn(binary, [], { stdio: "inherit" });
  child.on("exit", (code) => process.exit(code ?? 1));
}

main().catch((err) => {
  console.error(err.message);
  process.exit(1);
});

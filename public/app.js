const { invoke } = window.__TAURI__.core;

const serverBadge = document.getElementById("serverBadge");
const listenUrlEl = document.getElementById("listenUrl");
const binaryBadge = document.getElementById("binaryBadge");
const configBadge = document.getElementById("configBadge");
const providersEl = document.getElementById("providers");
const providerSummaryEl = document.getElementById("providerSummary");
const modelsEl = document.getElementById("models");
const refreshModelsBtn = document.getElementById("refreshModels");
const logsEl = document.getElementById("logs");
const exportLogsBtn = document.getElementById("exportLogs");
const providersModal = document.getElementById("providersModal");
const openProvidersBtn = document.getElementById("openProviders");
const closeProvidersBtn = document.getElementById("closeProviders");
const autostartToggle = document.getElementById("autostartToggle");

const startProxyAutomaticallyInput = document.getElementById("startProxyAutomatically");
const proxyToggle = document.getElementById("proxyToggle");

const vercelGatewayToggle = document.getElementById("vercelGatewayToggle");
const vercelGatewayApiKey = document.getElementById("vercelGatewayApiKey");
const gatewayFields = document.getElementById("gatewayFields");
const gatewayUrlRow = document.getElementById("gatewayUrlRow");
const gatewayUrlEl = document.getElementById("gatewayUrl");

const codexInstructionsToggle = document.getElementById("codexInstructionsToggle");
const commercialModeToggle = document.getElementById("commercialModeToggle");

const themeToggleBtn = document.getElementById("themeToggle");
const themeBulb = document.getElementById("themeBulb");

let currentSettings = null;
const THEME_KEY = "ui.theme";
let proxyActionPending = false;
let latestListenUrl = null;
let initialModelsRefreshTimer = null;

async function req(route, options = {}) {
  const method = options.method || "GET";
  const body = options.body ? JSON.parse(options.body) : null;

  try {
    if (method === "GET" && route === "/api/settings") {
      return await invoke("get_settings");
    }
    if (method === "POST" && route === "/api/settings") {
      return await invoke("save_settings", { settings: body });
    }
    if (method === "GET" && route === "/api/providers") {
      return await invoke("get_providers");
    }
    if (method === "GET" && route === "/api/logs") {
      return await invoke("get_logs");
    }
    if (method === "GET" && route === "/api/status") {
      return await invoke("get_status");
    }
    if (method === "POST" && route === "/api/server/start") {
      await invoke("server_start");
      return { ok: true };
    }
    if (method === "POST" && route === "/api/server/stop") {
      await invoke("server_stop");
      return { ok: true };
    }
    if (method === "POST" && route === "/api/server/restart") {
      await invoke("server_restart");
      return { ok: true };
    }
  } catch (error) {
    throw new Error(typeof error === "string" ? error : error?.message || "Command failed");
  }

  throw new Error(`Unsupported action: ${method} ${route}`);
}

function showNotice(text) {
  const old = document.querySelector(".toast");
  old?.remove();

  const toast = document.createElement("div");
  toast.className = "toast";
  toast.textContent = text;

  document.body.appendChild(toast);
  setTimeout(() => toast.remove(), 2400);
}

function normalizeTheme(mode) {
  return mode === "dark" ? "dark" : "light";
}

function preferredTheme() {
  return window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches
    ? "dark"
    : "light";
}

function applyTheme(mode) {
  const resolved = normalizeTheme(mode || preferredTheme());
  document.body.dataset.theme = resolved;
  localStorage.setItem(THEME_KEY, resolved);
  if (themeBulb) {
    themeBulb.checked = resolved === "light";
  }
  if (themeToggleBtn) {
    const label = resolved.charAt(0).toUpperCase() + resolved.slice(1);
    themeToggleBtn.title = `Theme: ${label}`;
    themeToggleBtn.setAttribute("aria-label", `Theme: ${label}`);
  }
}

function applySettingsForm(settings) {
  currentSettings = settings;
  startProxyAutomaticallyInput.checked = Boolean(settings.startProxyAutomatically);

  if (vercelGatewayToggle) {
    vercelGatewayToggle.checked = Boolean(settings.vercelGatewayEnabled);
  }
  if (vercelGatewayApiKey) {
    vercelGatewayApiKey.value = settings.vercelGatewayApiKey || "";
  }
  if (codexInstructionsToggle) {
    codexInstructionsToggle.checked = Boolean(settings.codexInstructionsEnabled);
  }
  if (commercialModeToggle) {
    commercialModeToggle.checked = Boolean(settings.commercialMode);
  }
}

const PROVIDER_STYLES = {
  codex:  { label: "CX", color: "#10a37f", bg: "rgba(16,163,127,0.12)" },
  claude: { label: "CL", color: "#d4a574", bg: "rgba(212,165,116,0.12)" },
  gemini: { label: "GM", color: "#4285f4", bg: "rgba(66,133,244,0.12)" },
  qwen:   { label: "QW", color: "#ff6a00", bg: "rgba(255,106,0,0.12)" },
};

function getProviderStyle(id) {
  return PROVIDER_STYLES[id] || { label: id.slice(0, 2).toUpperCase(), color: "var(--text-3)", bg: "var(--panel-inset)" };
}

function updateProviderSummaries(providers = []) {
  if (!providerSummaryEl) return;
  if (providers.length === 0) {
    providerSummaryEl.innerHTML = '<span class="prov-dot-none">None</span>';
  } else {
    providerSummaryEl.innerHTML = providers.map((p) => {
      const s = getProviderStyle(p.id);
      return `<span class="prov-badge ${p.connected ? 'on' : 'off'}" title="${p.name}" style="--pc:${s.color};--pb:${s.bg}">${s.label}</span>`;
    }).join("");
  }
}

function renderProviders(providers = []) {
  providersEl.innerHTML = "";

  updateProviderSummaries(providers);

  for (const provider of providers) {
    const item = document.createElement("div");
    item.className = "provider";

    const name = document.createElement("span");
    name.className = "provider-name";
    name.textContent = provider.name;

    const toggleLabel = document.createElement("label");
    toggleLabel.className = "toggle-label provider-toggle";
    const toggle = document.createElement("input");
    toggle.type = "checkbox";
    toggle.checked = provider.enabled;
    toggle.addEventListener("change", async () => {
      if (!currentSettings) return;
      const updated = currentSettings.providers.map(p =>
        p.id === provider.id ? { ...p, enabled: toggle.checked } : p
      );
      try {
        const saved = await req("/api/settings", { method: "POST", body: JSON.stringify({ ...currentSettings, providers: updated }) });
        applySettingsForm(saved);
        showNotice(`${provider.name} ${toggle.checked ? "enabled" : "disabled"}`);
      } catch (err) {
        showNotice(err.message);
        toggle.checked = !toggle.checked;
      }
    });
    toggleLabel.append(toggle);
    const pill = document.createElement("span");
    pill.className = "skeuo-pill";
    pill.innerHTML = '<span class="skeuo-track"></span><i class="skeuo-knob"></i><span class="skeuo-off">OFF</span><span class="skeuo-on">ON</span>';
    toggleLabel.append(pill);

    const status = document.createElement("span");
    status.textContent = provider.connected ? "Connected" : "Not detected";
    status.className = provider.connected ? "provider-status good" : "provider-status warn";

    // Reasoning level select — levels depend on provider
    const reasoningSelect = document.createElement("select");
    reasoningSelect.className = "reasoning-select";
    let reasoningOptions;
    if (provider.id === "codex") {
      reasoningOptions = [
        { v: "", l: "None" },
        { v: "minimal", l: "Minimal" },
        { v: "low", l: "Low" },
        { v: "medium", l: "Medium" },
        { v: "high", l: "High" },
        { v: "xhigh", l: "X-High" },
      ];
    } else if (provider.id === "claude") {
      reasoningOptions = [
        { v: "", l: "None" },
        { v: "low", l: "Low" },
        { v: "medium", l: "Medium" },
        { v: "high", l: "High" },
        { v: "xhigh", l: "X-High" },
        { v: "max", l: "Max" },
      ];
    } else if (provider.id === "gemini") {
      reasoningOptions = [
        { v: "", l: "None" },
        { v: "low", l: "Low" },
        { v: "medium", l: "Medium" },
        { v: "high", l: "High" },
      ];
    } else {
      // qwen, others
      reasoningOptions = [
        { v: "", l: "None" },
        { v: "minimal", l: "Minimal" },
        { v: "low", l: "Low" },
        { v: "medium", l: "Medium" },
        { v: "high", l: "High" },
      ];
    }
    for (const opt of reasoningOptions) {
      const el = document.createElement("option");
      el.value = opt.v;
      el.textContent = opt.l;
      if (provider.reasoningEffort === opt.v) el.selected = true;
      reasoningSelect.appendChild(el);
    }
    reasoningSelect.addEventListener("change", async () => {
      if (!currentSettings) return;
      const updated = currentSettings.providers.map(p =>
        p.id === provider.id ? { ...p, reasoningEffort: reasoningSelect.value } : p
      );
      try {
        const saved = await req("/api/settings", { method: "POST", body: JSON.stringify({ ...currentSettings, providers: updated }) });
        applySettingsForm(saved);
        if (reasoningSelect.value) {
          showNotice(`${provider.name} reasoning: ${reasoningSelect.value}`);
        } else {
          showNotice(`${provider.name} reasoning: none`);
        }
        // If the modal is open, refresh the table so selects match server state
        if (!providersModal.hidden) {
          const { providers: fresh } = await req("/api/providers");
          renderProviders(fresh);
        }
      } catch (err) {
        showNotice(err.message);
        reasoningSelect.value = provider.reasoningEffort || "";
      }
    });

    const button = document.createElement("button");
    button.className = "btn ghost";
    button.textContent = provider.authAvailable ? "Authenticate" : "Unavailable";
    button.disabled = !provider.authAvailable;
    button.addEventListener("click", async () => {
      try {
        await invoke("run_provider_auth", { providerId: provider.id });
        showNotice(`Opened terminal for ${provider.name}`);
      } catch (error) {
        showNotice(typeof error === "string" ? error : error?.message || "Failed to open terminal");
      }
    });

    item.append(name, toggleLabel, status, reasoningSelect, button);
    providersEl.appendChild(item);
  }
}

async function refreshProviders() {
  const { providers } = await req("/api/providers");
  // Only update summary badges during polling so the modal table isn't
  // destroyed mid-interaction. Full table rebuilds on modal open and after saves.
  updateProviderSummaries(providers);
}

function renderModels(models) {
  modelsEl.innerHTML = "";
  if (models.length === 0) {
    modelsEl.innerHTML = '<span class="models-empty">No models</span>';
    return;
  }
  for (const model of models) {
    const tag = document.createElement("span");
    tag.className = "model-tag";
    tag.textContent = model.id;
    modelsEl.appendChild(tag);
  }
}

async function refreshModels(listenUrl) {
  if (initialModelsRefreshTimer) {
    clearTimeout(initialModelsRefreshTimer);
    initialModelsRefreshTimer = null;
  }
  if (!listenUrl) {
    modelsEl.innerHTML = '<span class="models-empty">No endpoint</span>';
    if (refreshModelsBtn) refreshModelsBtn.disabled = true;
    return;
  }
  if (refreshModelsBtn) refreshModelsBtn.disabled = true;
  try {
    modelsEl.innerHTML = '<span class="models-empty">Loading...</span>';
    const res = await fetch(`${listenUrl}/models`);
    if (!res.ok) throw new Error(res.statusText);
    const data = await res.json();
    const models = data.data || [];
    renderModels(models);
  } catch {
    modelsEl.innerHTML = '<span class="models-empty">Unavailable</span>';
  } finally {
    if (refreshModelsBtn) refreshModelsBtn.disabled = false;
  }
}

function scheduleInitialModelsRefresh(listenUrl) {
  if (initialModelsRefreshTimer) {
    clearTimeout(initialModelsRefreshTimer);
    initialModelsRefreshTimer = null;
  }
  if (!listenUrl) {
    refreshModels(null).catch((err) => showNotice(err.message));
    return;
  }
  modelsEl.innerHTML = '<span class="models-empty">Loading...</span>';
  if (refreshModelsBtn) refreshModelsBtn.disabled = true;
  initialModelsRefreshTimer = setTimeout(() => {
    initialModelsRefreshTimer = null;
    if (latestListenUrl === listenUrl) {
      refreshModels(listenUrl).catch((err) => showNotice(err.message));
    }
  }, 750);
}

async function refreshLogs() {
  const { logs } = await req("/api/logs");
  logsEl.textContent = logs
    .slice(-80)
    .map((entry) => `[${new Date(entry.ts).toLocaleTimeString()}] ${entry.source}: ${entry.line}`)
    .join("\n");
  logsEl.scrollTop = logsEl.scrollHeight;
}

async function refreshSettings() {
  const settings = await req("/api/settings");
  applySettingsForm(settings);
}

async function saveSettings() {
  if (!currentSettings) return;
  const next = {
    ...currentSettings,
    startProxyAutomatically: startProxyAutomaticallyInput.checked
  };
  const saved = await req("/api/settings", { method: "POST", body: JSON.stringify(next) });
  applySettingsForm(saved);
  showNotice("Settings saved");
}

async function runServerAction(action) {
  proxyActionPending = true;
  if (proxyToggle) proxyToggle.disabled = true;
  try {
    await req(`/api/server/${action}`, { method: "POST" });
    await refreshStatus();
    await refreshLogs();
  } finally {
    proxyActionPending = false;
  }
}

proxyToggle.addEventListener("change", () => {
  const action = proxyToggle.checked ? "start" : "stop";
  runServerAction(action)
    .then(() => showNotice(`Proxy ${action}ed`))
    .catch((err) => {
      showNotice(err.message);
    })
    .finally(() => {
      proxyActionPending = false;
      refreshStatus().catch(() => {});
    });
});

startProxyAutomaticallyInput.addEventListener("change", () => {
  saveSettings()
    .then(() => showNotice("Auto-start setting updated"))
    .catch((err) => showNotice(err.message));
});

vercelGatewayToggle?.addEventListener("change", async () => {
  await saveGatewaySettings();
});

vercelGatewayApiKey?.addEventListener("change", () => saveGatewaySettings());

codexInstructionsToggle?.addEventListener("change", async () => {
  if (!currentSettings) return;
  const next = {
    ...currentSettings,
    codexInstructionsEnabled: codexInstructionsToggle.checked,
  };
  try {
    const saved = await req("/api/settings", { method: "POST", body: JSON.stringify(next) });
    applySettingsForm(saved);
    showNotice(`Codex instructions ${codexInstructionsToggle.checked ? "enabled" : "disabled"}`);
  } catch (err) {
    showNotice(err.message);
    codexInstructionsToggle.checked = !codexInstructionsToggle.checked;
  }
});

commercialModeToggle?.addEventListener("change", async () => {
  if (!currentSettings) return;
  const next = {
    ...currentSettings,
    commercialMode: commercialModeToggle.checked,
  };
  try {
    const saved = await req("/api/settings", { method: "POST", body: JSON.stringify(next) });
    applySettingsForm(saved);
    showNotice(`Commercial mode ${commercialModeToggle.checked ? "enabled" : "disabled"}`);
  } catch (err) {
    showNotice(err.message);
    commercialModeToggle.checked = !commercialModeToggle.checked;
  }
});

async function saveGatewaySettings() {
  if (!currentSettings) return;
  const enabled = vercelGatewayToggle.checked;
  const next = {
    ...currentSettings,
    vercelGatewayEnabled: enabled,
    vercelGatewayApiKey: vercelGatewayApiKey.value.trim(),
  };
  try {
    const saved = await req("/api/settings", { method: "POST", body: JSON.stringify(next) });
    applySettingsForm(saved);
    if (gatewayUrlRow && gatewayUrlEl) {
      if (enabled) {
        gatewayUrlRow.hidden = false;
        gatewayUrlEl.textContent = "https://ai-gateway.vercel.sh";
        gatewayUrlEl.className = "status-value mono good";
      } else {
        gatewayUrlRow.hidden = false;
        gatewayUrlEl.textContent = "Disabled";
        gatewayUrlEl.className = "status-value mono warn";
      }
    }
    showNotice("Gateway settings saved");
  } catch (err) {
    showNotice(err.message);
  }
}

themeBulb?.addEventListener("change", () => {
  const mode = themeBulb.checked ? "light" : "dark";
  applyTheme(mode);
  showNotice(`${mode.charAt(0).toUpperCase() + mode.slice(1)} mode`);
});

document.querySelectorAll(".section-toggle").forEach((el) => {
  el.addEventListener("click", () => {
    const target = document.getElementById(el.dataset.target);
    if (!target) return;
    const collapsed = target.classList.toggle("collapsed");
    el.classList.toggle("collapsed", collapsed);
  });
});

openProvidersBtn.addEventListener("click", async () => {
  providersModal.hidden = false;
  // Rebuild the full provider table when opening the modal, so interactive
  // controls (toggles, selects) are fresh and match current settings.
  try {
    const { providers } = await req("/api/providers");
    renderProviders(providers);
  } catch (err) {
    showNotice(err.message);
  }
});

closeProvidersBtn.addEventListener("click", () => {
  providersModal.hidden = true;
});

providersModal.addEventListener("click", (e) => {
  if (e.target === providersModal) providersModal.hidden = true;
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !providersModal.hidden) {
    providersModal.hidden = true;
  }
});

async function checkAutostart() {
  if (!autostartToggle) return;
  try {
    const enabled = await invoke("plugin:autostart|is_enabled");
    autostartToggle.checked = enabled;
  } catch {
    autostartToggle.disabled = true;
  }
}

autostartToggle?.addEventListener("change", async () => {
  try {
    if (autostartToggle.checked) {
      await invoke("plugin:autostart|enable");
      showNotice("System autostart enabled");
    } else {
      await invoke("plugin:autostart|disable");
      showNotice("System autostart disabled");
    }
  } catch (err) {
    showNotice(typeof err === "string" ? err : err?.message || "Failed to update autostart");
    autostartToggle.checked = !autostartToggle.checked;
  }
});

exportLogsBtn.addEventListener("click", () => {
  const text = logsEl.textContent || "";
  if (!text) return;
  const blob = new Blob([text], { type: "text/plain" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `cli-proxy-logs-${new Date().toISOString().slice(0, 19).replace(/:/g, "-")}.txt`;
  a.click();
  URL.revokeObjectURL(url);
});

refreshModelsBtn?.addEventListener("click", (event) => {
  event.stopPropagation();
  refreshModels(latestListenUrl).catch((err) => showNotice(err.message));
});

async function refreshStatus() {
  const status = await req("/api/status");
  const nextListenUrl = status.running ? status.listenUrl : null;
  const connectionChanged = nextListenUrl !== latestListenUrl;
  latestListenUrl = nextListenUrl;
  serverBadge.textContent = status.running && status.pid ? `Running (PID ${status.pid})` : status.running ? "Running" : "Stopped";
  serverBadge.className = `status-value ${status.running ? "good" : "warn"}`;

  if (listenUrlEl) {
    if (status.listenUrl) {
      listenUrlEl.textContent = status.listenUrl;
      listenUrlEl.className = `status-value mono ${status.running ? "good" : "warn"}`;
    } else {
      listenUrlEl.textContent = "—";
      listenUrlEl.className = "status-value mono";
    }
  }

  if (gatewayUrlRow && gatewayUrlEl) {
    if (status.gatewayUrl) {
      gatewayUrlRow.hidden = false;
      gatewayUrlEl.textContent = status.gatewayEnabled ? status.gatewayUrl : "Disabled";
      gatewayUrlEl.className = `status-value mono ${status.gatewayEnabled ? "good" : "warn"}`;
    } else {
      gatewayUrlRow.hidden = true;
    }
  }

  if (binaryBadge) {
    binaryBadge.textContent = status.binaryAvailable ? "Available" : "Missing";
    binaryBadge.className = `status-value ${status.binaryAvailable ? "good" : "warn"}`;
  }

  if (configBadge) {
    configBadge.textContent = status.configValid ? "Valid" : "Invalid";
    configBadge.className = `status-value ${status.configValid ? "good" : "warn"}`;
  }

  if (proxyToggle) {
    proxyToggle.checked = Boolean(status.running);
    proxyToggle.disabled = !status.binaryAvailable || proxyActionPending;
  }

  if (refreshModelsBtn) refreshModelsBtn.disabled = !latestListenUrl;
  if (connectionChanged || !modelsEl.innerHTML) {
    scheduleInitialModelsRefresh(latestListenUrl);
  }
}

async function refreshAll() {
  await Promise.all([refreshStatus(), refreshProviders(), refreshLogs()]);
}

async function boot() {
  applyTheme(localStorage.getItem(THEME_KEY) || preferredTheme());
  await refreshSettings();
  await checkAutostart();
  await refreshAll();
  setInterval(() => refreshAll().catch(() => {}), 3500);
}

boot().catch((err) => {
  showNotice(err.message);
});

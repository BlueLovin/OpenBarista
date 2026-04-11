const settingsForm = document.getElementById("deviceSettingsForm");
const saveSettingsBtn = document.getElementById("saveSettingsBtn");
const saveWifiBtn = document.getElementById("saveWifiBtn");
const settingsStatusEl = document.getElementById("settingsStatus");
const networkStatusEl = document.getElementById("networkStatus");
const refreshNetworksBtn = document.getElementById("refreshNetworksBtn");
const deviceLabelInput = document.getElementById("deviceLabelInput");
const ssidSelect = document.getElementById("ssidSelect");
const passwordInput = document.getElementById("passwordInput");
const temperatureOffsetInput = document.getElementById(
  "temperatureOffsetInput",
);
const buildIdEl = document.getElementById("buildId");
const boardIdEl = document.getElementById("boardId");
const directIpEl = document.getElementById("directIp");

const scanScalesBtn = document.getElementById("scanScalesBtn");
const scaleStatusEl = document.getElementById("scaleStatus");
const scaleCurrentCardEl = document.getElementById("scaleCurrentCard");
const scaleDeviceListEl = document.getElementById("scaleDeviceList");

let currentSavedSsid = "";
let scalePollHandle = null;
let scaleActionPending = false;

function setStatus(text, isError = false) {
  if (!settingsStatusEl) return;
  settingsStatusEl.textContent = text;
  settingsStatusEl.style.color = isError ? "#ffb4a2" : "#d8e7ff";
}

function setNetworkStatus(text, isError = false) {
  if (!networkStatusEl) return;
  networkStatusEl.textContent = text;
  networkStatusEl.style.color = isError ? "#ffb4a2" : "#9fb0cd";
}

function setScaleStatus(text, isError = false) {
  if (!scaleStatusEl) return;
  scaleStatusEl.textContent = text;
  scaleStatusEl.style.color = isError ? "#ffb4a2" : "#d8e7ff";
}

function renderSsidOptions(ssids) {
  if (!ssidSelect) return;

  const selectedBefore = ssidSelect.value;
  const unique = Array.from(
    new Set(
      ssids.filter((name) => typeof name === "string" && name.length > 0),
    ),
  ).sort((a, b) => a.localeCompare(b));

  ssidSelect.innerHTML = "";

  const keepCurrentOption = document.createElement("option");
  keepCurrentOption.value = "";
  keepCurrentOption.textContent = currentSavedSsid
    ? `Keep current (${currentSavedSsid})`
    : "Keep current network";
  ssidSelect.appendChild(keepCurrentOption);

  for (const ssid of unique) {
    const opt = document.createElement("option");
    opt.value = ssid;
    opt.textContent = ssid;
    ssidSelect.appendChild(opt);
  }

  if (selectedBefore && unique.includes(selectedBefore)) {
    ssidSelect.value = selectedBefore;
  } else {
    ssidSelect.value = "";
  }
}

async function loadNetworks() {
  if (refreshNetworksBtn) refreshNetworksBtn.disabled = true;

  try {
    const resp = await fetch("/networks", { cache: "no-store" });
    if (!resp.ok) {
      throw new Error("network scan unavailable");
    }

    const payload = await resp.json();
    const list = Array.isArray(payload) ? payload : [];
    const merged = currentSavedSsid ? [currentSavedSsid, ...list] : list;
    renderSsidOptions(merged);
    setNetworkStatus(
      list.length > 0
        ? "Select a network from the list or keep current."
        : "No nearby networks found. Keeping current network is available.",
    );
  } catch (_err) {
    renderSsidOptions(currentSavedSsid ? [currentSavedSsid] : []);
    setNetworkStatus("Live scan is unavailable on this page right now.");
  } finally {
    if (refreshNetworksBtn) refreshNetworksBtn.disabled = false;
  }
}

async function loadSettings() {
  try {
    const resp = await fetch("/api/settings", { cache: "no-store" });
    if (!resp.ok) {
      throw new Error("settings endpoint failed");
    }

    const data = await resp.json();

    if (deviceLabelInput && typeof data.device_label === "string") {
      deviceLabelInput.value = data.device_label;
    }
    if (typeof data.ssid === "string") {
      currentSavedSsid = data.ssid;
    }
    if (
      temperatureOffsetInput &&
      typeof data.temperature_offset_c === "number" &&
      Number.isFinite(data.temperature_offset_c)
    ) {
      temperatureOffsetInput.value = data.temperature_offset_c.toFixed(1);
    }

    renderSsidOptions(currentSavedSsid ? [currentSavedSsid] : []);

    if (buildIdEl && typeof data.build_id === "string") {
      buildIdEl.textContent = data.build_id;
    }
    if (boardIdEl && typeof data.board_id === "string") {
      boardIdEl.textContent = data.board_id;
    }
    if (directIpEl && typeof data.ip_addr === "string") {
      directIpEl.textContent = data.ip_addr;
      if (
        directIpEl.parentElement &&
        directIpEl.parentElement.tagName === "A"
      ) {
        directIpEl.parentElement.href = `http://${data.ip_addr}`;
      }
    }

    setStatus("Settings loaded.");
    setNetworkStatus("Click Refresh to scan nearby Wi-Fi networks.");
  } catch (_err) {
    setStatus("Could not load settings right now.", true);
    renderSsidOptions([]);
    setNetworkStatus("Could not load networks.", true);
  }
}

async function saveSettings(ev) {
  ev.preventDefault();
  if (!saveSettingsBtn || !saveWifiBtn) return;

  const submitter = ev.submitter;
  const saveMode = submitter?.dataset?.saveMode === "wifi" ? "wifi" : "device";
  const updatingWifi = saveMode === "wifi";

  const body = new URLSearchParams();
  body.set(
    "device_label",
    deviceLabelInput ? deviceLabelInput.value.trim() : "",
  );
  body.set("wifi_update", updatingWifi ? "1" : "0");
  body.set("ssid", updatingWifi && ssidSelect ? ssidSelect.value : "");
  body.set(
    "password",
    updatingWifi && passwordInput ? passwordInput.value : "",
  );
  body.set(
    "temperature_offset_c",
    temperatureOffsetInput ? temperatureOffsetInput.value.trim() || "0" : "0",
  );

  saveSettingsBtn.disabled = true;
  saveWifiBtn.disabled = true;
  setStatus(
    updatingWifi ? "Applying Wi-Fi changes..." : "Saving device settings...",
  );

  try {
    const resp = await fetch("/api/settings", {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: body.toString(),
    });

    const payload = await resp.json();
    if (!resp.ok || !payload.ok) {
      throw new Error(payload.message || "save failed");
    }

    if (passwordInput) {
      passwordInput.value = "";
    }

    if (payload.rebooting) {
      setStatus("Saved. Rebooting now to apply Wi-Fi settings...");
    } else {
      setStatus(
        updatingWifi ? "Wi-Fi settings saved." : "Device settings saved.",
      );
    }
  } catch (err) {
    setStatus(`Save failed: ${err.message || "unknown error"}`, true);
  } finally {
    saveSettingsBtn.disabled = false;
    saveWifiBtn.disabled = false;
  }
}

function buildScaleActionButton(label, action, address = "", disabled = false) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = action === "connect" ? "btn-secondary" : "btn-chip";
  button.textContent = label;
  button.dataset.scaleAction = action;
  button.disabled = disabled;
  if (address) {
    button.dataset.scaleAddress = address;
  }
  return button;
}

function scaleConnectBusy(data) {
  return (
    Boolean(data) &&
    (scaleActionPending ||
      data.state === "connecting" ||
      data.state === "discovering")
  );
}

function formatScaleMetric(value, unit = "", digits = 1) {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return "--";
  }

  return unit ? `${value.toFixed(digits)} ${unit}` : value.toFixed(digits);
}

function buildScaleMetric(label, value) {
  const card = document.createElement("div");
  card.className = "scale-live-metric";

  const metricLabel = document.createElement("span");
  metricLabel.className = "scale-live-label";
  metricLabel.textContent = label;

  const metricValue = document.createElement("strong");
  metricValue.className = "scale-live-value";
  metricValue.textContent = value;

  card.append(metricLabel, metricValue);
  return card;
}

function renderScaleCurrent(data) {
  if (!scaleCurrentCardEl) return;
  scaleCurrentCardEl.innerHTML = "";

  const connected = Boolean(data.connected_name) && data.state === "ready";
  const saved = data.saved_scale || null;
  const connectBusy = scaleConnectBusy(data);

  if (!connected && !saved) {
    scaleCurrentCardEl.className = "scale-current scale-current-empty";
    const title = document.createElement("strong");
    title.textContent = "No scale saved yet";
    const copy = document.createElement("p");
    copy.textContent = "Use Find Scales and tap your scale to save it.";
    scaleCurrentCardEl.append(title, copy);
    return;
  }

  scaleCurrentCardEl.className = "scale-current";

  const head = document.createElement("div");
  head.className = "scale-card-head";

  const titleWrap = document.createElement("div");
  const title = document.createElement("strong");
  title.textContent = connected ? data.connected_name : saved.name;
  const meta = document.createElement("p");
  meta.className = "scale-card-copy";
  meta.textContent = connected
    ? `${data.connected_address} • ${data.protocol || "connected"}`
    : `${saved.address} • saved for quick reconnect`;
  titleWrap.append(title, meta);

  const badge = document.createElement("span");
  if (connected) {
    badge.className = "scale-badge scale-badge-live";
    badge.textContent = "Connected";
  } else if (connectBusy) {
    badge.className = "scale-badge scale-badge-busy";
    badge.textContent = "Connecting...";
  } else {
    badge.className = "scale-badge";
    badge.textContent = "Saved";
  }

  head.append(titleWrap, badge);
  scaleCurrentCardEl.appendChild(head);

  if (connected) {
    const liveGrid = document.createElement("div");
    liveGrid.className = "scale-live-grid";
    liveGrid.append(
      buildScaleMetric("Weight", formatScaleMetric(data.weight_g, "g", 1)),
      buildScaleMetric("Flow", formatScaleMetric(data.flow_gps, "g/s", 1)),
      buildScaleMetric(
        "Battery",
        data.battery_percent == null ? "--" : `${data.battery_percent}%`,
      ),
    );
    scaleCurrentCardEl.appendChild(liveGrid);
  }

  const actionRow = document.createElement("div");
  actionRow.className = "scale-actions";
  if (connected) {
    actionRow.appendChild(buildScaleActionButton("Disconnect", "disconnect"));
  } else if (connectBusy) {
    // Show a cancel/disconnect button while connecting so the user isn't stuck.
    actionRow.appendChild(buildScaleActionButton("Cancel", "disconnect"));
  } else if (saved) {
    actionRow.appendChild(
      buildScaleActionButton("Connect Saved Scale", "connect", saved.address),
    );
  }
  if (saved) {
    // Forget is always available — never disabled during connect.
    actionRow.appendChild(buildScaleActionButton("Forget", "forget"));
  }
  scaleCurrentCardEl.appendChild(actionRow);
}

function renderScaleDevices(data) {
  if (!scaleDeviceListEl) return;
  scaleDeviceListEl.innerHTML = "";

  const devices = Array.isArray(data.devices) ? data.devices : [];
  const connectBusy = scaleConnectBusy(data);
  if (!devices.length) {
    const empty = document.createElement("p");
    empty.className = "scale-empty";
    empty.textContent =
      data.state === "scanning"
        ? "Scanning nearby devices..."
        : "No scale candidates yet. Tap Find Scales and wake your scale first.";
    scaleDeviceListEl.appendChild(empty);
    return;
  }

  for (const device of devices) {
    const card = document.createElement("article");
    card.className = "scale-device";

    const body = document.createElement("div");
    body.className = "scale-device-body";

    const name = document.createElement("strong");
    name.textContent = device.name || device.address;
    const meta = document.createElement("p");
    meta.className = "scale-card-copy";
    meta.textContent = `${device.address} • ${device.rssi} dBm`;
    body.append(name, meta);

    const side = document.createElement("div");
    side.className = "scale-device-side";
    if (device.saved) {
      const pill = document.createElement("span");
      pill.className = "scale-badge";
      pill.textContent = "Saved";
      side.appendChild(pill);
    }
    const isThisDevice =
      connectBusy &&
      data.connected_address &&
      data.connected_address.toLowerCase() === device.address.toLowerCase();
    side.appendChild(
      buildScaleActionButton(
        isThisDevice ? "Connecting..." : "Connect",
        "connect",
        device.address,
        connectBusy,
      ),
    );

    card.append(body, side);
    scaleDeviceListEl.appendChild(card);
  }
}

function renderScaleState(data) {
  renderScaleCurrent(data);
  renderScaleDevices(data);

  if (scanScalesBtn) {
    // Only disable the scan button when a POST is in-flight or we're
    // already connected. During connecting/discovering, the user should
    // still be able to start a new scan (which cancels the connect).
    scanScalesBtn.disabled = scaleActionPending || data.state === "ready";
  }

  const liveSummary = Boolean(data.connected_name)
    ? ` Live: ${formatScaleMetric(data.weight_g, "g", 1)} at ${formatScaleMetric(data.flow_gps, "g/s", 1)}.`
    : "";

  setScaleStatus(
    `${data.message || "Scale status updated."}${liveSummary}`,
    data.state === "error",
  );
}

async function loadScaleStatus() {
  try {
    const resp = await fetch("/api/scale", { cache: "no-store" });
    if (!resp.ok) {
      throw new Error("scale status endpoint failed");
    }

    const data = await resp.json();
    renderScaleState(data);
  } catch (_err) {
    setScaleStatus("Could not load Bluetooth scale status.", true);
  }
}

async function sendScaleAction(action, address = "") {
  scaleActionPending = true;
  if (scanScalesBtn) scanScalesBtn.disabled = true;
  document.querySelectorAll("button[data-scale-action]").forEach((button) => {
    button.disabled = true;
  });

  const busyMessage =
    {
      scan: "Finding nearby scales...",
      connect: "Connecting to the selected scale...",
      disconnect: "Disconnecting current scale...",
      forget: "Removing saved scale...",
    }[action] || "Updating scale...";

  setScaleStatus(busyMessage);

  const body = new URLSearchParams();
  body.set("action", action);
  if (address) {
    body.set("address", address);
  }

  try {
    const resp = await fetch("/api/scale", {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: body.toString(),
    });
    const payload = await resp.json();
    if (!resp.ok || !payload.ok) {
      throw new Error(payload.message || "scale update failed");
    }

    setScaleStatus(payload.message || "Scale updated.");
    await loadScaleStatus();
  } catch (err) {
    setScaleStatus(
      `Scale action failed: ${err.message || "unknown error"}`,
      true,
    );
  } finally {
    scaleActionPending = false;
    await loadScaleStatus();
  }
}

function onScaleActionClick(ev) {
  const button = ev.target.closest("button[data-scale-action]");
  if (!button) return;

  const action = button.dataset.scaleAction;
  const address = button.dataset.scaleAddress || "";
  sendScaleAction(action, address);
}

if (settingsForm) {
  settingsForm.addEventListener("submit", saveSettings);
}

if (refreshNetworksBtn) {
  refreshNetworksBtn.addEventListener("click", loadNetworks);
}

if (scanScalesBtn) {
  scanScalesBtn.addEventListener("click", () => sendScaleAction("scan"));
}

if (scaleCurrentCardEl) {
  scaleCurrentCardEl.addEventListener("click", onScaleActionClick);
}

if (scaleDeviceListEl) {
  scaleDeviceListEl.addEventListener("click", onScaleActionClick);
}

document.addEventListener("DOMContentLoaded", () => {
  loadSettings();
  loadScaleStatus();

  if (scalePollHandle) {
    clearInterval(scalePollHandle);
  }
  scalePollHandle = setInterval(loadScaleStatus, 1000);
});

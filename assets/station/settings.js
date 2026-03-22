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

let currentSavedSsid = "";

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

if (settingsForm) {
  settingsForm.addEventListener("submit", saveSettings);
}

if (refreshNetworksBtn) {
  refreshNetworksBtn.addEventListener("click", loadNetworks);
}

document.addEventListener("DOMContentLoaded", () => {
  loadSettings();
});

const settingsForm = document.getElementById("deviceSettingsForm");
const saveSettingsBtn = document.getElementById("saveSettingsBtn");
const settingsStatusEl = document.getElementById("settingsStatus");
const deviceLabelInput = document.getElementById("deviceLabelInput");
const ssidInput = document.getElementById("ssidInput");
const passwordInput = document.getElementById("passwordInput");
const buildIdEl = document.getElementById("buildId");
const boardIdEl = document.getElementById("boardId");
const directIpEl = document.getElementById("directIp");

function setStatus(text, isError = false) {
  if (!settingsStatusEl) return;
  settingsStatusEl.textContent = text;
  settingsStatusEl.style.color = isError ? "#9a1f1f" : "#5e4a3b";
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
    if (ssidInput && typeof data.ssid === "string") {
      ssidInput.value = data.ssid;
    }
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
  } catch (_err) {
    setStatus("Could not load settings right now.", true);
  }
}

async function saveSettings(ev) {
  ev.preventDefault();
  if (!saveSettingsBtn) return;

  const body = new URLSearchParams();
  body.set(
    "device_label",
    deviceLabelInput ? deviceLabelInput.value.trim() : "",
  );
  body.set("ssid", ssidInput ? ssidInput.value.trim() : "");
  body.set("password", passwordInput ? passwordInput.value : "");

  saveSettingsBtn.disabled = true;
  setStatus("Saving settings...");

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
      setStatus("Saved successfully.");
    }
  } catch (err) {
    setStatus(`Save failed: ${err.message || "unknown error"}`, true);
  } finally {
    saveSettingsBtn.disabled = false;
  }
}

if (settingsForm) {
  settingsForm.addEventListener("submit", saveSettings);
}

document.addEventListener("DOMContentLoaded", () => {
  loadSettings();
});

const select = document.getElementById("networkSelect");
const ssidInput = document.getElementById("ssid");
const status = document.getElementById("netStatus");
const refreshBtn = document.getElementById("refreshBtn");
const statusBadge = document.getElementById("connectStageBadge");
const portalStatusText = document.getElementById("portalStatusText");
const statusAttempt = document.getElementById("statusAttempt");
const connectBtn = document.getElementById("connectBtn");

if (select && ssidInput) {
  select.addEventListener("change", () => {
    if (select.value) {
      ssidInput.value = select.value;
    }
  });
}

async function refreshNetworks() {
  if (!status || !select) {
    return;
  }

  status.textContent = "Refreshing list...";
  try {
    const resp = await fetch("/networks", { cache: "no-store" });
    const items = JSON.parse(await resp.text());
    select.innerHTML = "";

    if (!Array.isArray(items) || items.length === 0) {
      const opt = document.createElement("option");
      opt.value = "";
      opt.textContent = "No networks found";
      select.appendChild(opt);
      status.textContent =
        "No networks found. You can still type SSID manually.";
      return;
    }

    const placeholder = document.createElement("option");
    placeholder.value = "";
    placeholder.textContent = "Select a network";
    select.appendChild(placeholder);

    items.forEach((ssid) => {
      const opt = document.createElement("option");
      opt.value = ssid;
      opt.textContent = ssid;
      select.appendChild(opt);
    });

    status.textContent = `Found ${items.length} network(s).`;
  } catch (_err) {
    status.textContent =
      "Could not load networks right now. Enter SSID manually.";
  }
}

function renderConnectionStatus(data) {
  if (statusBadge && typeof data.stage === "string") {
    statusBadge.textContent = data.stage;
    statusBadge.classList.remove("badge-live", "badge-warn");
    if (data.stage === "connected") {
      statusBadge.classList.add("badge-live");
    } else if (data.stage === "failed" || data.stage === "rebooting") {
      statusBadge.classList.add("badge-warn");
    }
  }

  const attemptText =
    typeof data.attempt === "number" && typeof data.total === "number"
      ? `attempt ${data.attempt}/${data.total}`
      : "booting";

  if (statusAttempt) {
    statusAttempt.textContent = attemptText;
  }

  if (portalStatusText) {
    const message = typeof data.message === "string" ? data.message : "";
    portalStatusText.textContent = message || `Wi-Fi ${attemptText}`;
  }

  if (connectBtn && data.stage === "rebooting") {
    connectBtn.disabled = true;
  }
}

async function pollStatus() {
  try {
    const resp = await fetch("/status", { cache: "no-store" });
    if (!resp.ok) {
      return;
    }
    const payload = await resp.json();
    renderConnectionStatus(payload);
  } catch (_err) {
    // Keep UI usable even if status polling fails temporarily.
  }
}

if (refreshBtn) {
  refreshBtn.addEventListener("click", refreshNetworks);
}

refreshNetworks();
pollStatus();
setInterval(pollStatus, 1000);

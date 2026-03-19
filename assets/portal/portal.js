const select = document.getElementById("networkSelect");
const ssidInput = document.getElementById("ssid");
const status = document.getElementById("netStatus");
const refreshBtn = document.getElementById("refreshBtn");

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

if (refreshBtn) {
  refreshBtn.addEventListener("click", refreshNetworks);
}

refreshNetworks();

const POLL_MS = 250;
const MAX_IDLE_PTS = 720;
const IDLE_WINDOW_S = 60;

const P_MIN = 0;
const P_MAX = 12;
const FLOW_MIN = 0;
const FLOW_FALLBACK_MAX = 4;
const WEIGHT_MIN = 0;
const WEIGHT_FALLBACK_MAX = 40;

const PROFILES = {
  "Flat 9 bar": (_t) => 9.0,
  "Lever (9 -> 6 bar)": (t) => Math.max(6.0, 9.0 - 0.1 * Math.max(0, t - 5)),
  "Pre-infusion (0->4->9)": (t) => (t < 6 ? Math.min(4.0, 0.65 * t) : 9.0),
  "Blooming (4->9 bar)": (t) =>
    t < 10 ? 4.0 : Math.min(9.0, 4.0 + 0.5 * (t - 10)),
  "Temperature surf": (t) =>
    t < 3 ? 9.0 : Math.max(6.0, 9.0 - 0.06 * (t - 3)),
  None: (_t) => null,
};

let shotActive = false;
let shotStartMs = null;
let lastSeq = -1;
let idleOffsetS = 0;
let windowS = IDLE_WINDOW_S;
let latestScaleWeightG = 0;
let latestFlowGps = 0;
let scaleConnected = false;
let shotWeightZeroG = null;
// Two-phase firmware-sync flag.  startPostSucceeded is set when the
// action=start POST resolves; shotSyncedFromFirmware is only set once
// telemetry subsequently confirms recording_active=true.  This prevents
// a stale recording_active=false poll (arriving between the POST resolving
// and the firmware actually starting) from immediately clearing the UI.
let shotSyncedFromFirmware = false;
let startPostSucceeded = false;

let xs = [];
let pressures = [];
let flows = [];
let weights = [];
let targets = [];

let timerHandle = null;
let consecutiveFailures = 0;
const HW_FAIL_THRESHOLD = 5;
let plot = null;

const $ = (id) => document.getElementById(id);

const statusEl = $("telemetryStatus");
const tempEl = $("metricTemp");
const barEl = $("metricBar");
const weightEl = $("metricWeight");
const weightHintEl = $("metricWeightHint");
const psiEl = $("metricPsi");
const flowEl = $("metricFlow");
const peakBarEl = $("metricPeakBar");
const avgBarEl = $("metricAvgBar");
const timerEl = $("shotTimer");
const startBtn = $("startShotBtn");
const profileSel = $("profileSelect");
const windowSel = $("windowSelect");
const chartDiv = $("uplotChart");
const scaleSyncValueEl = $("scaleSyncValue");
const scaleSyncMetaEl = $("scaleSyncMeta");
const hwFailBanner = $("hwFailBanner");
const hwFailMsg = $("hwFailMsg");
const hwRetryBtn = $("hwRetryBtn");

function buildPlotOpts(width) {
  return {
    width,
    height: chartHeightFor(width),
    cursor: { show: true },
    legend: { live: true },
    scales: {
      x: { time: false },
      bar: { range: () => [P_MIN, P_MAX] },
      flow: {
        range: (_u, min, max) => [
          FLOW_MIN,
          Math.max(
            FLOW_FALLBACK_MAX,
            Math.ceil(Math.max(min ?? 0, max ?? 0) + 1),
          ),
        ],
      },
      weight: {
        range: (_u, min, max) => [
          WEIGHT_MIN,
          Math.max(
            WEIGHT_FALLBACK_MAX,
            Math.ceil(Math.max(min ?? 0, max ?? 0) + 2),
          ),
        ],
      },
    },
    axes: [
      {
        label: "t (s)",
        labelSize: 14,
        stroke: "#8192b5",
        grid: { stroke: "#1d273a", width: 1 },
        ticks: { stroke: "#2c3a56", width: 1 },
        values: (_u, ticks) => ticks.map((v) => v.toFixed(0) + "s"),
      },
      {
        scale: "bar",
        label: "bar",
        labelSize: 14,
        stroke: "#ff9c66",
        grid: { stroke: "#1d273a", width: 1 },
        ticks: { stroke: "#2c3a56", width: 1 },
        values: (_u, ticks) => ticks.map((v) => v.toFixed(1)),
      },
      {
        scale: "flow",
        side: 1,
        label: "g/s",
        labelSize: 14,
        stroke: "#65a2ff",
        grid: { show: false },
        ticks: { stroke: "#2c3a56", width: 1 },
        values: (_u, ticks) => ticks.map((v) => v.toFixed(1)),
      },
      {
        scale: "weight",
        side: 1,
        label: "g",
        labelSize: 14,
        stroke: "#74e39a",
        grid: { show: false },
        ticks: { stroke: "#2c3a56", width: 1 },
        values: (_u, ticks) => ticks.map((v) => v.toFixed(0)),
      },
    ],
    series: [
      {},
      {
        label: "Pressure",
        scale: "bar",
        stroke: "#ff9553",
        width: 2.5,
        fill: "rgba(255, 122, 47, 0.14)",
        points: { show: false },
      },
      {
        label: "Flow",
        scale: "flow",
        stroke: "#65a2ff",
        width: 2,
        fill: "rgba(101, 162, 255, 0.12)",
        points: { show: false },
      },
      {
        label: "Weight",
        scale: "weight",
        stroke: "#74e39a",
        width: 2,
        points: { show: false },
      },
      {
        label: "Target",
        scale: "bar",
        stroke: "#c4ff5d",
        width: 1.5,
        dash: [6, 4],
        points: { show: false },
      },
    ],
  };
}

function chartHeightFor(width) {
  if (width > 480) return 250;
  // On mobile, compute from available viewport space
  const vh = window.innerHeight || 667;
  // Reserve: topbar ~52, metrics ~68, foot ~50, psi/flow ~52, cta ~48, nav ~44, gaps ~48, padding ~60
  const reserved = 422;
  const avail = vh - reserved;
  return Math.max(120, Math.min(avail, 300));
}

function chartWidth() {
  return chartDiv ? chartDiv.clientWidth || chartDiv.offsetWidth || 220 : 220;
}

function initPlot() {
  if (!chartDiv || typeof uPlot === "undefined") return;
  const w = chartWidth();
  plot = new uPlot(buildPlotOpts(w), [[], [], [], [], []], chartDiv);
}

window.addEventListener("resize", () => {
  if (plot && chartDiv) {
    const width = chartWidth();
    plot.setSize({ width, height: chartHeightFor(width) });
  }
});

function currentProfileFn() {
  const key = profileSel ? profileSel.value : "Flat 9 bar";
  return PROFILES[key] ?? PROFILES.None;
}

function ensureUplotCss() {
  if (document.querySelector('link[data-uplot="1"]')) {
    return;
  }
  const css = document.createElement("link");
  css.rel = "stylesheet";
  css.href = "/uplot.min.css";
  css.dataset.uplot = "1";
  document.head.appendChild(css);
}

function loadUplotJs() {
  return new Promise((resolve, reject) => {
    if (typeof window.uPlot !== "undefined") {
      resolve();
      return;
    }

    const existing = document.querySelector('script[data-uplot="1"]');
    if (existing) {
      existing.addEventListener("load", () => resolve(), { once: true });
      existing.addEventListener(
        "error",
        () => reject(new Error("uPlot load failed")),
        {
          once: true,
        },
      );
      return;
    }

    const script = document.createElement("script");
    script.src = "/uplot.min.js";
    script.defer = true;
    script.dataset.uplot = "1";
    script.onload = () => resolve();
    script.onerror = () => reject(new Error("uPlot load failed"));
    document.head.appendChild(script);
  });
}

async function bootstrapChartAssets() {
  ensureUplotCss();
  try {
    await loadUplotJs();
    initPlot();
  } catch (_err) {
    if (statusEl) {
      statusEl.textContent = "Chart unavailable";
      statusEl.className = "badge";
    }
  }
}

function startTimer() {
  timerHandle = setInterval(() => {
    if (shotStartMs !== null && timerEl) {
      const formatted = formatTimer((performance.now() - shotStartMs) / 1000);
      timerEl.innerHTML = formatted + "<span>s</span>";
      const indicatorTimer = document.getElementById('shotTimerIndicator');
      if (indicatorTimer) indicatorTimer.textContent = formatted;
    }
  }, 50);
}

function stopTimer() {
  if (timerHandle) {
    clearInterval(timerHandle);
    timerHandle = null;
  }
}

function enterRecordingMode() {
  xs = [];
  pressures = [];
  flows = [];
  weights = [];
  targets = [];
  lastSeq = -1; // ensure first poll after shot start is never skipped
  shotStartMs = performance.now();
  shotActive = true;
  shotWeightZeroG = scaleConnected ? latestScaleWeightG : 0;
  windowS = 90;
  // Immediately clear the chart so idle data doesn't linger into the shot view.
  if (plot) {
    plot.setData([[], [], [], [], []]);
    plot.setScale("x", { min: 0, max: 30 });
  }
  const indicatorEl = document.getElementById('shotIndicator');
  if (indicatorEl) indicatorEl.hidden = false;
  if (startBtn) {
    startBtn.textContent = "STOP EXTRACTION";
    startBtn.dataset.active = "1";
  }
  if (timerEl) timerEl.innerHTML = "00:00<span>s</span>";
  if (peakBarEl) peakBarEl.textContent = "--";
  if (avgBarEl) avgBarEl.textContent = "--";
  startTimer();
}

function exitRecordingMode() {
  shotActive = false;
  shotStartMs = null;
  shotWeightZeroG = null;
  windowS = IDLE_WINDOW_S;
  stopTimer();
  const indicatorEl = document.getElementById('shotIndicator');
  if (indicatorEl) indicatorEl.hidden = true;
  if (startBtn) {
    startBtn.textContent = "START EXTRACTION";
    delete startBtn.dataset.active;
  }
}

function startShot() {
  startPostSucceeded = false;
  enterRecordingMode();
  // Tell the backend to start recording regardless of pressure.
  fetch('/api/shots', {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: 'action=start',
  })
    // Don't set shotSyncedFromFirmware here — the next telemetry poll may
    // still carry a stale recording_active=false snapshot and would
    // immediately clear the UI.  Instead, set startPostSucceeded and let
    // poll() arm shotSyncedFromFirmware once it sees recording_active=true.
    .then(function () { startPostSucceeded = true; })
    .catch(function () { /* ignore network errors */ });
}

function stopShot() {
  shotSyncedFromFirmware = false;
  startPostSucceeded = false;
  exitRecordingMode();
  // Save the shot server-side and show a toast with a link.
  fetch('/api/shots', {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: 'action=save',
  })
    .then(function (r) { return r.json(); })
    .then(function (d) {
      if (d.ok) {
        showToast('Shot saved! <a href="/history?id=' + d.id + '">View \u2192</a>');
      }
    })
    .catch(function () { /* ignore network errors */ });
}

function formatTimer(totalSeconds) {
  const sec = Math.max(0, Math.floor(totalSeconds));
  const mm = String(Math.floor(sec / 60)).padStart(2, "0");
  const ss = String(sec % 60).padStart(2, "0");
  return `${mm}:${ss}`;
}

function refreshStats() {
  const valid = pressures.filter((v) => v != null && !isNaN(v));
  if (!valid.length) return;
  const peak = Math.max(...valid);
  const avg = valid.reduce((a, b) => a + b, 0) / valid.length;
  if (peakBarEl) peakBarEl.textContent = peak.toFixed(2) + " bar";
  if (avgBarEl) avgBarEl.textContent = avg.toFixed(2) + " bar";
}

function refreshPlot() {
  if (!plot || !xs.length) return;

  const xNow = xs[xs.length - 1];
  let startIdx = 0;
  if (!shotActive) {
    const cutoff = xNow - windowS;
    while (startIdx < xs.length - 1 && xs[startIdx] < cutoff) startIdx++;
  }

  plot.setData([
    xs.slice(startIdx),
    pressures.slice(startIdx),
    flows.slice(startIdx),
    weights.slice(startIdx),
    targets.slice(startIdx),
  ]);

  const xMin = xs[startIdx];
  const xMax = shotActive ? Math.max(xNow + 1, 30) : xNow + 1;
  plot.setScale("x", { min: xMin, max: xMax });
}

function displayedWeightG() {
  if (!scaleConnected || !Number.isFinite(latestScaleWeightG)) {
    return null;
  }

  if (shotActive && shotWeightZeroG !== null) {
    return Math.max(0, latestScaleWeightG - shotWeightZeroG);
  }

  return Math.max(0, latestScaleWeightG);
}

function refreshScaleUi() {
  const weight = displayedWeightG();

  if (weightEl) {
    weightEl.textContent = weight === null ? "--" : weight.toFixed(1);
  }
  if (weightHintEl) {
    weightHintEl.textContent = shotActive
      ? "Zeroed at shot start"
      : scaleConnected
        ? "Live scale weight"
        : "Pair in Settings";
  }
  if (flowEl) {
    flowEl.textContent = scaleConnected
      ? `${latestFlowGps.toFixed(1)} g/s`
      : "--";
  }
  if (scaleSyncValueEl) {
    scaleSyncValueEl.textContent = scaleConnected ? "Connected" : "Not linked";
  }
  if (scaleSyncMetaEl) {
    scaleSyncMetaEl.textContent = scaleConnected
      ? "Weight and flow live"
      : "Open Settings to pair";
  }
}

async function poll() {
  try {
    const r = await fetch("/api/telemetry", { cache: "no-store" });
    const d = await r.json();

    consecutiveFailures = 0;
    if (hwFailBanner) hwFailBanner.hidden = true;

    if (d.seq === lastSeq) return;
    lastSeq = d.seq;

    // Sync recording indicator with firmware truth.
    if (d.recording_active && !shotActive) {
      // Firmware auto-detected a shot — enter recording mode without POSTing.
      shotSyncedFromFirmware = true;
      enterRecordingMode();
    } else if (d.recording_active && shotActive && startPostSucceeded && !shotSyncedFromFirmware) {
      // POST already succeeded AND telemetry now confirms the firmware is
      // recording — safe to arm firmware-end detection.  Doing this here
      // (rather than in the .then() callback) avoids a race where a stale
      // recording_active=false snapshot arrives before the firmware has
      // actually started and immediately clears the UI.
      shotSyncedFromFirmware = true;
    } else if (!d.recording_active && shotActive && shotSyncedFromFirmware) {
      // Firmware ended the shot (pressure drop / auto-finalize) — clear UI.
      shotSyncedFromFirmware = false;
      startPostSucceeded = false;
      exitRecordingMode();
      showToast('Shot saved! <a href="/history">View history \u2192</a>');
    }

    let t;
    if (shotActive) {
      t = (performance.now() - shotStartMs) / 1000;
    } else {
      idleOffsetS += POLL_MS / 1000;
      t = idleOffsetS;
    }

    xs.push(t);
    pressures.push(d.pressure_bar);
    scaleConnected = Boolean(d.scale_connected);
    latestScaleWeightG = Number.isFinite(d.weight_g) ? d.weight_g : 0;
    latestFlowGps = Number.isFinite(d.flow_gps) ? d.flow_gps : 0;
    flows.push(scaleConnected ? latestFlowGps : null);
    weights.push(scaleConnected ? Math.max(0, latestScaleWeightG) : null);
    targets.push(currentProfileFn()(t));

    if (!shotActive && xs.length > MAX_IDLE_PTS) {
      const drop = xs.length - MAX_IDLE_PTS;
      xs.splice(0, drop);
      pressures.splice(0, drop);
      flows.splice(0, drop);
      weights.splice(0, drop);
      targets.splice(0, drop);
    }

    if (tempEl) tempEl.textContent = d.temperature_c.toFixed(1);
    if (barEl) barEl.textContent = d.pressure_bar.toFixed(1);
    if (psiEl) psiEl.textContent = d.pressure_psi.toFixed(1) + " psi";
    refreshScaleUi();

    if (statusEl) {
      statusEl.textContent = shotActive ? "Recording" : "Live";
      statusEl.className = shotActive
        ? "badge badge-recording"
        : "badge badge-live";
    }

    refreshPlot();
    if (shotActive) refreshStats();
  } catch (_e) {
    consecutiveFailures++;
    if (statusEl) {
      statusEl.textContent = "Disconnected";
      statusEl.className = "badge";
    }
    scaleConnected = false;
    latestFlowGps = 0;
    refreshScaleUi();

    if (consecutiveFailures >= HW_FAIL_THRESHOLD) {
      stopPolling();
      if (hwFailBanner) {
        if (hwFailMsg) {
          hwFailMsg.textContent = "Hardware unreachable — check connections.";
        }
        hwFailBanner.hidden = false;
      }
    }
  }
}

if (startBtn) {
  startBtn.addEventListener("click", () => {
    if (shotActive) stopShot();
    else startShot();
  });
}

if (windowSel) {
  windowSel.addEventListener("change", () => {
    windowS = parseInt(windowSel.value, 10);
  });
}

let pollStopped = false;

function startPolling() {
  pollStopped = false;
  schedulePoll();
}

function stopPolling() {
  pollStopped = true;
}

function schedulePoll() {
  if (pollStopped) return;
  setTimeout(async () => {
    await poll();
    schedulePoll();
  }, POLL_MS);
}

if (hwRetryBtn) {
  hwRetryBtn.addEventListener("click", () => {
    consecutiveFailures = 0;
    if (hwFailBanner) hwFailBanner.hidden = true;
    if (statusEl) {
      statusEl.textContent = "Reconnecting...";
      statusEl.className = "badge";
    }
    startPolling();
  });
}

document.addEventListener("DOMContentLoaded", () => {
  if (profileSel) {
    Object.keys(PROFILES).forEach((name, i) => {
      const opt = document.createElement("option");
      opt.value = name;
      opt.textContent = name;
      if (i === 0) opt.selected = true;
      profileSel.appendChild(opt);
    });
  }

  setTimeout(bootstrapChartAssets, 50);
  startPolling();
});

function showToast(html) {
  var toast = document.createElement('div');
  toast.className = 'toast';
  toast.innerHTML = html;
  document.body.appendChild(toast);
  setTimeout(function () {
    toast.style.opacity = '0';
    toast.style.transition = 'opacity 0.3s';
    setTimeout(function () { toast.remove(); }, 350);
  }, 5000);
}

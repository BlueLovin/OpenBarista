const POLL_MS = 250;
const MAX_IDLE_PTS = 720;
const IDLE_WINDOW_S = 60;

// ─── Shot Storage ─────────────────────────────────────────────────────────────
const SHOTS_STORAGE_KEY = "openbarista_shots";
const MAX_SAVED_SHOTS = 20;
const MIN_SHOT_DURATION_S = 3;

function loadSavedShots() {
  try {
    const raw = localStorage.getItem(SHOTS_STORAGE_KEY);
    return raw ? JSON.parse(raw) : [];
  } catch (_e) {
    return [];
  }
}

function persistShots(shots) {
  try {
    localStorage.setItem(SHOTS_STORAGE_KEY, JSON.stringify(shots));
  } catch (_e) {
    // localStorage unavailable or quota exceeded
  }
}

function saveShot(shotXs, shotPressures, shotFlows, shotWeights, shotTargets) {
  const duration = shotXs.length ? shotXs[shotXs.length - 1] : 0;
  if (duration < MIN_SHOT_DURATION_S) return false;

  const validP = shotPressures.filter((v) => v != null && Number.isFinite(v));
  const peakBar = validP.length ? Math.max(...validP) : null;
  const avgBar = validP.length
    ? validP.reduce((a, b) => a + b, 0) / validP.length
    : null;

  const validW = shotWeights.filter((v) => v != null && Number.isFinite(v));
  const firstW = validW.length ? validW[0] : null;
  const lastW = validW.length ? validW[validW.length - 1] : null;
  const finalWeightG =
    firstW != null && lastW != null ? Math.max(0, lastW - firstW) : null;

  const shot = {
    id: String(Date.now()),
    savedAt: Date.now(),
    duration,
    peakBar,
    avgBar,
    finalWeightG,
    profile: profileSel ? profileSel.value : "None",
    xs: shotXs,
    pressures: shotPressures,
    flows: shotFlows,
    weights: shotWeights,
    targets: shotTargets,
  };

  const shots = loadSavedShots();
  shots.unshift(shot);
  if (shots.length > MAX_SAVED_SHOTS) shots.splice(MAX_SAVED_SHOTS);
  persistShots(shots);
  return true;
}

function deleteSavedShot(id) {
  persistShots(loadSavedShots().filter((s) => s.id !== id));
}

function clearAllSavedShots() {
  persistShots([]);
}

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
      timerEl.innerHTML =
        formatTimer((performance.now() - shotStartMs) / 1000) +
        "<span>s</span>";
    }
  }, 50);
}

function stopTimer() {
  if (timerHandle) {
    clearInterval(timerHandle);
    timerHandle = null;
  }
}

function startShot() {
  xs = [];
  pressures = [];
  flows = [];
  weights = [];
  targets = [];
  shotStartMs = performance.now();
  shotActive = true;
  shotWeightZeroG = scaleConnected ? latestScaleWeightG : 0;
  windowS = 90;
  if (startBtn) {
    startBtn.textContent = "STOP EXTRACTION";
    startBtn.dataset.active = "1";
  }
  if (timerEl) timerEl.innerHTML = "00:00<span>s</span>";
  if (peakBarEl) peakBarEl.textContent = "--";
  if (avgBarEl) avgBarEl.textContent = "--";
  startTimer();
}

function stopShot() {
  if (shotActive && xs.length > 1) {
    const saved = saveShot(
      [...xs],
      [...pressures],
      [...flows],
      [...weights],
      [...targets],
    );
    if (saved) showToast("Shot saved");
  }
  shotActive = false;
  shotStartMs = null;
  shotWeightZeroG = null;
  windowS = IDLE_WINDOW_S;
  stopTimer();
  if (startBtn) {
    startBtn.textContent = "START EXTRACTION";
    delete startBtn.dataset.active;
  }
}

// ─── Toast Notifications ─────────────────────────────────────────────────────
function showToast(message, durationMs = 2800) {
  const container = $("toastContainer");
  if (!container) return;
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = message;
  container.appendChild(el);
  requestAnimationFrame(() =>
    requestAnimationFrame(() => el.classList.add("toast-show")),
  );
  setTimeout(() => {
    el.classList.remove("toast-show");
    el.addEventListener("transitionend", () => el.remove(), { once: true });
  }, durationMs);
}

// ─── View Switching ───────────────────────────────────────────────────────────
let currentView = "brew";

function showView(view) {
  currentView = view;
  const brewView = $("brewView");
  const historyView = $("historyView");
  const navBrew = $("navBrew");
  const navHistory = $("navHistory");

  if (view === "brew") {
    if (brewView) brewView.hidden = false;
    if (historyView) historyView.hidden = true;
    if (navBrew) {
      navBrew.classList.add("nav-item-active");
      navBrew.setAttribute("aria-current", "page");
    }
    if (navHistory) {
      navHistory.classList.remove("nav-item-active");
      navHistory.removeAttribute("aria-current");
    }
  } else if (view === "history") {
    if (brewView) brewView.hidden = true;
    if (historyView) historyView.hidden = false;
    if (navBrew) {
      navBrew.classList.remove("nav-item-active");
      navBrew.removeAttribute("aria-current");
    }
    if (navHistory) {
      navHistory.classList.add("nav-item-active");
      navHistory.setAttribute("aria-current", "page");
    }
    renderHistory();
  }
}

// ─── History View ─────────────────────────────────────────────────────────────
let replayPlot = null;
let replayPlotId = null;

function renderHistory() {
  const list = $("historyList");
  if (!list) return;
  list.innerHTML = "";

  const replayPanelEl = $("replayPanel");
  if (replayPanelEl) replayPanelEl.hidden = true;
  if (replayPlot) {
    replayPlot.destroy();
    replayPlot = null;
    replayPlotId = null;
  }

  const shots = loadSavedShots();
  if (!shots.length) {
    const empty = document.createElement("p");
    empty.className = "history-empty";
    empty.textContent =
      "No shots saved yet. Start an extraction to begin recording!";
    list.appendChild(empty);
    return;
  }

  shots.forEach((shot) => {
    const card = document.createElement("article");
    card.className = "shot-card";
    card.dataset.id = shot.id;

    const date = new Date(shot.savedAt);
    const dateFmt = date.toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
    });
    const timeFmt = date.toLocaleTimeString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
    });

    const hdr = document.createElement("div");
    hdr.className = "shot-card-hdr";

    const meta = document.createElement("div");
    meta.className = "shot-card-meta";

    const timeSpan = document.createElement("span");
    timeSpan.className = "shot-card-time";
    timeSpan.textContent = `${dateFmt} · ${timeFmt}`;

    const profileSpan = document.createElement("span");
    profileSpan.className = "shot-card-profile";
    profileSpan.textContent = shot.profile;

    meta.append(timeSpan, profileSpan);

    const delBtn = document.createElement("button");
    delBtn.className = "shot-del-btn";
    delBtn.setAttribute("aria-label", "Delete this shot");
    delBtn.dataset.id = shot.id;
    delBtn.textContent = "\u2715";
    delBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      deleteSavedShot(shot.id);
      showToast("Shot deleted");
      renderHistory();
    });

    hdr.append(meta, delBtn);
    card.appendChild(hdr);

    const statsRow = document.createElement("div");
    statsRow.className = "shot-stats-row";

    function makeStat(label, value, unit) {
      const div = document.createElement("div");
      div.className = "shot-stat";
      const lbl = document.createElement("span");
      lbl.className = "shot-stat-lbl";
      lbl.textContent = label;
      const val = document.createElement("span");
      val.className = "shot-stat-val";
      val.textContent = value;
      if (unit) {
        const u = document.createElement("span");
        u.className = "shot-stat-unit";
        u.textContent = unit;
        val.appendChild(u);
      }
      div.append(lbl, val);
      return div;
    }

    statsRow.append(
      makeStat("Duration", formatTimer(shot.duration), "s"),
      makeStat(
        "Peak",
        shot.peakBar != null ? shot.peakBar.toFixed(1) : "--",
        shot.peakBar != null ? " bar" : "",
      ),
      makeStat(
        "Avg",
        shot.avgBar != null ? shot.avgBar.toFixed(1) : "--",
        shot.avgBar != null ? " bar" : "",
      ),
      makeStat(
        "Weight",
        shot.finalWeightG != null ? shot.finalWeightG.toFixed(1) : "--",
        shot.finalWeightG != null ? " g" : "",
      ),
    );
    card.appendChild(statsRow);

    const replayBtn = document.createElement("button");
    replayBtn.className = "shot-replay-btn";
    replayBtn.dataset.id = shot.id;
    replayBtn.innerHTML = "&#9654; Replay";
    replayBtn.addEventListener("click", () => showReplay(shot));
    card.appendChild(replayBtn);

    list.appendChild(card);
  });
}

function buildReplayPlotOpts(width) {
  const height = Math.min(180, Math.max(120, Math.floor(width * 0.45)));
  return {
    width,
    height,
    cursor: { show: false },
    legend: { show: false },
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
        labelSize: 12,
        stroke: "#8192b5",
        grid: { stroke: "#1d273a", width: 1 },
        ticks: { stroke: "#2c3a56", width: 1 },
        values: (_u, ticks) => ticks.map((v) => v.toFixed(0) + "s"),
      },
      {
        scale: "bar",
        label: "bar",
        labelSize: 12,
        stroke: "#ff9c66",
        grid: { stroke: "#1d273a", width: 1 },
        ticks: { stroke: "#2c3a56", width: 1 },
        values: (_u, ticks) => ticks.map((v) => v.toFixed(1)),
      },
      {
        scale: "flow",
        side: 1,
        label: "g/s",
        labelSize: 12,
        stroke: "#65a2ff",
        grid: { show: false },
        ticks: { stroke: "#2c3a56", width: 1 },
        values: (_u, ticks) => ticks.map((v) => v.toFixed(1)),
      },
      {
        scale: "weight",
        side: 1,
        label: "g",
        labelSize: 12,
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

async function showReplay(shot) {
  const replayPanelEl = $("replayPanel");
  const replayChartEl = $("replayChart");
  const replayStatsEl = $("replayStats");
  if (!replayPanelEl || !replayChartEl) return;

  replayPanelEl.hidden = false;
  replayPanelEl.scrollIntoView({ behavior: "smooth", block: "nearest" });

  if (replayPlot) {
    replayPlot.destroy();
    replayPlot = null;
  }
  replayChartEl.innerHTML = "";
  replayPlotId = shot.id;

  if (replayStatsEl) {
    replayStatsEl.innerHTML = "";
    const date = new Date(shot.savedAt);
    const dateFmt = date.toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
    });
    const timeFmt = date.toLocaleTimeString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
    });

    function makeReplayStat(label, value, unit) {
      const d = document.createElement("div");
      d.className = "shot-stat";
      const l = document.createElement("span");
      l.className = "shot-stat-lbl";
      l.textContent = label;
      const v = document.createElement("span");
      v.className = "shot-stat-val";
      v.textContent = value;
      if (unit) {
        const u = document.createElement("span");
        u.className = "shot-stat-unit";
        u.textContent = unit;
        v.appendChild(u);
      }
      d.append(l, v);
      return d;
    }

    replayStatsEl.append(
      makeReplayStat("Saved", `${dateFmt} · ${timeFmt}`, ""),
      makeReplayStat("Duration", formatTimer(shot.duration), "s"),
      makeReplayStat(
        "Peak",
        shot.peakBar != null ? shot.peakBar.toFixed(1) : "--",
        shot.peakBar != null ? " bar" : "",
      ),
      makeReplayStat(
        "Avg",
        shot.avgBar != null ? shot.avgBar.toFixed(1) : "--",
        shot.avgBar != null ? " bar" : "",
      ),
      makeReplayStat(
        "Weight",
        shot.finalWeightG != null ? shot.finalWeightG.toFixed(1) : "--",
        shot.finalWeightG != null ? " g" : "",
      ),
    );
  }

  try {
    await loadUplotJs();
    ensureUplotCss();

    if (replayPlotId !== shot.id) return;

    const width =
      replayChartEl.clientWidth || replayChartEl.offsetWidth || 280;
    replayPlot = new uPlot(
      buildReplayPlotOpts(width),
      [shot.xs, shot.pressures, shot.flows, shot.weights, shot.targets],
      replayChartEl,
    );
  } catch (_err) {
    replayChartEl.innerHTML =
      '<p class="replay-no-chart">Chart unavailable</p>';
  }
}

function closeReplay() {
  const replayPanelEl = $("replayPanel");
  if (replayPanelEl) replayPanelEl.hidden = true;
  if (replayPlot) {
    replayPlot.destroy();
    replayPlot = null;
    replayPlotId = null;
  }
  const replayChartEl = $("replayChart");
  if (replayChartEl) replayChartEl.innerHTML = "";
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

  const navBrewEl = $("navBrew");
  const navHistoryEl = $("navHistory");
  const clearHistoryBtnEl = $("clearHistoryBtn");
  const closeReplayBtnEl = $("closeReplayBtn");

  if (navBrewEl) {
    navBrewEl.addEventListener("click", (e) => {
      e.preventDefault();
      showView("brew");
    });
  }

  if (navHistoryEl) {
    navHistoryEl.addEventListener("click", (e) => {
      e.preventDefault();
      showView("history");
    });
  }

  if (clearHistoryBtnEl) {
    clearHistoryBtnEl.addEventListener("click", () => {
      clearAllSavedShots();
      showToast("History cleared");
      renderHistory();
    });
  }

  if (closeReplayBtnEl) {
    closeReplayBtnEl.addEventListener("click", closeReplay);
  }

  setTimeout(bootstrapChartAssets, 50);
  startPolling();
});

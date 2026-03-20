/* ── OpenBarista · Espresso Profile Monitor ─────────────────────────────────── */

// ── Configuration ─────────────────────────────────────────────────────────────

const POLL_MS = 500; // polling interval (ms)
const MAX_IDLE_PTS = 600; // max buffered points in idle mode (5 min @ 500ms)
const IDLE_WINDOW_S = 60; // default rolling window (seconds)

// Fixed Y-axis ranges — tuned for espresso (9+ bar, 85-100 °C boiler temp)
const P_MIN = 0;
const P_MAX = 12; // 12 bar covers pre-infusion → over-pressure safety
const T_MIN = 80;
const T_MAX = 102;

// ── Target pressure profiles ──────────────────────────────────────────────────
// Each function receives elapsed shot seconds and returns a bar value (or null).
// Extend PROFILES here to add your own.
const PROFILES = {
  "Flat 9 bar": (t) => 9.0,
  "Lever (9 → 6 bar)": (t) => Math.max(6.0, 9.0 - 0.1 * Math.max(0, t - 5)),
  "Pre-infusion (0→4→9)": (t) => (t < 6 ? Math.min(4.0, 0.65 * t) : 9.0),
  "Blooming (4→9 bar)": (t) =>
    t < 10 ? 4.0 : Math.min(9.0, 4.0 + 0.5 * (t - 10)),
  "Temperature surf": (t) =>
    t < 3 ? 9.0 : Math.max(6.0, 9.0 - 0.06 * (t - 3)),
  None: (_t) => null,
};

// ── State ─────────────────────────────────────────────────────────────────────

let shotActive = false;
let shotStartMs = null; // performance.now() snapshot when shot was started
let lastSeq = -1;
let idleOffsetS = 0; // monotonic x counter used in idle/monitor mode
let windowS = IDLE_WINDOW_S;

// Parallel arrays fed into uPlot: [xs, pressures, temperatures, targets]
let xs = [];
let pressures = [];
let temperatures = [];
let targets = [];

let timerHandle = null;

// ── DOM ───────────────────────────────────────────────────────────────────────

const $ = (id) => document.getElementById(id);

const statusEl = $("telemetryStatus");
const tempEl = $("metricTemp");
const barEl = $("metricBar");
const psiEl = $("metricPsi");
const peakBarEl = $("metricPeakBar");
const avgBarEl = $("metricAvgBar");
const timerEl = $("shotTimer");
const startBtn = $("startShotBtn");
const profileSel = $("profileSelect");
const windowSel = $("windowSelect");
const chartDiv = $("uplotChart");

// ── uPlot ─────────────────────────────────────────────────────────────────────

let plot = null;

function buildPlotOpts(width) {
  return {
    width,
    height: 270,
    cursor: { show: true },
    legend: { live: true },
    scales: {
      x: { time: false },
      // Fixed Y ranges: pressure 0-12 bar, temperature 80-102 °C
      bar: { range: () => [P_MIN, P_MAX] },
      tmp: { range: () => [T_MIN, T_MAX] },
    },
    axes: [
      {
        label: "t (s)",
        labelSize: 14,
        values: (_u, ticks) => ticks.map((v) => v.toFixed(0) + "s"),
      },
      {
        scale: "bar",
        label: "bar",
        labelSize: 14,
        stroke: "#1a6ab5",
        grid: { stroke: "#ede4da", width: 1 },
        ticks: { stroke: "#d4c4b4", width: 1 },
        values: (_u, ticks) => ticks.map((v) => v.toFixed(1)),
      },
      {
        scale: "tmp",
        side: 1,
        label: "°C",
        labelSize: 14,
        stroke: "#b85c00",
        grid: { show: false },
        ticks: { stroke: "#d4c4b4", width: 1 },
        values: (_u, ticks) => ticks.map((v) => v.toFixed(0)),
      },
    ],
    series: [
      {},
      {
        label: "Pressure",
        scale: "bar",
        stroke: "#1a6ab5",
        width: 2.5,
        fill: "rgba(26, 106, 181, 0.07)",
        points: { show: false },
      },
      {
        label: "Temp",
        scale: "tmp",
        stroke: "#b85c00",
        width: 1.5,
        points: { show: false },
      },
      {
        label: "Target",
        scale: "bar",
        stroke: "#6a8a1a",
        width: 1.5,
        dash: [6, 4],
        points: { show: false },
      },
    ],
  };
}

function initPlot() {
  if (!chartDiv || typeof uPlot === "undefined") return;
  const w = Math.max(chartDiv.offsetWidth, 300);
  plot = new uPlot(buildPlotOpts(w), [[], [], [], []], chartDiv);
}

window.addEventListener("resize", () => {
  if (plot && chartDiv) {
    plot.setSize({ width: Math.max(chartDiv.offsetWidth, 300), height: 270 });
  }
});

// ── Profile helpers ────────────────────────────────────────────────────────────

function currentProfileFn() {
  const key = profileSel ? profileSel.value : "Flat 9 bar";
  return PROFILES[key] ?? PROFILES["None"];
}

// ── Shot timer ─────────────────────────────────────────────────────────────────

function startTimer() {
  timerHandle = setInterval(() => {
    if (shotStartMs !== null && timerEl) {
      timerEl.textContent =
        ((performance.now() - shotStartMs) / 1000).toFixed(2) + "s";
    }
  }, 50);
}

function stopTimer() {
  if (timerHandle) {
    clearInterval(timerHandle);
    timerHandle = null;
  }
}

// ── Shot control ───────────────────────────────────────────────────────────────

function startShot() {
  xs = [];
  pressures = [];
  temperatures = [];
  targets = [];
  shotStartMs = performance.now();
  shotActive = true;
  windowS = 90; // show up to 90 s shot window
  if (startBtn) {
    startBtn.textContent = "Stop Shot";
    startBtn.dataset.active = "1";
  }
  if (timerEl) timerEl.textContent = "0.00s";
  if (peakBarEl) peakBarEl.textContent = "--";
  if (avgBarEl) avgBarEl.textContent = "--";
  startTimer();
}

function stopShot() {
  shotActive = false;
  shotStartMs = null;
  windowS = IDLE_WINDOW_S;
  stopTimer();
  if (startBtn) {
    startBtn.textContent = "Start Shot";
    delete startBtn.dataset.active;
  }
}

// ── Stats ──────────────────────────────────────────────────────────────────────

function refreshStats() {
  const valid = pressures.filter((v) => v != null && !isNaN(v));
  if (!valid.length) return;
  const peak = Math.max(...valid);
  const avg = valid.reduce((a, b) => a + b, 0) / valid.length;
  if (peakBarEl) peakBarEl.textContent = peak.toFixed(2) + " bar";
  if (avgBarEl) avgBarEl.textContent = avg.toFixed(2) + " bar";
}

// ── Plot refresh ───────────────────────────────────────────────────────────────

function refreshPlot() {
  if (!plot || !xs.length) return;

  const xNow = xs[xs.length - 1];

  // In idle mode slide the visible window; in shot mode show from x=0
  let startIdx = 0;
  if (!shotActive) {
    const cutoff = xNow - windowS;
    while (startIdx < xs.length - 1 && xs[startIdx] < cutoff) startIdx++;
  }

  plot.setData([
    xs.slice(startIdx),
    pressures.slice(startIdx),
    temperatures.slice(startIdx),
    targets.slice(startIdx),
  ]);

  const xMin = xs[startIdx];
  const xMax = shotActive ? Math.max(xNow + 1, 30) : xNow + 1;
  plot.setScale("x", { min: xMin, max: xMax });
}

// ── Poll ───────────────────────────────────────────────────────────────────────

async function poll() {
  try {
    const r = await fetch("/api/telemetry", { cache: "no-store" });
    const d = await r.json();

    // Deduplicate by sequence number — the sensor loop runs at 50 ms,
    // so most polls within the same reading window are skipped here.
    if (d.seq === lastSeq) return;
    lastSeq = d.seq;

    // Compute elapsed-time X value
    let t;
    if (shotActive) {
      t = (performance.now() - shotStartMs) / 1000;
    } else {
      idleOffsetS += POLL_MS / 1000;
      t = idleOffsetS;
    }

    xs.push(t);
    pressures.push(d.pressure_bar);
    temperatures.push(d.temperature_c);
    targets.push(currentProfileFn()(t));

    // Trim idle buffer to avoid unbounded growth
    if (!shotActive && xs.length > MAX_IDLE_PTS) {
      const drop = xs.length - MAX_IDLE_PTS;
      xs.splice(0, drop);
      pressures.splice(0, drop);
      temperatures.splice(0, drop);
      targets.splice(0, drop);
    }

    // Update metric cards
    if (tempEl) tempEl.textContent = d.temperature_c.toFixed(2) + " \u00b0C";
    if (barEl) barEl.textContent = d.pressure_bar.toFixed(2) + " bar";
    if (psiEl) psiEl.textContent = d.pressure_psi.toFixed(2) + " psi";

    if (statusEl) {
      statusEl.textContent = shotActive ? "\u25cf Recording" : "\u25cf Live";
      statusEl.className = shotActive
        ? "badge badge-recording"
        : "badge badge-live";
    }

    refreshPlot();
    if (shotActive) refreshStats();
  } catch (_e) {
    if (statusEl) {
      statusEl.textContent = "Disconnected";
      statusEl.className = "badge";
    }
  }
}

// ── Events ─────────────────────────────────────────────────────────────────────

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

// ── Bootstrap ──────────────────────────────────────────────────────────────────

document.addEventListener("DOMContentLoaded", () => {
  if (profileSel) {
    Object.keys(PROFILES).forEach((name, i) => {
      const opt = document.createElement("option");
      opt.value = name;
      opt.textContent = name;
      if (i === 0) opt.selected = true; // "Flat 9 bar" default
      profileSel.appendChild(opt);
    });
  }

  initPlot();
  setInterval(poll, POLL_MS);
  poll();
});

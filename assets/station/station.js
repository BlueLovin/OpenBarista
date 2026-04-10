const POLL_MS = 500;
const MAX_IDLE_PTS = 600;
const IDLE_WINDOW_S = 60;

const P_MIN = 0;
const P_MAX = 12;
const T_MIN = 80;
const T_MAX = 102;

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

let xs = [];
let pressures = [];
let temperatures = [];
let targets = [];

let timerHandle = null;
let plot = null;

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

function buildPlotOpts(width) {
  return {
    width,
    height: 250,
    cursor: { show: true },
    legend: { live: true },
    scales: {
      x: { time: false },
      bar: { range: () => [P_MIN, P_MAX] },
      tmp: { range: () => [T_MIN, T_MAX] },
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
        scale: "tmp",
        side: 1,
        label: "C",
        labelSize: 14,
        stroke: "#54ebf6",
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
        label: "Temp",
        scale: "tmp",
        stroke: "#65a2ff",
        width: 1.6,
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

function initPlot() {
  if (!chartDiv || typeof uPlot === "undefined") return;
  const w = Math.max(chartDiv.offsetWidth, 300);
  plot = new uPlot(buildPlotOpts(w), [[], [], [], []], chartDiv);
}

window.addEventListener("resize", () => {
  if (plot && chartDiv) {
    plot.setSize({ width: Math.max(chartDiv.offsetWidth, 300), height: 250 });
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
  temperatures = [];
  targets = [];
  shotStartMs = performance.now();
  shotActive = true;
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
  shotActive = false;
  shotStartMs = null;
  windowS = IDLE_WINDOW_S;
  stopTimer();
  if (startBtn) {
    startBtn.textContent = "START EXTRACTION";
    delete startBtn.dataset.active;
  }
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
    temperatures.slice(startIdx),
    targets.slice(startIdx),
  ]);

  const xMin = xs[startIdx];
  const xMax = shotActive ? Math.max(xNow + 1, 30) : xNow + 1;
  plot.setScale("x", { min: xMin, max: xMax });
}

async function poll() {
  try {
    const r = await fetch("/api/telemetry", { cache: "no-store" });
    const d = await r.json();

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
    temperatures.push(d.temperature_c);
    targets.push(currentProfileFn()(t));

    if (!shotActive && xs.length > MAX_IDLE_PTS) {
      const drop = xs.length - MAX_IDLE_PTS;
      xs.splice(0, drop);
      pressures.splice(0, drop);
      temperatures.splice(0, drop);
      targets.splice(0, drop);
    }

    if (tempEl) tempEl.textContent = d.temperature_c.toFixed(1);
    if (barEl) barEl.textContent = d.pressure_bar.toFixed(1);
    if (psiEl) psiEl.textContent = d.pressure_psi.toFixed(1) + " psi";

    if (statusEl) {
      statusEl.textContent = shotActive ? "Recording" : "Live";
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
  setInterval(poll, POLL_MS);
  poll();
});

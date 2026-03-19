const maxPoints = 120;
const points = [];

const statusEl = document.getElementById("telemetryStatus");
const tempEl = document.getElementById("metricTemp");
const barEl = document.getElementById("metricBar");
const psiEl = document.getElementById("metricPsi");
const canvas = document.getElementById("telemetryCanvas");
const ctx = canvas ? canvas.getContext("2d") : null;

function pushPoint(sample) {
  points.push(sample);
  if (points.length > maxPoints) {
    points.shift();
  }
}

function rangeFor(key) {
  if (points.length === 0) {
    return { min: 0, max: 1 };
  }

  let min = Number.POSITIVE_INFINITY;
  let max = Number.NEGATIVE_INFINITY;
  for (const p of points) {
    const value = p[key];
    if (value < min) min = value;
    if (value > max) max = value;
  }

  if (!Number.isFinite(min) || !Number.isFinite(max) || min === max) {
    return { min: min - 1, max: max + 1 };
  }

  const pad = (max - min) * 0.1;
  return { min: min - pad, max: max + pad };
}

function mapY(v, range, h, top, bottom) {
  const usable = h - top - bottom;
  const ratio = (v - range.min) / (range.max - range.min);
  return top + (1 - ratio) * usable;
}

function drawSeries(dataKey, color, range, w, h, top, right, bottom, left) {
  if (!ctx || points.length < 2) {
    return;
  }

  const usableW = w - left - right;
  ctx.beginPath();
  ctx.strokeStyle = color;
  ctx.lineWidth = 2;

  points.forEach((p, idx) => {
    const x = left + (idx / (maxPoints - 1)) * usableW;
    const y = mapY(p[dataKey], range, h, top, bottom);
    if (idx === 0) {
      ctx.moveTo(x, y);
    } else {
      ctx.lineTo(x, y);
    }
  });

  ctx.stroke();
}

function drawAxes(tempRange, pressureRange, w, h, top, right, bottom, left) {
  if (!ctx) {
    return;
  }

  ctx.strokeStyle = "#cbb8a6";
  ctx.lineWidth = 1;

  ctx.beginPath();
  ctx.moveTo(left, top);
  ctx.lineTo(left, h - bottom);
  ctx.lineTo(w - right, h - bottom);
  ctx.stroke();

  ctx.fillStyle = "#5a4637";
  ctx.font = "12px sans-serif";
  ctx.fillText(`${tempRange.max.toFixed(1)} C`, 6, top + 10);
  ctx.fillText(`${tempRange.min.toFixed(1)} C`, 6, h - bottom);

  const rightLabel = `${pressureRange.min.toFixed(2)}-${pressureRange.max.toFixed(2)} bar`;
  ctx.fillText(rightLabel, w - right - 100, top + 10);
}

function drawGraph() {
  if (!ctx || !canvas) {
    return;
  }

  const w = canvas.width;
  const h = canvas.height;
  const top = 20;
  const right = 24;
  const bottom = 28;
  const left = 44;

  ctx.clearRect(0, 0, w, h);

  const tempRange = rangeFor("temperature_c");
  const pressureRange = rangeFor("pressure_bar");
  drawAxes(tempRange, pressureRange, w, h, top, right, bottom, left);
  drawSeries(
    "temperature_c",
    "#bd5a12",
    tempRange,
    w,
    h,
    top,
    right,
    bottom,
    left,
  );
  drawSeries(
    "pressure_bar",
    "#176087",
    pressureRange,
    w,
    h,
    top,
    right,
    bottom,
    left,
  );
}

function updateMetrics(sample) {
  if (tempEl) tempEl.textContent = `${sample.temperature_c.toFixed(2)} C`;
  if (barEl) barEl.textContent = `${sample.pressure_bar.toFixed(2)} bar`;
  if (psiEl) psiEl.textContent = `${sample.pressure_psi.toFixed(2)} psi`;
}

async function pollTelemetry() {
  try {
    const response = await fetch("/api/telemetry", { cache: "no-store" });
    const sample = await response.json();
    pushPoint(sample);
    updateMetrics(sample);
    drawGraph();
    if (statusEl) statusEl.textContent = "Live";
  } catch (_err) {
    if (statusEl) statusEl.textContent = "Disconnected";
  }
}

setInterval(pollTelemetry, 500);
pollTelemetry();

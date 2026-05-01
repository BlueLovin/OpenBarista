/* ── History page JS ─────────────────────────────────────────────────── */

(function () {
  'use strict';

  const CHART_OPTS = {
    width: Math.min(window.innerWidth - 32, 600),
    height: 220,
    cursor: { show: true },
    legend: { show: true },
    scales: { x: { time: false }, y: { range: [0, 12] } },
    series: [
      {},
      { label: 'Pressure', stroke: '#ff7828', width: 2, scale: 'y' },
      { label: 'Flow', stroke: '#4ea8de', width: 2, scale: 'y2' },
      { label: 'Weight', stroke: '#56d364', width: 2, scale: 'y3' },
    ],
    axes: [
      { label: 's' },
      { scale: 'y', label: 'bar', size: 44 },
      { scale: 'y2', label: 'g/s', side: 1, size: 44 },
    ],
  };

  // ── Routing ────────────────────────────────────────────────────────── //

  const params = new URLSearchParams(window.location.search);
  const shotId = params.has('id') ? parseInt(params.get('id'), 10) : null;

  if (shotId !== null && Number.isFinite(shotId)) {
    showDetail(shotId);
  } else {
    showList();
  }

  // ── List view ──────────────────────────────────────────────────────── //

  function showList() {
    document.getElementById('listView').hidden = false;
    document.getElementById('detailView').hidden = true;

    fetch('/api/shots')
      .then(function (r) { return r.json(); })
      .then(renderList)
      .catch(function () {
        document.getElementById('listEmpty').textContent =
          'Failed to load shots. Is the device reachable?';
      });
  }

  function renderList(shots) {
    var listEl = document.getElementById('shotList');
    var emptyEl = document.getElementById('listEmpty');
    var strip = document.getElementById('analyticsStrip');

    // Clear existing cards
    Array.from(listEl.querySelectorAll('.shot-card')).forEach(function (el) {
      el.remove();
    });

    if (!shots || shots.length === 0) {
      emptyEl.hidden = false;
      return;
    }

    emptyEl.hidden = true;
    strip.hidden = false;

    // Aggregate analytics
    var totalDuration = 0, totalYield = 0, totalPressure = 0, totalTemp = 0;
    shots.forEach(function (s) {
      totalDuration += s.duration_ms / 1000;
      totalYield += s.yield_g;
      totalPressure += s.max_pressure_bar;
      totalTemp += s.avg_temperature_c;
    });
    var n = shots.length;
    document.getElementById('avgDuration').textContent = (totalDuration / n).toFixed(0) + 's';
    document.getElementById('avgYield').textContent = (totalYield / n).toFixed(1) + 'g';
    document.getElementById('avgPressure').textContent = (totalPressure / n).toFixed(2) + ' bar';
    document.getElementById('avgTemp').textContent = (totalTemp / n).toFixed(1) + '°C';

    shots.forEach(function (shot) {
      var card = document.createElement('div');
      card.className = 'shot-card';
      card.setAttribute('role', 'listitem');
      card.innerHTML =
        '<span class="shot-card-ts">' + formatTimestamp(shot.unix_timestamp) + '</span>' +
        '<div class="shot-card-metrics">' +
          metric('Duration', (shot.duration_ms / 1000).toFixed(0) + 's') +
          metric('Yield', shot.yield_g.toFixed(1) + 'g') +
          metric('Max pressure', shot.max_pressure_bar.toFixed(2) + ' bar') +
          metric('Avg temp', shot.avg_temperature_c.toFixed(1) + '°C') +
        '</div>' +
        '<span class="shot-card-chevron" aria-hidden="true">&#8250;</span>';

      card.addEventListener('click', function () {
        window.location.href = '/history?id=' + shot.id;
      });

      listEl.appendChild(card);
    });
  }

  function metric(label, value) {
    return '<div class="shot-card-metric">' +
      '<span class="shot-card-metric-label">' + label + '</span>' +
      '<span class="shot-card-metric-value">' + value + '</span>' +
      '</div>';
  }

  // ── Detail view ────────────────────────────────────────────────────── //

  function showDetail(id) {
    document.getElementById('listView').hidden = true;
    var detailEl = document.getElementById('detailView');
    detailEl.hidden = false;

    fetch('/api/shot?id=' + id)
      .then(function (r) {
        if (!r.ok) throw new Error('not found');
        return r.json();
      })
      .then(function (shot) { renderDetail(shot); })
      .catch(function () {
        detailEl.innerHTML =
          '<p style="color:var(--c-muted);text-align:center;margin-top:2rem">' +
          'Shot not found. <a href="/history">Back to history</a></p>';
      });
  }

  function renderDetail(shot) {
    // Header
    document.getElementById('detailTimestamp').textContent =
      formatTimestamp(shot.unix_timestamp);

    // Metrics row
    document.getElementById('detailDuration').textContent =
      (shot.points.length > 0
        ? (shot.points[shot.points.length - 1].time_ms / 1000).toFixed(0)
        : '0');
    document.getElementById('detailMaxPressure').textContent =
      Math.max.apply(null, shot.points.map(function (p) { return p.pressure_bar; })).toFixed(2);
    document.getElementById('detailYield').textContent =
      (shot.points.length > 0
        ? shot.points[shot.points.length - 1].weight_g.toFixed(1)
        : '0');
    var avgTemp = shot.points.length > 0
      ? shot.points.reduce(function (a, p) { return a + p.temperature_c; }, 0) / shot.points.length
      : 0;
    document.getElementById('detailAvgTemp').textContent = avgTemp.toFixed(1);

    // Build chart data
    var xs = shot.points.map(function (p) { return p.time_ms / 1000; });
    var pressure = shot.points.map(function (p) { return p.pressure_bar; });
    var flow = shot.points.map(function (p) { return p.flow_gps; });
    var weight = shot.points.map(function (p) { return p.weight_g; });

    var chartEl = document.getElementById('detailChart');

    var opts = Object.assign({}, CHART_OPTS, {
      width: Math.min(window.innerWidth - 32, 600),
      scales: {
        x: { time: false },
        y: { range: [0, Math.max(10, Math.max.apply(null, pressure) * 1.2)] },
        y2: { range: [0, Math.max(4, Math.max.apply(null, flow) * 1.2)] },
        y3: { range: [0, Math.max(50, Math.max.apply(null, weight) * 1.2)] },
      },
    });

    // Assign series to their scales
    var seriesOpts = JSON.parse(JSON.stringify(CHART_OPTS.series));
    seriesOpts[2].scale = 'y2';
    seriesOpts[3].scale = 'y3';
    opts.series = seriesOpts;

    var fullData = [xs, pressure, flow, weight];
    // eslint-disable-next-line no-undef
    var chart = new uPlot(opts, fullData, chartEl);

    // ── Replay ──────────────────────────────────────────────────────── //

    var replayBtn = document.getElementById('replayBtn');
    var replayTimer = null;

    replayBtn.addEventListener('click', function () {
      if (replayTimer !== null) return; // already playing
      replayBtn.textContent = 'Playing…';
      replayBtn.disabled = true;

      // Show just the first point to start.
      chart.setData([
        xs.slice(0, 1),
        pressure.slice(0, 1),
        flow.slice(0, 1),
        weight.slice(0, 1),
      ]);

      var i = 1;

      function scheduleNext() {
        if (i >= xs.length) {
          replayTimer = null;
          replayBtn.disabled = false;
          replayBtn.textContent = 'Replay again';
          chart.setData(fullData);
          return;
        }
        // Delay = real time between this point and the previous one.
        var prevMs = shot.points[i - 1].time_ms;
        var currMs = shot.points[i].time_ms;
        var delay = Math.max(16, currMs - prevMs);
        replayTimer = setTimeout(function () {
          i++;
          chart.setData([
            xs.slice(0, i),
            pressure.slice(0, i),
            flow.slice(0, i),
            weight.slice(0, i),
          ]);
          scheduleNext();
        }, delay);
      }

      scheduleNext();
    });

    // ── Delete ───────────────────────────────────────────────────────── //

    document.getElementById('deleteBtn').addEventListener('click', function () {
      if (!confirm('Delete this shot? This cannot be undone.')) return;
      fetch('/api/shots', {
        method: 'POST',
        headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
        body: 'action=delete&id=' + shot.id,
      })
        .then(function (r) { return r.json(); })
        .then(function (d) {
          if (d.ok) {
            window.location.href = '/history';
          } else {
            alert('Failed to delete: ' + (d.message || 'unknown error'));
          }
        })
        .catch(function () { alert('Request failed.'); });
    });
  }

  // ── Helpers ────────────────────────────────────────────────────────── //

  function formatTimestamp(unixSecs) {
    if (!unixSecs || unixSecs === 0) return 'Unknown time';
    var d = new Date(unixSecs * 1000);
    return d.toLocaleString(undefined, {
      year: 'numeric', month: 'short', day: 'numeric',
      hour: '2-digit', minute: '2-digit',
    });
  }
})();

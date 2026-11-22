// The metrics dashboard: follows the server's event stream (or, without one, polls data.json)
// for the samples it has not seen, and draws the tiles, charts and tables. No libraries;
// charts are SVG drawn here.
"use strict";
(function () {
  const SVG = "http://www.w3.org/2000/svg";
  const KEEP = 720;
  // Against the origin, not the page's address, which may carry a user name and password.
  const dataUrl = new URL(document.body.dataset.data, location.origin).href;
  const eventsUrl = document.body.dataset.events ? new URL(document.body.dataset.events, location.origin).href : "";
  const $ = (id) => document.getElementById(id);
  let samples = [];
  let last = 0;
  let latest = null;
  let interval = 5;
  let range = 300;
  let paused = false;
  let timer = 0;
  // The live stream while it works; once it fails for good, polling takes over.
  let source = null;
  let live = !!eventsUrl && typeof EventSource === "function";
  let sortKey = "requests";
  let sortDir = -1;
  let filter = "";

  // ---- formatting ----
  function count(n) {
    if (n == null) return "–";
    const a = Math.abs(n);
    if (a >= 1e9) return (n / 1e9).toFixed(1) + "G";
    if (a >= 1e6) return (n / 1e6).toFixed(1) + "M";
    if (a >= 1e4) return (n / 1e3).toFixed(1) + "k";
    if (Number.isInteger(n)) return n.toLocaleString("en");
    return n.toFixed(a >= 100 ? 0 : a >= 10 ? 1 : 2);
  }
  function seconds(s) {
    if (s == null) return "–";
    if (s < 0.001) return (s * 1000).toFixed(2) + " ms";
    if (s < 0.01) return (s * 1000).toFixed(1) + " ms";
    if (s < 1) return (s * 1000).toFixed(0) + " ms";
    return s.toFixed(2) + " s";
  }
  function bytes(b) {
    if (b == null) return "–";
    if (b >= 2 ** 30) return (b / 2 ** 30).toFixed(2) + " GB";
    if (b >= 2 ** 20) return (b / 2 ** 20).toFixed(1) + " MB";
    return (b / 1024).toFixed(0) + " KB";
  }
  function percent(f) {
    if (f == null) return "–";
    if (f === 0) return "0%";
    if (f < 0.001) return "<0.1%";
    return (f * 100).toFixed(f < 0.1 ? 1 : 0) + "%";
  }
  function uptime(s) {
    s = Math.max(0, Math.floor(s));
    const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
    if (d > 0) return d + "d " + h + "h";
    if (h > 0) return h + "h " + m + "m";
    if (m > 0) return m + "m " + (s % 60) + "s";
    return s + "s";
  }
  function clock(t) {
    return new Date(t * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  }

  // ---- DOM helpers ----
  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }
  function svg(tag, attrs, parent) {
    const e = document.createElementNS(SVG, tag);
    for (const k in attrs) e.setAttribute(k, attrs[k]);
    if (parent) parent.appendChild(e);
    return e;
  }
  function legend(id, series, box) {
    const host = $(id);
    host.replaceChildren();
    if (series.length < 2) return;
    for (const s of series) {
      const item = el("span");
      item.append(el("span", "key k-" + s.cls + (box ? " box" : "")), document.createTextNode(s.name));
      host.appendChild(item);
    }
  }

  // ---- charts ----
  // A nice upper bound and ticks for values up to `max`.
  function ticks(max) {
    if (!(max > 0)) max = 1;
    const raw = max / 3;
    const p = 10 ** Math.floor(Math.log10(raw));
    const step = [1, 2, 2.5, 5, 10].map((m) => m * p).find((s) => s >= raw);
    const out = [];
    for (let v = 0; v <= max + step * 0.001; v += step) out.push(v);
    if (out[out.length - 1] < max) out.push(out[out.length - 1] + step);
    return out;
  }

  // Draw a time series chart in `id`. `series` are {name, cls, values}, aligned with `times`;
  // `stacked` draws areas on top of one another.
  function chart(id, times, series, format, stacked) {
    const host = $(id);
    const width = host.clientWidth;
    const height = host.clientHeight;
    host.replaceChildren();
    if (!width) return;
    const root = svg("svg", { width, height, viewBox: "0 0 " + width + " " + height }, host);
    const tip = el("div", "tip");
    tip.hidden = true;
    host.appendChild(tip);
    const left = 54, right = 10, top = 8, bottom = 20;
    const w = width - left - right, h = height - top - bottom;
    if (times.length < 2) {
      svg("text", { x: left + w / 2, y: top + h / 2, "text-anchor": "middle", class: "empty" }, root).textContent =
        times.length ? "Collecting… the next sample comes in " + interval + " s" : "Waiting for the first sample";
      return;
    }
    const stacks = series.map((s) => s.values.map((v) => v || 0));
    if (stacked) for (let i = 1; i < stacks.length; i++) stacks[i] = stacks[i].map((v, j) => v + stacks[i - 1][j]);
    let max = 0;
    for (const values of stacked ? [stacks[stacks.length - 1]] : series.map((s) => s.values))
      for (const v of values) if (v != null && v > max) max = v;
    const yt = ticks(max);
    const top_ = yt[yt.length - 1];
    const t0 = times[0], t1 = times[times.length - 1];
    const x = (t) => left + ((t - t0) / (t1 - t0 || 1)) * w;
    const y = (v) => top + h - (v / top_) * h;
    for (const v of yt) {
      svg("line", { x1: left, x2: left + w, y1: y(v), y2: y(v), class: v === 0 ? "baseline" : "gridline" }, root);
      svg("text", { x: left - 6, y: y(v) + 4, "text-anchor": "end", class: "tick" }, root).textContent = format(v);
    }
    const labels = Math.max(2, Math.min(5, Math.floor(w / 90)));
    for (let i = 0; i < labels; i++) {
      const t = t0 + ((t1 - t0) * i) / (labels - 1);
      const anchor = i === 0 ? "start" : i === labels - 1 ? "end" : "middle";
      svg("text", { x: x(t), y: height - 4, "text-anchor": anchor, class: "tick" }, root).textContent = clock(t);
    }
    if (stacked) {
      for (let i = stacks.length - 1; i >= 0; i--) {
        let d = "";
        times.forEach((t, j) => (d += (j ? "L" : "M") + x(t).toFixed(1) + "," + y(stacks[i][j]).toFixed(1)));
        for (let j = times.length - 1; j >= 0; j--)
          d += "L" + x(times[j]).toFixed(1) + "," + y(i ? stacks[i - 1][j] : 0).toFixed(1);
        svg("path", { d: d + "Z", class: "band f-" + series[i].cls }, root);
      }
    } else {
      for (const s of series) {
        let d = "", pen = false;
        s.values.forEach((v, j) => {
          if (v == null) { pen = false; return; }
          d += (pen ? "L" : "M") + x(times[j]).toFixed(1) + "," + y(v).toFixed(1);
          pen = true;
        });
        if (d) svg("path", { d, class: "line s-" + s.cls }, root);
      }
    }
    // Hover: a crosshair at the nearest sample, and every series' value there.
    const cross = svg("line", { y1: top, y2: top + h, class: "crosshair", visibility: "hidden" }, root);
    const dots = series.map((s) => svg("circle", { r: 4, class: "dot f-" + s.cls, visibility: "hidden" }, root));
    const hit = svg("rect", { x: left, y: top, width: w, height: h, fill: "transparent" }, root);
    function show(px) {
      const t = t0 + ((px - left) / w) * (t1 - t0);
      let j = 0;
      for (let k = 1; k < times.length; k++) if (Math.abs(times[k] - t) < Math.abs(times[j] - t)) j = k;
      const cx = x(times[j]);
      cross.setAttribute("x1", cx);
      cross.setAttribute("x2", cx);
      cross.setAttribute("visibility", "visible");
      tip.replaceChildren(el("div", "when", clock(times[j])));
      series.forEach((s, i) => {
        const v = s.values[j];
        const level = stacked ? stacks[i][j] : v;
        if (v == null) dots[i].setAttribute("visibility", "hidden");
        else {
          dots[i].setAttribute("cx", cx);
          dots[i].setAttribute("cy", y(level));
          dots[i].setAttribute("visibility", "visible");
        }
        const row = el("div", "row");
        row.append(el("span", "key k-" + s.cls), el("strong", null, format(v)), el("span", "name", s.name));
        tip.appendChild(row);
      });
      tip.hidden = false;
      const tw = tip.offsetWidth;
      tip.style.left = Math.max(0, Math.min(width - tw, cx + 12 + tw > width ? cx - tw - 12 : cx + 12)) + "px";
    }
    function hide() {
      cross.setAttribute("visibility", "hidden");
      dots.forEach((d) => d.setAttribute("visibility", "hidden"));
      tip.hidden = true;
    }
    hit.addEventListener("pointermove", (e) => show(e.clientX - root.getBoundingClientRect().left));
    hit.addEventListener("pointerleave", hide);
  }

  function sparkline(values) {
    const box = svg("svg", { viewBox: "0 0 100 28", preserveAspectRatio: "none" });
    const vs = values.filter((v) => v != null);
    if (vs.length < 2) return box;
    const lo = Math.min(...vs), hi = Math.max(...vs);
    let d = "";
    values.forEach((v, i) => {
      if (v == null) return;
      const px = (i / (values.length - 1)) * 100;
      const py = hi === lo ? 14 : 26 - ((v - lo) / (hi - lo)) * 24;
      d += (d ? "L" : "M") + px.toFixed(1) + "," + py.toFixed(1);
    });
    svg("path", { d, class: "spark", "vector-effect": "non-scaling-stroke" }, box);
    return box;
  }

  // ---- drawing ----
  function visible() {
    if (!samples.length) return [];
    const end = samples[samples.length - 1].time;
    return samples.filter((s) => s.time > end - range - interval / 2);
  }

  function drawCharts() {
    const shown = visible();
    const times = shown.map((s) => s.time);
    const per = (k) => shown.map((s) => (k === 1 ? s.classes[0] + s.classes[1] : s.classes[k]) / interval);
    const requests = [
      { name: "2xx", cls: "c2xx", values: per(1) },
      { name: "3xx", cls: "c3xx", values: per(2) },
      { name: "4xx", cls: "c4xx", values: per(3) },
      { name: "5xx", cls: "c5xx", values: per(4) },
    ];
    legend("l-requests", requests, true);
    chart("c-requests", times, requests, count, true);
    const latency = [
      { name: "p50", cls: "s1", values: shown.map((s) => s.p50) },
      { name: "p90", cls: "s2", values: shown.map((s) => s.p90) },
      { name: "p99", cls: "s3", values: shown.map((s) => s.p99) },
    ];
    legend("l-latency", latency);
    chart("c-latency", times, latency, seconds);
    const load = [
      { name: "In flight", cls: "s1", values: shown.map((s) => s.in_flight) },
      { name: "Connections", cls: "s2", values: shown.map((s) => s.connections) },
      { name: "Tasks", cls: "s3", values: shown.map((s) => s.tasks) },
    ];
    legend("l-load", load);
    chart("c-load", times, load, count);
    const memory = [
      { name: "Heap", cls: "s1", values: shown.map((s) => s.heap) },
      { name: "Resident", cls: "s2", values: shown.map((s) => s.resident || null) },
    ];
    legend("l-memory", memory);
    chart("c-memory", times, memory, bytes);
  }

  function drawTiles(d) {
    const s = d.summary;
    $("version").textContent = "version " + d.version;
    if (d.started != null) {
      $("t-uptime").textContent = uptime(d.now - d.started);
      $("t-started").textContent = "since " + new Date(d.started * 1000).toLocaleString();
    }
    $("t-rate").textContent = s.rate == null ? "–" : count(s.rate);
    $("t-total").textContent = count(s.requests) + " in total";
    const errors = $("t-errors");
    errors.textContent = percent(s.errors);
    errors.classList.toggle("bad", s.errors > 0);
    $("t-latency").textContent = [s.p50, s.p95, s.p99].map(seconds).join(" · ");
    $("t-flight").textContent = count(s.in_flight);
    $("t-connections").textContent = count(s.connections);
  }

  function cell(text, cls) {
    return el("td", cls, text);
  }

  function drawRoutes(d) {
    const body = $("routes").tBodies[0];
    const rows = d.routes.filter((r) => !filter || (r.method + " " + r.route).toLowerCase().includes(filter));
    rows.sort((a, b) => {
      const x = a[sortKey], y = b[sortKey];
      if (typeof x === "string") return sortDir * x.localeCompare(y);
      return sortDir * ((x == null ? -1 : x) - (y == null ? -1 : y));
    });
    body.replaceChildren();
    if (!rows.length) {
      const td = cell(d.routes.length ? "No route matches the filter." : "No requests yet.", "none");
      td.colSpan = 9;
      body.appendChild(el("tr")).appendChild(td);
    }
    for (const r of rows) {
      const tr = el("tr");
      tr.append(
        cell(r.method || "–"),
        cell(r.route, "path"),
        cell(count(r.requests), "num"),
        cell(r.rate == null ? "–" : count(r.rate), "num"),
        cell(percent(r.client_errors), "num"),
        cell(percent(r.server_errors), "num"),
        cell(seconds(r.p50), "num"),
        cell(seconds(r.p95), "num"),
        cell(seconds(r.p99), "num"),
      );
      body.appendChild(tr);
    }
    for (const th of $("routes").tHead.rows[0].cells)
      th.setAttribute("aria-sort", th.dataset.key === sortKey ? (sortDir < 0 ? "descending" : "ascending") : "none");
  }

  function drawEvents(id, events, none) {
    const body = $(id).tBodies[0];
    body.replaceChildren();
    if (!events.length) {
      const td = cell(none, "none");
      td.colSpan = 5;
      body.appendChild(el("tr")).appendChild(td);
      return;
    }
    for (const e of events) {
      const tr = el("tr");
      const request = cell(e.method + " " + e.path, "path");
      request.title = e.route === "unmatched" ? "No route" : "Route " + e.route;
      tr.append(
        cell(clock(e.time)),
        request,
        cell(String(e.status), "num status-code" + (e.status >= 500 ? " bad" : "")),
        cell(seconds(e.seconds), "num"),
        cell(e.id || "–", "id"),
      );
      body.appendChild(tr);
    }
  }

  function drawProgram(d) {
    const host = $("program");
    $("program-section").hidden = !d.program.length;
    host.replaceChildren();
    d.program.forEach((m, i) => {
      const card = el("div", "metric");
      card.append(el("div", "name", m.name), el("div", "help", m.help || m.kind));
      card.lastChild.title = m.help;
      card.appendChild(el("div", "value", count(m.value)));
      // A counter's line is its increase per sample; a gauge's, its value.
      let values = visible().map((s) => (s.program.length > i ? s.program[i] : null));
      if (m.kind !== "gauge") values = values.map((v, j) => (j && v != null && values[j - 1] != null ? v - values[j - 1] : null)).slice(1);
      card.appendChild(sparkline(values));
      host.appendChild(card);
    });
  }

  function draw() {
    if (!latest) return;
    drawTiles(latest);
    drawCharts();
    drawRoutes(latest);
    drawEvents("errors", latest.errors, "No server errors.");
    drawEvents("slowest", latest.slowest, "No requests in the last five minutes.");
    drawProgram(latest);
  }

  // ---- polling ----
  function status(text, bad) {
    const s = $("status");
    s.textContent = text;
    s.classList.toggle("bad", !!bad);
  }

  function apply(d, how) {
    if (d.last < last) samples = [];
    for (const s of d.samples) if (!samples.length || s.seq > samples[samples.length - 1].seq) samples.push(s);
    if (samples.length > KEEP) samples = samples.slice(samples.length - KEEP);
    last = d.last;
    interval = d.interval || 5;
    latest = d;
    draw();
    status(how + " " + clock(d.now));
  }

  async function poll() {
    clearTimeout(timer);
    try {
      const response = await fetch(dataUrl + "?since=" + last, { credentials: "same-origin", cache: "no-store" });
      if (!response.ok) throw new Error("HTTP " + response.status);
      apply(await response.json(), "Updated");
    } catch (error) {
      status("Cannot reach the server (" + error.message + "); retrying", true);
    }
    if (!paused) timer = setTimeout(poll, interval * 1000);
  }

  // Server-Sent Events: each sample arrives as it is taken. The browser reconnects by itself,
  // sending the last sample's number as Last-Event-ID; a stream refused outright (an old
  // server, a proxy that buffers) falls back to polling.
  function follow() {
    let opened = false;
    source = new EventSource(eventsUrl);
    source.addEventListener("open", () => {
      opened = true;
      document.body.dataset.mode = "live";
    });
    source.addEventListener("message", (e) => {
      try {
        apply(JSON.parse(e.data), "Live,");
      } catch (error) {
        status("Unreadable data from the server (" + error.message + ")", true);
      }
    });
    source.addEventListener("error", () => {
      if (source && source.readyState === EventSource.CLOSED) {
        source = null;
        if (!opened) live = false;
        document.body.dataset.mode = "polling";
        if (!paused) poll();
      } else status("Connection lost; reconnecting", true);
    });
  }

  function start() {
    if (live) follow();
    else poll();
  }

  function stop() {
    clearTimeout(timer);
    if (source) {
      source.close();
      source = null;
    }
  }

  // ---- controls ----
  for (const button of document.querySelectorAll("[data-range]")) {
    button.addEventListener("click", () => {
      range = Number(button.dataset.range);
      for (const b of document.querySelectorAll("[data-range]")) b.setAttribute("aria-pressed", String(b === button));
      draw();
    });
  }
  $("pause").addEventListener("click", () => {
    paused = !paused;
    $("pause").setAttribute("aria-pressed", String(paused));
    $("pause").textContent = paused ? "Resume" : "Pause";
    if (paused) {
      stop();
      status("Paused");
    } else start();
  });
  $("route-filter").addEventListener("input", (e) => {
    filter = e.target.value.trim().toLowerCase();
    if (latest) drawRoutes(latest);
  });
  for (const th of $("routes").tHead.rows[0].cells) {
    th.tabIndex = 0;
    const sort = () => {
      if (sortKey === th.dataset.key) sortDir = -sortDir;
      else {
        sortKey = th.dataset.key;
        sortDir = sortKey === "method" || sortKey === "route" ? 1 : -1;
      }
      if (latest) drawRoutes(latest);
    };
    th.addEventListener("click", sort);
    th.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        sort();
      }
    });
  }
  let resizing = 0;
  window.addEventListener("resize", () => {
    cancelAnimationFrame(resizing);
    resizing = requestAnimationFrame(drawCharts);
  });
  start();
})();

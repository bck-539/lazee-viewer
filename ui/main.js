// Lazee Viewer UI. No framework and no build step: the heavy lifting is in
// Rust, and this file only lays out what it is given.
"use strict";

const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open: openDialog } = window.__TAURI__.dialog;

const $ = (sel) => document.querySelector(sel);
const GAP = 4;
const OVERSCAN_ROWS = 3;

const state = {
  ids: [],
  offline: new Set(),
  source: null,
  sort: "newest_first",
  cellSize: 180,
  selected: null,
  scanning: false,
  paused: false,
};

// ---------- Virtual grid ----------
// Only the rows in view (plus a few either side) have DOM nodes. Cells are
// recycled as you scroll, so 250,000 photos cost the same as 250.

const grid = $("#grid");
const spacer = $("#spacer");
const pool = [];
let layout = { cols: 1, size: 180, rows: 0 };
let renderQueued = false;

function computeLayout() {
  const width = grid.clientWidth - GAP;
  const cols = Math.max(1, Math.floor(width / (state.cellSize + GAP)));
  const size = Math.floor(width / cols) - GAP;
  const rows = Math.ceil(state.ids.length / cols);
  layout = { cols, size, rows };
  spacer.style.height = `${rows * (size + GAP) + GAP}px`;
}

function queueRender() {
  if (!renderQueued) {
    renderQueued = true;
    requestAnimationFrame(() => {
      renderQueued = false;
      render();
    });
  }
}

function render() {
  const { cols, size, rows } = layout;
  const step = size + GAP;
  const first = Math.max(0, Math.floor(grid.scrollTop / step) - OVERSCAN_ROWS);
  const last = Math.min(rows, Math.ceil((grid.scrollTop + grid.clientHeight) / step) + OVERSCAN_ROWS);
  const start = first * cols;
  const end = Math.min(state.ids.length, last * cols);
  const needed = end - start;

  while (pool.length < needed) {
    const cell = document.createElement("div");
    cell.className = "cell";
    const img = document.createElement("img");
    img.decoding = "async";
    img.draggable = false;
    cell.appendChild(img);
    grid.appendChild(cell);
    pool.push(cell);
  }
  for (let i = 0; i < pool.length; i++) {
    const cell = pool[i];
    if (i >= needed) {
      cell.hidden = true;
      continue;
    }
    const index = start + i;
    const id = state.ids[index];
    const x = GAP + (index % cols) * step;
    const y = GAP + Math.floor(index / cols) * step;
    cell.hidden = false;
    cell.style.width = cell.style.height = `${size}px`;
    cell.style.transform = `translate(${x}px, ${y}px)`;
    if (cell.dataset.id !== String(id)) {
      cell.dataset.id = id;
      cell.firstChild.src = convertFileSrc(String(id), "thumb");
    }
    cell.classList.toggle("offline", state.offline.has(id));
    cell.classList.toggle("selected", id === state.selected);
  }
}

grid.addEventListener("scroll", queueRender, { passive: true });
new ResizeObserver(() => {
  computeLayout();
  queueRender();
}).observe(grid);

grid.addEventListener("click", (e) => {
  const cell = e.target.closest(".cell");
  if (cell) select(Number(cell.dataset.id));
});

grid.addEventListener("keydown", (e) => {
  if (state.selected === null || state.ids.length === 0) return;
  const moves = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -layout.cols, ArrowDown: layout.cols };
  if (!(e.key in moves)) return;
  e.preventDefault();
  const index = state.ids.indexOf(state.selected);
  const next = Math.min(state.ids.length - 1, Math.max(0, index + moves[e.key]));
  select(state.ids[next]);
  scrollIntoView(next);
});

function scrollIntoView(index) {
  const step = layout.size + GAP;
  const top = Math.floor(index / layout.cols) * step;
  if (top < grid.scrollTop) grid.scrollTop = top;
  else if (top + step > grid.scrollTop + grid.clientHeight) grid.scrollTop = top + step - grid.clientHeight + GAP;
}

async function select(id) {
  state.selected = id;
  queueRender();
  const info = await invoke("photo_info", { id });
  if (!info || state.selected !== id) return;
  const parts = [info.name];
  if (info.taken_at) parts.push(info.taken_at.replace("T", " "));
  if (info.width && info.height) parts.push(`${info.width} × ${info.height}`);
  if (info.camera) parts.push(info.camera);
  parts.push(info.online ? info.folder : `${info.folder} (drive not connected)`);
  if (info.error) parts.push(`Could not read: ${info.error}`);
  $("#info").textContent = parts.join("  ·  ");
}

// ---------- Library ----------

async function loadPhotos({ keepScroll = false } = {}) {
  const list = await invoke("photo_list", { sort: state.sort, source: state.source });
  state.ids = list.ids;
  state.offline = new Set(list.offline);
  $("#count").textContent = `${list.ids.length.toLocaleString()} photos`;
  $("#empty").hidden = list.ids.length > 0;
  if (!keepScroll) grid.scrollTop = 0;
  for (const cell of pool) cell.dataset.id = "";
  computeLayout();
  render();
}

async function loadSources(sources) {
  sources ??= await invoke("sources");
  const box = $("#sources");
  box.replaceChildren();
  for (const s of sources) {
    const b = document.createElement("button");
    b.className = "source" + (s.online ? "" : " offline") + (state.source === s.id ? " active" : "");
    b.dataset.source = s.id;
    b.title = s.online ? s.display_path : `${s.display_path}\nThis drive is not connected.`;
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = s.display_path;
    const n = document.createElement("span");
    n.className = "n";
    n.textContent = s.photo_count.toLocaleString();
    const rm = document.createElement("span");
    rm.className = "remove";
    rm.textContent = "✕";
    rm.title = "Remove from library (files are not deleted)";
    rm.addEventListener("click", (e) => {
      e.stopPropagation();
      removeSource(s);
    });
    b.append(name, n, rm);
    box.appendChild(b);
  }
}

$("#sidebar nav").addEventListener("click", (e) => {
  const b = e.target.closest(".source");
  if (!b) return;
  state.source = b.dataset.source === "" ? null : Number(b.dataset.source);
  document.querySelectorAll(".source").forEach((x) => x.classList.toggle("active", x === b));
  loadPhotos();
});

async function removeSource(s) {
  if (!confirm(`Remove ${s.display_path} from the library?\n\nThe photos stay where they are.`)) return;
  try {
    await invoke("remove_source", { id: s.id });
    if (state.source === s.id) state.source = null;
    await Promise.all([loadSources(), loadPhotos()]);
  } catch (e) {
    alert(e);
  }
}

$("#add-source").addEventListener("click", async () => {
  const path = await openDialog({ directory: true, multiple: false, title: "Add a photo folder" });
  if (!path) return;
  try {
    await invoke("add_source", { path });
    await loadSources();
    startScan();
  } catch (e) {
    alert(e);
  }
});

$("#sort").addEventListener("change", (e) => {
  state.sort = e.target.value;
  loadPhotos();
});

$("#zoom").addEventListener("input", (e) => {
  state.cellSize = Number(e.target.value);
  computeLayout();
  queueRender();
});

// ---------- Scanning ----------

function setScanning(on) {
  state.scanning = on;
  state.paused = false;
  $("#scan-start").hidden = on;
  $("#scan-pause").hidden = !on;
  $("#scan-cancel").hidden = !on;
  $("#scan-bar").hidden = !on;
  $("#scan-pause").textContent = "Pause";
}

async function startScan() {
  if (state.scanning) return;
  try {
    setScanning(true);
    $("#scan-status").textContent = "Checking drives…";
    await invoke("start_scan");
  } catch (e) {
    setScanning(false);
    $("#scan-status").textContent = String(e);
  }
}

$("#scan-start").addEventListener("click", startScan);
$("#scan-pause").addEventListener("click", () => {
  state.paused = !state.paused;
  invoke("pause_scan", { paused: state.paused });
  $("#scan-pause").textContent = state.paused ? "Resume" : "Pause";
});
$("#scan-cancel").addEventListener("click", () => invoke("cancel_scan"));

let lastLiveReload = 0;
listen("scan-progress", ({ payload: p }) => {
  const bar = $("#scan-bar");
  const where = p.source ? `${p.source}\n` : "";
  if (p.phase === "walking") {
    bar.removeAttribute("value");
    $("#scan-status").textContent = `${where}Looking for photos: ${p.found.toLocaleString()} found`;
  } else if (p.phase === "processing") {
    bar.max = Math.max(1, p.to_process);
    bar.value = p.processed;
    $("#scan-status").textContent =
      `${where}Reading new photos: ${p.processed.toLocaleString()} of ${p.to_process.toLocaleString()}`;
    // Show photos as they arrive, without reloading on every event.
    if (Date.now() - lastLiveReload > 4000) {
      lastLiveReload = Date.now();
      loadPhotos({ keepScroll: true });
    }
  } else {
    bar.removeAttribute("value");
    $("#scan-status").textContent = "Finishing up…";
  }
});

listen("scan-finished", async ({ payload }) => {
  setScanning(false);
  if (payload.Err) {
    $("#scan-status").textContent = `Scan failed: ${payload.Err}`;
  } else {
    const s = payload.Ok;
    const bits = [];
    if (s.added) bits.push(`${s.added.toLocaleString()} new`);
    if (s.updated) bits.push(`${s.updated.toLocaleString()} changed`);
    if (s.moved) bits.push(`${s.moved.toLocaleString()} moved`);
    if (s.removed) bits.push(`${s.removed.toLocaleString()} removed`);
    if (s.failed) bits.push(`${s.failed.toLocaleString()} could not be read`);
    let text = s.cancelled ? "Scan stopped. " : "Scan finished. ";
    text += bits.length ? bits.join(", ") + "." : "Nothing changed.";
    if (s.offline.length) text += ` Not connected: ${s.offline.join(", ")}.`;
    $("#scan-status").textContent = text;
  }
  await Promise.all([loadSources(), loadPhotos({ keepScroll: true })]);
});

// ---------- Start ----------

(async () => {
  await Promise.all([loadSources(), loadPhotos()]);
  grid.focus();
  // Drives may have been plugged in or unplugged since last time.
  loadSources(await invoke("refresh_volumes"));
  loadPhotos({ keepScroll: true });
})();

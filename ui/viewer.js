// Full-screen viewer: one photo at a time, with zoom and pan.
//
// The thumbnail shows instantly, scaled up, while the full image loads from
// disk; then the full image takes over. Neighbours are preloaded so flicking
// through a folder feels immediate. Uses `state`, `invoke`, `select` and
// friends from main.js.
"use strict";

const viewer = $("#viewer");
const vThumb = $("#viewer-thumb");
const vFull = $("#viewer-full");
const vMessage = $("#viewer-message");
const vFaces = $("#viewer-faces");
const { getCurrentWindow } = window.__TAURI__.window;

const view = {
  index: -1,
  scale: 1,
  x: 0,
  y: 0,
  fit: true, // follow the window size until the user zooms
  natW: 0,
  natH: 0,
  preload: [],
  boxes: false, // show face boxes
  drag: null,
  swipe: null,
};

const MAX_ZOOM = 8; // in device pixels per image pixel

function photoUrl(id) {
  return convertFileSrc(String(id), "photo");
}

function openViewer(index) {
  if (index < 0 || index >= state.ids.length) return;
  viewer.hidden = false;
  viewer.focus();
  show(index);
  wake();
}

async function closeViewer() {
  viewer.hidden = true;
  vFull.removeAttribute("src");
  vThumb.removeAttribute("src");
  view.preload = [];
  try {
    const win = getCurrentWindow();
    if (await win.isFullscreen()) await win.setFullscreen(false);
  } catch {}
  if (view.index >= 0) {
    select(state.ids[view.index]);
    scrollIntoView(view.index);
  }
  grid.focus();
}

function show(index) {
  index = Math.max(0, Math.min(state.ids.length - 1, index));
  view.index = index;
  const id = state.ids[index];
  view.fit = true;
  view.natW = view.natH = 0;
  vMessage.hidden = true;
  vThumb.hidden = false;
  vThumb.src = convertFileSrc(String(id), "thumb");
  vFull.style.visibility = "hidden";
  vFull.dataset.id = id;
  vFull.src = photoUrl(id);
  vFaces.replaceChildren();
  vFaces.hidden = true;
  $("#viewer-prev").hidden = index === 0;
  $("#viewer-next").hidden = index === state.ids.length - 1;
  updateTitle(id);
  // Warm the cache for the photos either side.
  view.preload = [index + 1, index - 1, index + 2]
    .filter((i) => i >= 0 && i < state.ids.length)
    .map((i) => {
      const img = new Image();
      img.src = photoUrl(state.ids[i]);
      return img;
    });
}

async function updateTitle(id) {
  $("#viewer-title").textContent = "";
  const info = await invoke("photo_info", { id });
  if (!info || String(id) !== vFull.dataset.id) return;
  const parts = [info.name];
  if (info.taken_at) parts.push(info.taken_at.replace("T", " "));
  if (info.camera) parts.push(info.camera);
  $("#viewer-title").textContent = parts.join("  ·  ");
  $("#viewer-title").title = info.folder;
}

vFull.addEventListener("load", () => {
  if (viewer.hidden) return;
  view.natW = vFull.naturalWidth;
  view.natH = vFull.naturalHeight;
  vFull.style.width = `${view.natW}px`;
  vFull.style.height = `${view.natH}px`;
  fitToScreen();
  vFull.style.visibility = "visible";
  vThumb.hidden = true;
  loadFaces(Number(vFull.dataset.id));
});

// ---------- Face boxes ----------

async function loadFaces(id) {
  vFaces.replaceChildren();
  if (!view.boxes) return;
  const faces = await invoke("photo_faces", { id });
  if (String(id) !== vFull.dataset.id) return;
  vFaces.replaceChildren(
    ...faces.map((f) => {
      const box = document.createElement("div");
      box.className = "face";
      box.style.left = `${f.x * 100}%`;
      box.style.top = `${f.y * 100}%`;
      box.style.width = `${f.w * 100}%`;
      box.style.height = `${f.h * 100}%`;
      return box;
    }),
  );
  vFaces.hidden = false;
  apply();
}

function toggleBoxes() {
  view.boxes = !view.boxes;
  $("#viewer-boxes").classList.toggle("on", view.boxes);
  if (view.boxes && view.natW) loadFaces(Number(vFull.dataset.id));
  else vFaces.hidden = true;
}

vFull.addEventListener("error", async () => {
  if (viewer.hidden || !vFull.getAttribute("src")) return;
  const id = Number(vFull.dataset.id);
  const problem = await invoke("photo_problem", { id });
  if (String(id) !== vFull.dataset.id) return;
  vMessage.textContent = problem;
  vMessage.hidden = false;
});

// ---------- Zoom and pan ----------

const dpr = () => window.devicePixelRatio || 1;

/// Scale at which one image pixel is one screen pixel.
const actualScale = () => 1 / dpr();

function fitScale() {
  if (!view.natW) return 1;
  // Small photos are shown at actual size rather than blown up.
  return Math.min(viewer.clientWidth / view.natW, viewer.clientHeight / view.natH, actualScale());
}

function apply() {
  const w = view.natW * view.scale;
  const h = view.natH * view.scale;
  const vw = viewer.clientWidth;
  const vh = viewer.clientHeight;
  // Centre along any axis where the image is smaller than the window, and
  // never let it be dragged off screen along the others.
  view.x = w <= vw ? (vw - w) / 2 : Math.min(0, Math.max(vw - w, view.x));
  view.y = h <= vh ? (vh - h) / 2 : Math.min(0, Math.max(vh - h, view.y));
  vFull.style.transform = `translate(${view.x}px, ${view.y}px) scale(${view.scale})`;
  // Face boxes sit on the photo, sized in screen pixels.
  vFaces.style.transform = `translate(${view.x}px, ${view.y}px)`;
  vFaces.style.width = `${w}px`;
  vFaces.style.height = `${h}px`;
  const zoomed = w > vw + 1 || h > vh + 1;
  viewer.classList.toggle("zoomed", zoomed);
  viewer.classList.toggle("pixels", view.scale / actualScale() >= 3);
  $("#viewer-zoom").textContent = view.natW ? `${Math.round((view.scale / actualScale()) * 100)}%` : "";
}

function fitToScreen() {
  view.fit = true;
  view.scale = fitScale();
  apply();
}

/// Zooms to `scale`, keeping the image point under (cx, cy) where it is.
function zoomTo(scale, cx = viewer.clientWidth / 2, cy = viewer.clientHeight / 2) {
  if (!view.natW) return;
  scale = Math.max(fitScale(), Math.min(MAX_ZOOM * actualScale(), scale));
  const ix = (cx - view.x) / view.scale;
  const iy = (cy - view.y) / view.scale;
  view.scale = scale;
  view.x = cx - ix * scale;
  view.y = cy - iy * scale;
  view.fit = scale === fitScale();
  apply();
}

function toggleActual(cx, cy) {
  if (Math.abs(view.scale - actualScale()) < 1e-6 || !view.fit) fitToScreen();
  else zoomTo(actualScale(), cx, cy);
}

viewer.addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    if (view.natW) {
      const factor = Math.exp(-e.deltaY * (e.deltaMode === 1 ? 0.05 : 0.0015));
      zoomTo(view.scale * factor, e.clientX, e.clientY);
    }
  },
  { passive: false },
);

viewer.addEventListener("dblclick", (e) => {
  if (e.target.closest("button")) return;
  toggleActual(e.clientX, e.clientY);
});

viewer.addEventListener("pointerdown", (e) => {
  if (e.target.closest("button")) return;
  viewer.setPointerCapture(e.pointerId);
  if (viewer.classList.contains("zoomed")) {
    view.drag = { x: e.clientX - view.x, y: e.clientY - view.y };
    viewer.classList.add("dragging");
  } else {
    view.swipe = { x: e.clientX, y: e.clientY, t: performance.now() };
  }
});

viewer.addEventListener("pointermove", (e) => {
  wake();
  if (!view.drag) return;
  view.x = e.clientX - view.drag.x;
  view.y = e.clientY - view.drag.y;
  view.fit = false;
  apply();
});

viewer.addEventListener("pointerup", (e) => {
  view.drag = null;
  viewer.classList.remove("dragging");
  const s = view.swipe;
  view.swipe = null;
  if (!s) return;
  // A quick horizontal flick goes to the next or previous photo.
  const dx = e.clientX - s.x;
  const dy = e.clientY - s.y;
  if (Math.abs(dx) > 60 && Math.abs(dx) > 2 * Math.abs(dy) && performance.now() - s.t < 600) {
    show(view.index + (dx < 0 ? 1 : -1));
  }
});

new ResizeObserver(() => {
  if (!viewer.hidden && view.natW) view.fit ? fitToScreen() : apply();
}).observe(viewer);

// ---------- Controls ----------

async function toggleFullscreen() {
  const win = getCurrentWindow();
  await win.setFullscreen(!(await win.isFullscreen()));
}

$("#viewer-close").addEventListener("click", closeViewer);
$("#viewer-prev").addEventListener("click", () => show(view.index - 1));
$("#viewer-next").addEventListener("click", () => show(view.index + 1));
$("#viewer-fit").addEventListener("click", fitToScreen);
$("#viewer-actual").addEventListener("click", () => zoomTo(actualScale()));
$("#viewer-fullscreen").addEventListener("click", toggleFullscreen);
$("#viewer-boxes").addEventListener("click", toggleBoxes);

viewer.addEventListener("keydown", (e) => {
  const step = 80;
  const keys = {
    Escape: closeViewer,
    Backspace: closeViewer,
    ArrowRight: () => (viewer.classList.contains("zoomed") && e.shiftKey ? pan(-step, 0) : show(view.index + 1)),
    ArrowLeft: () => (viewer.classList.contains("zoomed") && e.shiftKey ? pan(step, 0) : show(view.index - 1)),
    ArrowUp: () => pan(0, step),
    ArrowDown: () => pan(0, -step),
    PageDown: () => show(view.index + 1),
    PageUp: () => show(view.index - 1),
    " ": () => show(view.index + 1),
    Home: () => show(0),
    End: () => show(state.ids.length - 1),
    f: toggleFullscreen,
    b: toggleBoxes,
    F11: toggleFullscreen,
    0: fitToScreen,
    1: () => zoomTo(actualScale()),
    "+": () => zoomTo(view.scale * 1.25),
    "=": () => zoomTo(view.scale * 1.25),
    "-": () => zoomTo(view.scale / 1.25),
  };
  const action = keys[e.key];
  if (!action) return;
  e.preventDefault();
  wake();
  action();
});

function pan(dx, dy) {
  view.x += dx;
  view.y += dy;
  view.fit = false;
  apply();
}

// Hide the controls and cursor after a moment without the mouse moving.
let idleTimer = 0;
function wake() {
  viewer.classList.remove("idle");
  clearTimeout(idleTimer);
  idleTimer = setTimeout(() => viewer.classList.add("idle"), 2500);
}

// People: groups of look-alike faces, and the face settings. Uses `state`,
// `invoke`, `loadPhotos` and friends from main.js.
"use strict";

const peopleView = $("#people");
const peopleList = $("#people-list");
const facesDialog = $("#faces-dialog");
let faceSetup = null;

function showGrid() {
  peopleView.hidden = true;
  grid.hidden = false;
  $("#people-nav").classList.toggle("active", state.group !== null);
  if (state.group !== null)
    document.querySelectorAll("#sidebar nav .source[data-source]").forEach((x) => x.classList.remove("active"));
  $("#back-to-people").hidden = state.group === null;
  $("#sort").parentElement.hidden = false;
  $("#zoom").parentElement.hidden = false;
}

async function showPeople() {
  state.group = null;
  grid.hidden = true;
  peopleView.hidden = false;
  document.querySelectorAll(".source").forEach((x) => x.classList.remove("active"));
  $("#people-nav").classList.add("active");
  $("#back-to-people").hidden = true;
  $("#sort").parentElement.hidden = true;
  $("#zoom").parentElement.hidden = true;
  $("#info").textContent = "";
  await refreshPeople();
  peopleView.focus();
}

/// Redraws the People page if it is showing.
async function refreshPeople() {
  if (peopleView.hidden) return;
  const groups = await invoke("face_groups");
  const empty = $("#people-empty");
  $("#count").textContent = `${groups.length.toLocaleString()} people`;
  empty.hidden = groups.length > 0;
  if (!groups.length) {
    empty.textContent = faceSetup?.enabled
      ? "No faces grouped yet. They appear here as the face search finds the same person in more than one photo."
      : "Face finding is off. Turn it on in Face settings to group photos by the people in them.";
  }
  peopleList.replaceChildren(
    ...groups.map((g) => {
      const b = document.createElement("button");
      b.className = "person";
      b.title = `${g.photos.toLocaleString()} photos`;
      const img = document.createElement("img");
      img.src = convertFileSrc(String(g.cover), "face");
      img.alt = "";
      img.draggable = false;
      const n = document.createElement("span");
      n.textContent = g.photos.toLocaleString();
      b.append(img, n);
      b.addEventListener("click", () => openGroup(g));
      return b;
    }),
  );
}

async function openGroup(g) {
  state.group = g.id;
  state.source = null;
  showGrid();
  await loadPhotos();
  $("#count").textContent = `Unnamed person · ${state.ids.length.toLocaleString()} photos`;
  grid.focus();
}

$("#people-nav").addEventListener("click", showPeople);
$("#back-to-people").addEventListener("click", showPeople);

// ---------- Face settings ----------

async function loadFaceSetup() {
  faceSetup = await invoke("face_setup");
  $("#people-nav").hidden = !faceSetup.enabled && faceSetup.stats.faces === 0;
  return faceSetup;
}

function fillPacks(select, packs, chosen) {
  select.replaceChildren(
    ...packs.map((p) => {
      const o = document.createElement("option");
      o.value = p.id;
      o.textContent = `${p.name} (${p.licence})` + (p.problem ? ` — ${p.problem}` : "");
      o.disabled = !!p.problem;
      o.selected = p.id === chosen;
      return o;
    }),
  );
  select.disabled = packs.length < 2;
}

$("#face-settings").addEventListener("click", async () => {
  const s = await loadFaceSetup();
  $("#faces-enabled").checked = s.enabled;
  fillPacks($("#faces-detector"), s.detectors, s.detector);
  fillPacks($("#faces-embedder"), s.embedders, s.embedder);
  $("#faces-folder").textContent = s.user_models;
  $("#faces-problem").textContent = s.problem ?? "";
  $("#faces-problem").hidden = !s.problem;
  const st = s.stats;
  $("#faces-stats").textContent = st.models
    ? `${st.scanned.toLocaleString()} of ${st.photos.toLocaleString()} photos looked at, ` +
      `${st.faces.toLocaleString()} faces found.`
    : "";
  facesDialog.showModal();
});

facesDialog.addEventListener("close", async () => {
  if (facesDialog.returnValue !== "save") return;
  const s = faceSetup;
  const enabled = $("#faces-enabled").checked;
  const detector = $("#faces-detector").value || null;
  const embedder = $("#faces-embedder").value || null;
  const switching = s.stats.faces > 0 && (detector !== s.detector || embedder !== s.embedder);
  if (s.stats.faces > 0 && !enabled && !confirm("Turn off face finding and delete every face found so far?")) return;
  if (enabled && switching && !confirm("Switch models? Every photo will be looked at again with the new ones.")) {
    return;
  }
  if (enabled && s.problem && !confirm(`${s.problem}\n\nSave anyway?`)) return;
  try {
    await invoke("set_faces", { enabled, detector, embedder });
  } catch (e) {
    alert(e);
  }
  await loadFaceSetup();
  refreshPeople();
});

loadFaceSetup();

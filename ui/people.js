// People: named people, groups of look-alike faces nobody has named yet,
// suggestions to review, and the face settings. Uses `state`, `invoke`,
// `loadPhotos` and friends from main.js.
"use strict";

const peopleView = $("#people");
const facesDialog = $("#faces-dialog");
const nameDialog = $("#name-dialog");
let faceSetup = null;
let peopleCache = [];

const faceUrl = (id) => convertFileSrc(String(id), "face");

function el(tag, props = {}, ...children) {
  const e = document.createElement(tag);
  Object.assign(e, props);
  e.append(...children);
  return e;
}

/// A round face picture, or the person's initials when no face is named.
function avatar(face, name = "") {
  if (face != null) return el("img", { src: faceUrl(face), alt: "", draggable: false, className: "avatar" });
  const initials = name
    .split(/\s+/)
    .map((w) => w[0] ?? "")
    .join("")
    .slice(0, 2)
    .toUpperCase();
  return el("span", { className: "avatar initials", textContent: initials || "?" });
}

// ---------- Views ----------

function setToolbar(mode) {
  const photos = mode === "photos";
  $("#sort").parentElement.hidden = !photos;
  $("#zoom").parentElement.hidden = !photos;
  $("#person-bar").hidden = !(photos && (state.person !== null || state.group !== null));
}

function showGrid() {
  peopleView.hidden = true;
  grid.hidden = false;
  const inPeople = state.person !== null || state.group !== null;
  $("#people-nav").classList.toggle("active", inPeople);
  if (inPeople)
    document.querySelectorAll("#sidebar nav .source[data-source]").forEach((x) => x.classList.remove("active"));
  setToolbar("photos");
}

function showPeopleView() {
  state.group = state.person = null;
  grid.hidden = true;
  peopleView.hidden = false;
  document.querySelectorAll(".source").forEach((x) => x.classList.remove("active"));
  $("#people-nav").classList.add("active");
  setToolbar("people");
  $("#info").textContent = "";
}

async function showPeople() {
  showPeopleView();
  peopleView.dataset.view = "people";
  await refreshPeople();
  peopleView.focus();
}

async function loadNames() {
  peopleCache = await invoke("people");
  $("#people-names").replaceChildren(...peopleCache.map((p) => el("option", { value: p.name })));
  return peopleCache;
}

/// Redraws the People page if it is showing.
async function refreshPeople() {
  if (peopleView.hidden) return;
  if (peopleView.dataset.view === "review") return showReview();
  const [people, groups] = await Promise.all([loadNames(), invoke("face_groups")]);
  const pending = people.reduce((n, p) => n + p.suggestions, 0);
  $("#count").textContent = `${people.length.toLocaleString()} named, ${groups.length.toLocaleString()} not named yet`;

  const parts = [];
  if (pending) {
    const b = el(
      "button",
      { className: "review-banner" },
      `${pending.toLocaleString()} faces may be people you know. `,
      el("strong", { textContent: "Review suggestions" }),
    );
    b.addEventListener("click", () => showReview());
    parts.push(b);
  }
  if (people.length) {
    parts.push(el("h2", { textContent: "People" }));
    parts.push(
      el(
        "div",
        { className: "people-grid" },
        ...people.map((p) => {
          const b = el(
            "button",
            { className: "person", title: `${p.photos.toLocaleString()} photos` },
            avatar(p.cover, p.name),
            el("span", { className: "name", textContent: p.name }),
            el("span", { className: "n", textContent: p.photos.toLocaleString() }),
          );
          if (p.suggestions)
            b.append(el("span", { className: "badge", textContent: p.suggestions, title: "Suggestions to review" }));
          b.addEventListener("click", () => openPerson(p));
          return b;
        }),
      ),
    );
  }
  if (groups.length) {
    parts.push(el("h2", { textContent: "Not named yet" }));
    parts.push(
      el(
        "div",
        { className: "people-grid" },
        ...groups.map((g) => {
          const b = el(
            "button",
            { className: "person", title: `${g.photos.toLocaleString()} photos` },
            avatar(g.cover),
            el("span", { className: "n", textContent: `${g.photos.toLocaleString()} photos` }),
          );
          b.addEventListener("click", () => openGroup(g));
          return b;
        }),
      ),
    );
  }
  if (!parts.length) {
    parts.push(
      el("p", {
        className: "empty",
        textContent: faceSetup?.enabled
          ? "Nobody yet. People appear here as faces are found in more than one photo, and when your photos already have name tags."
          : "Nobody yet. Name tags already in your photos show up here after a scan. Turn on face finding in Face settings to group photos by the faces in them.",
      }),
    );
  }
  peopleView.replaceChildren(...parts);
}

async function openGroup(g) {
  state.group = g.id;
  state.person = null;
  state.source = null;
  showGrid();
  await loadPhotos();
  const input = el("input", { placeholder: "Who is this?", autocomplete: "off" });
  input.setAttribute("list", "people-names");
  const save = el("button", { className: "primary", textContent: "Save name" });
  const submit = async () => {
    if (!input.value.trim()) return input.focus();
    try {
      const id = await invoke("name_group", { group: g.id, name: input.value });
      await loadNames();
      const p = peopleCache.find((x) => x.id === id);
      if (p) openPerson(p);
    } catch (e) {
      alert(e);
    }
  };
  save.addEventListener("click", submit);
  input.addEventListener("keydown", (e) => e.key === "Enter" && submit());
  personBar("Unnamed person", input, save);
  input.focus();
}

async function openPerson(p) {
  state.person = p.id;
  state.group = null;
  state.source = null;
  showGrid();
  await loadPhotos();
  const rename = el("button", { textContent: "Rename…", title: "Renaming to someone else's name merges the two" });
  rename.addEventListener("click", async () => {
    const r = await askName({
      title: `Rename ${p.name}`,
      value: p.name,
      note: "Use another person's name to merge the two.",
    });
    if (r.action !== "save" || !r.name.trim() || r.name === p.name) return;
    const id = await invoke("rename_person", { person: p.id, name: r.name }).catch((e) => alert(e));
    await loadNames();
    const now = peopleCache.find((x) => x.id === id);
    if (now) openPerson(now);
  });
  const forget = el("button", { textContent: "Forget name" });
  forget.addEventListener("click", async () => {
    if (!confirm(`Forget ${p.name}? Their faces become unnamed. Tags inside the photo files are not changed.`)) return;
    await invoke("delete_person", { person: p.id }).catch((e) => alert(e));
    showPeople();
  });
  const actions = [rename, forget];
  if (p.suggestions) {
    const review = el("button", { className: "primary", textContent: `Review ${p.suggestions} suggestions` });
    review.addEventListener("click", () => showReview(p.id));
    actions.unshift(review);
  }
  personBar(p.name, ...actions);
}

function personBar(title, ...actions) {
  $("#person-title").textContent = title;
  $("#person-actions").replaceChildren(...actions);
  $("#count").textContent = `${state.ids.length.toLocaleString()} photos`;
  setToolbar("photos");
}

// ---------- Review ----------

async function showReview(person = null) {
  showPeopleView();
  peopleView.dataset.view = "review";
  const list = await invoke("suggestions", { person });
  const back = el("button", { textContent: "‹ People" });
  back.addEventListener("click", showPeople);
  const parts = [el("div", { className: "review-head" }, back, el("h2", { textContent: "Is this them?" }))];
  $("#count").textContent = `${list.length.toLocaleString()} suggestions`;
  if (!list.length) parts.push(el("p", { className: "empty", textContent: "Nothing to review right now." }));
  for (const s of list) {
    const yes = el("button", { className: "primary", textContent: `Yes, ${s.name}` });
    const no = el("button", { textContent: "No" });
    const shown = s.faces.slice(0, 12);
    const card = el(
      "div",
      { className: "suggestion" },
      el("div", { className: "chips" }, ...shown.map((f) => avatar(f))),
      el(
        "div",
        { className: "what" },
        el("strong", { textContent: s.name + "?" }),
        el("span", { className: "n", textContent: `${s.faces.length} faces in ${s.photos} photos` }),
      ),
      el("div", { className: "answer" }, yes, no),
    );
    const answer = async (cmd) => {
      card.classList.add("done");
      yes.disabled = no.disabled = true;
      await invoke(cmd, { person: s.person_id, faces: s.faces }).catch((e) => alert(e));
      card.remove();
      if (!peopleView.querySelector(".suggestion")) showReview(person);
    };
    yes.addEventListener("click", () => answer("accept_suggestion"));
    no.addEventListener("click", () => answer("reject_suggestion"));
    parts.push(card);
  }
  peopleView.replaceChildren(...parts);
  peopleView.focus();
}

// ---------- Name dialog ----------

/// Asks for a name. Resolves to { action: "save" | "extra" | "cancel", name }.
function askName({ title, value = "", chip = null, note = "", extra = null }) {
  $("#name-title").textContent = title;
  $("#name-input").value = value;
  $("#name-note").textContent = note;
  $("#name-note").hidden = !note;
  $("#name-chip").hidden = chip == null;
  if (chip != null) $("#name-chip").src = faceUrl(chip);
  $("#name-extra").hidden = !extra;
  $("#name-extra").textContent = extra ?? "";
  nameDialog.returnValue = "cancel";
  loadNames();
  nameDialog.showModal();
  $("#name-input").select();
  return new Promise((resolve) => {
    nameDialog.addEventListener(
      "close",
      () => resolve({ action: nameDialog.returnValue || "cancel", name: $("#name-input").value }),
      { once: true },
    );
  });
}

$("#name-input").addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    nameDialog.close("save");
  }
});

/// Lets the user name, confirm or correct one face (from the viewer).
async function editFace(face) {
  const extra = face.suggested ? `Yes, ${face.person}` : face.person ? `Not ${face.person}` : "Not a face I know";
  const r = await askName({
    title: face.person ? (face.suggested ? `Is this ${face.person}?` : face.person) : "Who is this?",
    value: face.suggested ? "" : (face.person ?? ""),
    chip: face.id,
    extra: face.person || face.group != null ? extra : null,
  });
  try {
    if (r.action === "save" && r.name.trim()) await invoke("name_face", { face: face.id, name: r.name });
    else if (r.action === "extra" && face.suggested) await invoke("name_face", { face: face.id, name: face.person });
    else if (r.action === "extra") await invoke("unname_face", { face: face.id });
    else return false;
  } catch (e) {
    alert(e);
  }
  return true;
}

$("#people-nav").addEventListener("click", showPeople);
$("#person-back").addEventListener("click", showPeople);

// ---------- Face settings ----------

async function loadFaceSetup() {
  faceSetup = await invoke("face_setup");
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

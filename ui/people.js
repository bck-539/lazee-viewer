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
  $("#search").hidden = !photos;
  $("#filters-toggle").hidden = !photos;
  $("#grid-wrap").hidden = !photos;
  if (!photos) $("#filter-bar").hidden = true;
  $("#person-bar").hidden = !(photos && (state.person !== null || state.group !== null));
  $("#dupes").hidden = mode !== "dupes";
  $("#dupes-nav").classList.toggle("active", mode === "dupes");
}

function showGrid() {
  peopleView.hidden = true;
  const inPeople = state.person !== null || state.group !== null;
  $("#people-nav").classList.toggle("active", inPeople);
  if (inPeople)
    document.querySelectorAll("#sidebar nav .source[data-source]").forEach((x) => x.classList.remove("active"));
  setToolbar("photos");
}

function showPeopleView() {
  state.group = state.person = null;
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
  if (peopleView.dataset.view === "faces") return;
  const [people, groups, questions] = await Promise.all([
    loadNames(),
    invoke("face_groups"),
    invoke("same_person_questions").catch(() => []),
  ]).then(([p, g, q]) => [p, g, q.filter((x) => !skipped.has(`${x.group}:${x.other_group ?? "p" + x.person_id}`))]);
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
  if (questions.length) parts.push(questionCard(questions));
  if (people.length + groups.length > 1) parts.push(mergeBar());
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
          b.addEventListener("click", () => (merging ? pick(b, { person: p }) : openPerson(p)));
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
          b.addEventListener("click", () => (merging ? pick(b, { group: g }) : openGroup(g)));
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
          : "Nobody yet. Name tags already in your photos show up here after a scan. Turn on face finding in Settings to group photos by the faces in them.",
      }),
    );
  }
  peopleView.replaceChildren(...parts);
  peopleView.classList.toggle("merging", merging);
}

// ---------- Same person? ----------

/// Asks about one pair of look-alike groups at a time.
function questionCard(questions) {
  const q = questions[0];
  const chips = (faces) => el("div", { className: "chips" }, ...faces.map((f) => avatar(f)));
  const yes = el("button", { className: "primary", textContent: q.name ? `Yes, ${q.name}` : "Yes, same person" });
  const no = el("button", { textContent: "No" });
  const later = el("button", { textContent: "Ask later" });
  const card = el(
    "div",
    { className: "same-person" },
    el(
      "div",
      { className: "q-head" },
      el("h2", { textContent: q.name ? `Is this ${q.name}?` : "Are these the same person?" }),
      el("span", { className: "n", textContent: questions.length > 1 ? `${questions.length} questions` : "" }),
    ),
    el(
      "div",
      { className: "q-sides" },
      el(
        "div",
        { className: "q-side" },
        chips(q.faces),
        el("span", { className: "n", textContent: `${q.photos.toLocaleString()} photos` }),
      ),
      el("span", { className: "q-and", textContent: q.name ? "=" : "&" }),
      el(
        "div",
        { className: "q-side" },
        chips(q.other_faces),
        el("span", {
          className: "n",
          textContent: (q.name ? `${q.name}, ` : "") + `${q.other_photos.toLocaleString()} photos`,
        }),
      ),
    ),
    el("div", { className: "answer" }, yes, no, later),
  );
  const args = { group: q.group, otherGroup: q.other_group, person: q.person_id };
  const answer = async (cmd) => {
    card.classList.add("done");
    yes.disabled = no.disabled = later.disabled = true;
    if (cmd) await invoke(cmd, args).catch((e) => alert(e));
    else skipped.add(`${q.group}:${q.other_group ?? "p" + q.person_id}`);
    refreshPeople();
  };
  yes.addEventListener("click", () => answer("same_person"));
  no.addEventListener("click", () => answer("not_same_person"));
  later.addEventListener("click", () => answer(null));
  return card;
}

/// Questions put off with "Ask later", until the app is restarted.
const skipped = new Set();

// ---------- Merging ----------

let merging = false;
const picked = new Map();

function pick(button, item) {
  const key = item.person ? `p${item.person.id}` : `g${item.group.id}`;
  if (picked.has(key)) picked.delete(key);
  else picked.set(key, item);
  button.classList.toggle("picked", picked.has(key));
  updateMergeBar();
}

function mergeBar() {
  const start = el("button", { textContent: "Merge people…", title: "Pick people and groups that are one person" });
  const go = el("button", { className: "primary", id: "merge-go", textContent: "Merge" });
  const cancel = el("button", { textContent: "Cancel" });
  const label = el("span", { id: "merge-label" });
  start.addEventListener("click", () => {
    merging = true;
    picked.clear();
    refreshPeople();
  });
  cancel.addEventListener("click", () => {
    merging = false;
    picked.clear();
    refreshPeople();
  });
  go.addEventListener("click", mergePicked);
  const bar = merging
    ? el("div", { className: "merge-bar active" }, label, el("span", { className: "spacer" }), go, cancel)
    : el("div", { className: "merge-bar" }, el("span", { className: "spacer" }), start);
  queueMicrotask(updateMergeBar);
  return bar;
}

function updateMergeBar() {
  const label = $("#merge-label");
  if (!label) return;
  label.textContent = picked.size
    ? `${picked.size} picked. Pick everyone who is the same person, then Merge.`
    : "Click the people and groups that are the same person.";
  $("#merge-go").disabled = picked.size < 2;
}

async function mergePicked() {
  const items = [...picked.values()];
  const named = items.filter((i) => i.person).map((i) => i.person);
  const groups = items.filter((i) => i.group).map((i) => i.group.id);
  let name = null;
  if (named.length === 1) name = named[0].name;
  else {
    const r = await askName({
      title: named.length ? "Which name should they have?" : "Who is this?",
      value: named[0]?.name ?? "",
      note: named.length ? `Now named ${named.map((p) => p.name).join(", ")}.` : "",
      extra: named.length ? null : "Merge without a name",
    });
    if (r.action === "save" && r.name.trim()) name = r.name;
    else if (r.action !== "extra") return;
  }
  try {
    for (const p of named) await invoke("rename_person", { person: p.id, name });
    if (groups.length) {
      const keep = groups.length > 1 ? await invoke("merge_groups", { groups }) : groups[0];
      if (name) await invoke("name_group", { group: keep, name });
    }
  } catch (e) {
    alert(e);
  }
  merging = false;
  picked.clear();
  refreshPeople();
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
  personBar("Unnamed person", input, save, pickFacesButton({ group: g.id }, "Unnamed person"));
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
    const files = faceSetup?.write_back
      ? "Their name will also be taken out of the photo files."
      : "Tags inside the photo files are not changed.";
    if (!confirm(`Forget ${p.name}? Their faces become unnamed. ${files}`)) return;
    await invoke("delete_person", { person: p.id }).catch((e) => alert(e));
    showPeople();
  });
  const actions = [rename, forget, pickFacesButton({ person: p.id }, p.name, p)];
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

// ---------- Picking faces ----------

function pickFacesButton(of, title, person = null) {
  const b = el("button", { textContent: "Pick faces…", title: "Move single faces to someone else" });
  b.addEventListener("click", () => showFaces(of, title, person));
  return b;
}

/// Every face of a group or person, to move some of them to someone else.
async function showFaces(of, title, person = null) {
  const back = () => (person ? openPerson(person) : openGroup({ id: of.group }));
  const faces = await invoke("faces_of", { group: of.group ?? null, person: of.person ?? null });
  if (!faces.length) return showPeople();
  showPeopleView();
  state.group = of.group ?? null;
  state.person = of.person ?? null;
  peopleView.dataset.view = "faces";
  const chosen = new Set();
  let last = -1;
  const backBtn = el("button", { textContent: "‹ Photos" });
  backBtn.addEventListener("click", back);
  const all = el("button", { textContent: "Pick all" });
  const move = el("button", { className: "primary", textContent: "Move to…" });
  const notThem = el("button", { textContent: person ? `Not ${person.name}` : "Not this person" });
  const label = el("span", { className: "n" });
  const cells = faces.map((f, i) => {
    const b = el(
      "button",
      { className: "face-pick" + (f.suggested ? " suggested" : ""), title: f.suggested ? "Suggested" : "" },
      el("img", { src: faceUrl(f.id), alt: "", loading: "lazy", draggable: false, className: "avatar" }),
    );
    b.addEventListener("click", (e) => {
      const range = e.shiftKey && last >= 0 ? [Math.min(last, i), Math.max(last, i)] : [i, i];
      const on = !chosen.has(f.id);
      for (let k = range[0]; k <= range[1]; k++) {
        if (on) chosen.add(faces[k].id);
        else chosen.delete(faces[k].id);
        cells[k].classList.toggle("picked", on);
      }
      last = i;
      update();
    });
    return b;
  });
  const update = () => {
    label.textContent = chosen.size
      ? `${chosen.size} of ${faces.length} faces picked`
      : `${faces.length} faces. Click to pick, Shift+click for a range.`;
    move.disabled = notThem.disabled = !chosen.size;
    all.textContent = chosen.size === faces.length ? "Pick none" : "Pick all";
  };
  all.addEventListener("click", () => {
    const on = chosen.size !== faces.length;
    faces.forEach((f, k) => {
      if (on) chosen.add(f.id);
      else chosen.delete(f.id);
      cells[k].classList.toggle("picked", on);
    });
    update();
  });
  move.addEventListener("click", async () => {
    const r = await askName({ title: `Who are these ${chosen.size} faces?`, note: "Pick someone or type a new name." });
    if (r.action !== "save" || !r.name.trim()) return;
    await invoke("name_faces", { faces: [...chosen], name: r.name }).catch((e) => alert(e));
    showFaces(of, title, person);
  });
  notThem.addEventListener("click", async () => {
    await invoke("unname_faces", { faces: [...chosen] }).catch((e) => alert(e));
    showFaces(of, title, person);
  });
  update();
  peopleView.replaceChildren(
    el(
      "div",
      { className: "review-head" },
      backBtn,
      el("h2", { textContent: title }),
      label,
      el("span", { className: "spacer" }),
      all,
      notThem,
      move,
    ),
    el("div", { className: "faces-grid" }, ...cells),
  );
  $("#count").textContent = `${faces.length.toLocaleString()} faces`;
  peopleView.focus();
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
  $("#write-back").checked = s.write_back;
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
  const writeBack = $("#write-back").checked;
  if (writeBack !== s.write_back) {
    const ok =
      !writeBack ||
      confirm(
        "Save names into your photo files?\n\nLazee Viewer will change the name part of the photos' metadata " +
          "(the same place Windows Photo Gallery keeps it). The pictures themselves are not touched. " +
          "Make sure you have a backup of your photos.",
      );
    if (ok) await invoke("set_write_back", { enabled: writeBack }).catch((e) => alert(e));
  }
  const enabled = $("#faces-enabled").checked;
  const detector = $("#faces-detector").value || null;
  const embedder = $("#faces-embedder").value || null;
  const facesChanged = enabled !== s.enabled || (enabled && (detector !== s.detector || embedder !== s.embedder));
  if (!facesChanged) return;
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

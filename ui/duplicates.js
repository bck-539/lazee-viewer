// Duplicates: photos that are in more than one place. Each photo is a row:
// the copy to keep on the left, every other copy beside it, ticked when it
// is picked for deletion. Uses `invoke`, `el` and friends from main.js and
// people.js.
"use strict";

const dupesView = $("#dupes");

const dupes = {
  enabled: false,
  groups: [],
  folders: [],
  /// Group key -> id of the copy to keep.
  keep: new Map(),
  /// Ids of copies picked for deletion.
  picked: new Set(),
  /// Groups the user changed by hand; a reload keeps their choices.
  touched: new Set(),
  showExpected: false,
  shown: 100,
  status: "",
  problems: [],
};

const PAGE = 100;

function formatSize(bytes) {
  const units = ["bytes", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (bytes >= 1024 && i < units.length - 1) {
    bytes /= 1024;
    i++;
  }
  return `${i ? bytes.toFixed(bytes < 10 ? 1 : 0) : bytes} ${units[i]}`;
}

const formatDate = (iso) => iso.replace("T", " ").slice(0, 16);

const roleNames = { leading: "Leading folder", backup: "Backup folder" };

// ---------- Data ----------

async function loadDupes() {
  const [enabled, folders, groups] = await Promise.all([
    invoke("duplicates_enabled"),
    invoke("dup_folders"),
    invoke("duplicates"),
  ]);
  dupes.enabled = enabled;
  dupes.folders = folders;
  const keep = new Map();
  const picked = new Set();
  for (const g of groups) {
    const ids = new Set(g.copies.map((c) => c.id));
    if (dupes.touched.has(g.key) && ids.has(dupes.keep.get(g.key))) {
      keep.set(g.key, dupes.keep.get(g.key));
      for (const c of g.copies) if (dupes.picked.has(c.id)) picked.add(c.id);
    } else {
      dupes.touched.delete(g.key);
      keep.set(g.key, g.copies[0].id);
      for (const c of g.copies) if (c.suggested) picked.add(c.id);
    }
  }
  dupes.groups = groups;
  dupes.keep = keep;
  dupes.picked = picked;
}

/// Groups worth showing: all of them, or only those with copies outside
/// backup folders.
const visibleGroups = () => dupes.groups.filter((g) => dupes.showExpected || !g.expected || touchedPicks(g));

const touchedPicks = (g) => g.copies.some((c) => dupes.picked.has(c.id));

function keptCopy(g) {
  const id = dupes.keep.get(g.key);
  return g.copies.find((c) => c.id === id) ?? g.copies[0];
}

/// The kept copy first, then the others in their usual order.
function orderedCopies(g) {
  const kept = keptCopy(g);
  return [kept, ...g.copies.filter((c) => c !== kept)];
}

function setKeep(g, copy) {
  const old = keptCopy(g);
  if (old === copy) return;
  dupes.keep.set(g.key, copy.id);
  dupes.picked.delete(copy.id);
  // The old one is now just another copy: pick it, unless it is expected.
  if (old.role !== "backup" && old.online) dupes.picked.add(old.id);
  dupes.touched.add(g.key);
}

function togglePick(g, copy, on = !dupes.picked.has(copy.id)) {
  if (copy.id === keptCopy(g).id || !copy.online) return;
  if (on) dupes.picked.add(copy.id);
  else dupes.picked.delete(copy.id);
  dupes.touched.add(g.key);
}

function pickedTotals(groups = visibleGroups()) {
  let n = 0;
  let bytes = 0;
  let aside = 0;
  for (const g of groups)
    for (const c of g.copies)
      if (dupes.picked.has(c.id) && c.id !== keptCopy(g).id) {
        n++;
        bytes += c.size;
        if (c.no_recycle_bin) aside++;
      }
  return { n, bytes, aside };
}

// ---------- Page ----------

async function showDupes() {
  state.group = state.person = null;
  peopleView.hidden = true;
  document.querySelectorAll(".source").forEach((x) => x.classList.remove("active"));
  setToolbar("dupes");
  $("#info").textContent = "";
  dupes.shown = PAGE;
  await loadDupes();
  renderDupes();
  dupesView.focus();
}

/// Redraws the page if it is showing, keeping the scroll position.
async function refreshDupes() {
  if (dupesView.hidden) return;
  await loadDupes();
  renderDupes();
}

function renderDupes() {
  const top = dupesView.scrollTop;
  const groups = visibleGroups();
  const expected = dupes.groups.filter((g) => g.expected).length;
  const copies = dupes.groups.reduce((n, g) => n + g.copies.length - 1, 0);
  $("#count").textContent = dupes.enabled
    ? `${dupes.groups.length.toLocaleString()} photos with copies`
    : "";

  const parts = [
    el(
      "p",
      { className: "note intro" },
      "Photos count as copies when the picture is exactly the same, wherever they are and whatever they are called. " +
        "Metadata such as tags, dates and comments is ignored, so a copy that another program tagged still counts. " +
        "A RAW file and its JPEG are not copies.",
    ),
    folderCards(),
    findBar(),
  ];

  if (dupes.enabled && dupes.groups.length === 0) {
    parts.push(el("p", { className: "empty", textContent: "No copies found. Every photo is in one place." }));
  } else if (dupes.enabled) {
    parts.push(summaryBar(groups, expected, copies));
    parts.push(folderSummary(groups));
    parts.push(dupTable(groups));
    if (groups.length > dupes.shown) {
      const more = el("button", {
        className: "more",
        textContent: `Show ${Math.min(PAGE, groups.length - dupes.shown).toLocaleString()} more of ${(groups.length - dupes.shown).toLocaleString()}`,
      });
      more.addEventListener("click", () => {
        dupes.shown += PAGE;
        renderDupes();
      });
      parts.push(more);
    }
    parts.push(deleteBar(groups));
  }
  dupesView.replaceChildren(...parts);
  dupesView.scrollTop = top;
}

function folderCards() {
  const card = (role, title, text) => {
    const list = el("ul", { className: "role-list" });
    for (const f of dupes.folders.filter((f) => f.role === role)) {
      const rm = el("button", { className: "remove", textContent: "✕", title: "Forget this folder's role" });
      rm.addEventListener("click", async () => {
        await invoke("remove_dup_folder", { id: f.id });
        dupes.touched.clear();
        refreshDupes();
      });
      list.append(
        el(
          "li",
          { className: f.online ? "" : "offline", title: f.online ? f.path : `${f.path}\nThis drive is not connected.` },
          el("span", { className: "name", textContent: f.path }),
          rm,
        ),
      );
    }
    if (!list.children.length) list.append(el("li", { className: "none", textContent: "None yet" }));
    const add = el("button", { textContent: "Add folder…" });
    add.addEventListener("click", async () => {
      const path = await openDialog({ directory: true, multiple: false, title: `Choose a ${title.toLowerCase()}` });
      if (!path) return;
      try {
        await invoke("set_dup_folder", { path, role });
        dupes.touched.clear();
        await refreshDupes();
      } catch (e) {
        alert(e);
      }
    });
    return el(
      "section",
      { className: `role-card ${role}` },
      el("h3", { textContent: title }),
      el("p", { className: "note", textContent: text }),
      list,
      add,
    );
  };
  return el(
    "div",
    { className: "role-cards" },
    card(
      "leading",
      "Leading folders",
      "Your main photo folders. When a photo is here, this copy is the one kept, and copies elsewhere are picked for deletion.",
    ),
    card(
      "backup",
      "Backup folders",
      "Copies here are expected. They are never picked for deletion, and photos whose only extra copies are here are hidden.",
    ),
  );
}

function findBar() {
  const find = el("button", {
    className: dupes.enabled ? "" : "primary",
    textContent: dupes.enabled ? "Look again" : "Find duplicates",
  });
  find.addEventListener("click", async () => {
    try {
      await invoke("find_duplicates");
      dupes.status = state.scanning ? "Waiting for the scan to finish…" : "Looking for duplicates…";
    } catch (e) {
      dupes.status = String(e);
    }
    renderDupes();
  });
  const bar = el("div", { className: "find-bar" }, find, el("span", { id: "dupes-status", textContent: dupes.status }));
  if (!dupes.enabled)
    bar.append(
      el("p", {
        className: "note",
        textContent:
          "The first search reads the end of every photo, and then compares possible copies in full. On a network " +
          "drive this can take a while; it runs in the background like a scan. After that, new photos are checked as they are scanned.",
      }),
    );
  if (dupes.problems.length) {
    const d = el("details", { className: "problems" }, el("summary", { textContent: "What could not be deleted" }));
    d.append(el("ul", {}, ...dupes.problems.map(([path, why]) => el("li", { textContent: `${path}: ${why}` }))));
    bar.append(d);
  }
  return bar;
}

function summaryBar(groups, expected, copies) {
  const t = pickedTotals(groups);
  const showExp = el("input", { type: "checkbox", checked: dupes.showExpected });
  showExp.addEventListener("change", () => {
    dupes.showExpected = showExp.checked;
    renderDupes();
  });
  const suggested = el("button", { textContent: "Pick suggested", title: "Pick every copy outside the leading and backup rules again" });
  suggested.addEventListener("click", () => {
    dupes.touched.clear();
    dupes.keep = new Map(dupes.groups.map((g) => [g.key, g.copies[0].id]));
    dupes.picked = new Set(dupes.groups.flatMap((g) => g.copies.filter((c) => c.suggested).map((c) => c.id)));
    renderDupes();
  });
  const none = el("button", { textContent: "Pick none" });
  none.addEventListener("click", () => {
    for (const g of dupes.groups) dupes.touched.add(g.key);
    dupes.picked.clear();
    renderDupes();
  });
  return el(
    "div",
    { className: "dupes-summary" },
    el("span", {
      textContent:
        `${dupes.groups.length.toLocaleString()} photos have ${copies.toLocaleString()} extra copies. ` +
        `${t.n.toLocaleString()} picked to delete (${formatSize(t.bytes)}).`,
    }),
    el("span", { className: "spacer" }),
    expected
      ? el("label", { className: "check" }, showExp, ` Show ${expected.toLocaleString()} only copied to backup folders`)
      : "",
    suggested,
    none,
  );
}

/// Every folder that holds copies, with quick ways to pick or keep them all.
function folderSummary(groups) {
  const byFolder = new Map();
  for (const g of groups) {
    const kept = keptCopy(g);
    for (const c of g.copies) {
      let f = byFolder.get(c.folder);
      if (!f) byFolder.set(c.folder, (f = { folder: c.folder, role: c.role, copies: 0, kept: 0, picked: 0 }));
      f.copies++;
      if (c === kept) f.kept++;
      else if (dupes.picked.has(c.id)) f.picked++;
    }
  }
  const rows = [...byFolder.values()].sort((a, b) => b.copies - a.copies || a.folder.localeCompare(b.folder));
  const table = el(
    "table",
    { className: "folder-table" },
    el(
      "thead",
      {},
      el(
        "tr",
        {},
        el("th", { textContent: "Folder" }),
        el("th", { textContent: "Photos" }),
        el("th", { textContent: "Kept" }),
        el("th", { textContent: "To delete" }),
        el("th"),
      ),
    ),
  );
  const body = el("tbody");
  for (const f of rows) {
    const pickAll = el("button", { textContent: "Delete all here", title: "Pick every copy in this folder that isn't kept" });
    pickAll.addEventListener("click", () => {
      for (const g of groups)
        for (const c of g.copies) if (c.folder === f.folder && c !== keptCopy(g)) togglePick(g, c, true);
      renderDupes();
    });
    const keepAll = el("button", { textContent: "Keep all here", title: "Keep the copy in this folder for every photo here" });
    keepAll.addEventListener("click", () => {
      for (const g of groups) {
        const here = g.copies.filter((c) => c.folder === f.folder);
        if (here.length && !here.includes(keptCopy(g))) setKeep(g, here[0]);
        for (const c of here) if (c !== keptCopy(g)) togglePick(g, c, false);
      }
      renderDupes();
    });
    const badge = roleNames[f.role] ? el("span", { className: `role ${f.role}`, textContent: roleNames[f.role] }) : "";
    body.append(
      el(
        "tr",
        {},
        el("td", { className: "path" }, f.folder, " ", badge),
        el("td", { textContent: f.copies.toLocaleString() }),
        el("td", { textContent: f.kept.toLocaleString() }),
        el("td", { textContent: f.picked.toLocaleString() }),
        el("td", { className: "actions" }, keepAll, pickAll),
      ),
    );
  }
  table.append(body);
  return el(
    "details",
    { className: "folder-summary", open: rows.length <= 12 },
    el("summary", { textContent: `Where the copies are: ${rows.length.toLocaleString()} folders` }),
    table,
  );
}

function dupTable(groups) {
  const table = el(
    "div",
    { className: "dupe-table" },
    el(
      "div",
      { className: "dupe-header" },
      el("span", { className: "keep-col", textContent: "Kept" }),
      el("span", { textContent: "Copies (ticked ones get deleted)" }),
    ),
  );
  for (const g of groups.slice(0, dupes.shown)) table.append(dupRow(g));
  return table;
}

function dupRow(g) {
  const row = el("div", { className: "dupe-row" });
  const kept = keptCopy(g);
  for (const c of orderedCopies(g)) row.append(copyCard(g, c, c === kept));
  return row;
}

function copyCard(g, c, kept) {
  const picked = !kept && dupes.picked.has(c.id);
  const card = el("div", {
    className: `dupe-card${kept ? " kept" : ""}${picked ? " picked" : ""}${c.online ? "" : " offline"}`,
    title: c.path ?? `${c.folder}\\${c.name}\nThis drive is not connected.`,
  });
  const img = el("img", { src: convertFileSrc(String(c.id), "thumb"), alt: "", draggable: false, loading: "lazy" });
  const head = el("div", { className: "card-head" });
  if (kept) {
    head.append(el("strong", { className: "keep-label", textContent: "Keep" }));
  } else {
    const box = el("input", { type: "checkbox", checked: picked, disabled: !c.online });
    box.addEventListener("change", () => {
      togglePick(g, c, box.checked);
      renderDupes();
    });
    head.append(el("label", { className: "check" }, box, " Delete"));
    img.addEventListener("click", () => {
      togglePick(g, c);
      renderDupes();
    });
  }
  if (roleNames[c.role]) head.append(el("span", { className: `role ${c.role}`, textContent: roleNames[c.role] }));
  if (!c.online) head.append(el("span", { className: "role offline", textContent: "Not connected" }));

  const facts = [formatSize(c.size), `changed ${formatDate(c.modified)}`];
  if (kept && g.taken_at) facts.unshift(`taken ${formatDate(g.taken_at)}`);
  if (kept && g.width && g.height) facts.push(`${g.width} × ${g.height}`);
  if (g.kind === "raw") facts.push("RAW");
  card.append(
    img,
    head,
    el("div", { className: "folder", textContent: c.folder }),
    el("div", { className: "file", textContent: c.name }),
    el("div", { className: "facts", textContent: facts.join(" · ") }),
  );
  if (!kept && c.online) {
    const keepThis = el("button", { className: "keep-this", textContent: "Keep this one instead" });
    keepThis.addEventListener("click", () => {
      setKeep(g, c);
      renderDupes();
    });
    card.append(keepThis);
  }
  return card;
}

function deleteBar(groups) {
  const t = pickedTotals(groups);
  const go = el("button", {
    className: "primary danger",
    disabled: t.n === 0,
    textContent: t.n ? `Delete ${t.n.toLocaleString()} picked copies (${formatSize(t.bytes)})` : "Nothing picked",
  });
  go.addEventListener("click", () => deletePicked(groups));
  return el(
    "div",
    { className: "delete-bar" },
    el("span", {
      className: "note",
      textContent: "Deleted copies go to the Recycle Bin. The copy marked Keep of each photo always stays.",
    }),
    el("span", { className: "spacer" }),
    go,
  );
}

async function deletePicked(groups) {
  const ids = [];
  for (const g of groups) {
    const kept = keptCopy(g);
    for (const c of g.copies) if (c !== kept && dupes.picked.has(c.id)) ids.push(c.id);
  }
  const t = pickedTotals(groups);
  if (!t.n) return;
  let text = `Delete ${t.n.toLocaleString()} copies (${formatSize(t.bytes)})?\n\n`;
  text += "They go to the Recycle Bin, so you can get them back from there. ";
  if (t.n > Math.min(groups.length, dupes.shown)) text += "This includes copies further down the list. ";
  if (t.aside)
    text +=
      `\n\n${t.aside.toLocaleString()} of them are on network or USB drives, which have no Recycle Bin. Those are moved ` +
      `into a ".Deleted by Lazee Viewer" folder inside their photo folder instead; delete that folder yourself once you're sure.`;
  text += "\n\nThe copy marked Keep of each photo stays where it is.";
  if (!confirm(text)) return;
  dupes.status = "Deleting…";
  dupes.problems = [];
  renderDupes();
  try {
    const r = await invoke("delete_duplicates", { ids });
    const done = r.recycled + r.moved_aside;
    let s = `Deleted ${done.toLocaleString()} copies, ${formatSize(r.bytes)} freed.`;
    if (r.moved_aside) s += ` ${r.moved_aside.toLocaleString()} were moved to: ${r.aside_folders.join(", ")}.`;
    if (r.names_kept) s += ` ${r.names_kept.toLocaleString()} face names were moved to the copies you kept.`;
    if (r.failed.length) s += ` ${r.failed.length.toLocaleString()} could not be deleted.`;
    dupes.status = s;
    dupes.problems = r.failed;
  } catch (e) {
    dupes.status = `Could not delete: ${e}`;
  }
  await Promise.all([refreshDupes(), loadSources()]);
}

// ---------- Background search ----------

listen("dups-started", () => {
  $("#scan-status").textContent = "Looking for duplicates…";
  dupes.status = "Looking for duplicates…";
  if (!dupesView.hidden) $("#dupes-status").textContent = dupes.status;
});

listen("dup-progress", ({ payload: p }) => {
  const bar = $("#scan-bar");
  bar.max = Math.max(1, p.total);
  bar.value = p.done;
  const text =
    p.phase === "quick"
      ? `Looking for duplicates: checked ${p.done.toLocaleString()} of ${p.total.toLocaleString()} photos`
      : `Comparing possible copies: ${p.done.toLocaleString()} of ${p.total.toLocaleString()}`;
  $("#scan-status").textContent = text;
  dupes.status = text;
  if (!dupesView.hidden) $("#dupes-status").textContent = text;
});

listen("dups-finished", async ({ payload: result }) => {
  if (result.Err) {
    dupes.status = `Could not look for duplicates: ${result.Err}`;
  } else {
    const s = result.Ok;
    dupes.status = s.cancelled
      ? "Search for duplicates stopped."
      : `${s.groups.toLocaleString()} photos have ${s.copies.toLocaleString()} extra copies.`;
    if (s.failed) dupes.status += ` ${s.failed.toLocaleString()} files could not be read.`;
  }
  $("#scan-status").textContent = dupes.status;
  refreshDupes();
});

$("#dupes-nav").addEventListener("click", showDupes);

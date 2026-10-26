// Search and the timeline. Uses `state`, `invoke`, `loadPhotos`, `layout`
// and friends from main.js, and `peopleCache` from people.js.
"use strict";

const search = {
  text: "",
  people: [], // { id, name }
  any: false,
  from: "",
  to: "",
};

/// The search part of the photo filter.
function searchFilter() {
  return {
    text: search.text || null,
    people: search.people.map((p) => p.id),
    any_person: search.any,
    from: search.from || null,
    to: search.to || null,
  };
}

function searching() {
  return !!(search.text || search.people.length || search.from || search.to);
}

let searchTimer = 0;
function searchChanged() {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => loadPhotos(), 200);
  $("#filters-toggle").classList.toggle("on", !!(search.people.length || search.from || search.to));
}

$("#search").addEventListener("input", (e) => {
  search.text = e.target.value.trim();
  searchChanged();
});

$("#search").addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    e.target.value = search.text = "";
    searchChanged();
    grid.focus();
  }
});

$("#filters-toggle").addEventListener("click", async () => {
  const bar = $("#filter-bar");
  bar.hidden = !bar.hidden;
  if (!bar.hidden) {
    await loadNames();
    $("#filter-person").focus();
  }
});

function renderFilterPeople() {
  $("#filter-people").replaceChildren(
    ...search.people.map((p) => {
      const chip = el("button", { className: "chip", title: "Remove" }, p.name, el("span", { textContent: " ✕" }));
      chip.addEventListener("click", () => {
        search.people = search.people.filter((x) => x.id !== p.id);
        renderFilterPeople();
        searchChanged();
      });
      return chip;
    }),
  );
  $("#filter-mode").hidden = search.people.length < 2;
}

function addFilterPerson() {
  const input = $("#filter-person");
  const name = input.value.trim().toLowerCase();
  const p = peopleCache.find((x) => x.name.toLowerCase() === name);
  if (!p) return;
  if (!search.people.some((x) => x.id === p.id)) search.people.push({ id: p.id, name: p.name });
  input.value = "";
  renderFilterPeople();
  searchChanged();
}

// Picking from the list fires "change"; typing a full name and Enter works too.
$("#filter-person").addEventListener("change", addFilterPerson);
$("#filter-person").addEventListener("keydown", (e) => e.key === "Enter" && addFilterPerson());
$("#filter-mode").addEventListener("change", (e) => {
  search.any = e.target.value === "any";
  searchChanged();
});
$("#filter-from").addEventListener("change", (e) => {
  search.from = e.target.value;
  searchChanged();
});
$("#filter-to").addEventListener("change", (e) => {
  search.to = e.target.value;
  searchChanged();
});
$("#filter-clear").addEventListener("click", () => {
  Object.assign(search, { text: "", people: [], any: false, from: "", to: "" });
  $("#search").value = $("#filter-from").value = $("#filter-to").value = "";
  $("#filter-mode").value = "all";
  renderFilterPeople();
  searchChanged();
});
renderFilterPeople();

// ---------- Timeline ----------
// Years down the right edge of the grid, placed where their photos are.
// Click or drag to jump; hovering shows the month.

const timeline = $("#timeline");
const timelineTip = $("#timeline-tip");
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function monthName(m) {
  const [y, mo] = m.split("-");
  return `${MONTHS[Number(mo) - 1] ?? ""} ${y}`;
}

function renderTimeline() {
  const months = state.months ?? [];
  const show = months.length > 1 && state.ids.length > 0;
  timeline.hidden = !show;
  grid.classList.toggle("with-timeline", show);
  if (!show) return;
  const total = state.ids.length;
  const labels = [];
  let lastYear = null;
  let lastTop = -100;
  for (const m of months) {
    const year = m.month.slice(0, 4);
    if (year === lastYear) continue;
    lastYear = year;
    const top = (m.start / total) * 100;
    // Skip years that would overlap the one above.
    if (((top - lastTop) / 100) * timeline.clientHeight < 16) continue;
    lastTop = top;
    labels.push(el("span", { className: "year", textContent: year, style: `top: ${top}%` }));
  }
  timeline.replaceChildren(timelineTip, ...labels);
}

function monthAt(clientY) {
  const r = timeline.getBoundingClientRect();
  const index = Math.floor(((clientY - r.top) / r.height) * state.ids.length);
  const i = Math.max(0, Math.min(state.ids.length - 1, index));
  const m = state.months.findLast((x) => x.start <= i) ?? state.months[0];
  return { m, top: clientY - r.top };
}

function jumpTo(clientY) {
  const { m } = monthAt(clientY);
  const step = layout.size + GAP;
  grid.scrollTop = Math.floor(m.start / layout.cols) * step;
}

let timelineDrag = false;
timeline.addEventListener("pointerdown", (e) => {
  timelineDrag = true;
  timeline.setPointerCapture(e.pointerId);
  jumpTo(e.clientY);
});
timeline.addEventListener("pointermove", (e) => {
  const { m, top } = monthAt(e.clientY);
  timelineTip.hidden = false;
  timelineTip.textContent = monthName(m.month);
  timelineTip.style.top = `${top}px`;
  if (timelineDrag) jumpTo(e.clientY);
});
timeline.addEventListener("pointerup", () => (timelineDrag = false));
timeline.addEventListener("pointerleave", () => {
  if (!timelineDrag) timelineTip.hidden = true;
});
new ResizeObserver(() => renderTimeline()).observe(timeline);

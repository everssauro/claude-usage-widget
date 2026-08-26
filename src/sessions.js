const { invoke } = window.__TAURI__.core;

// Windows are derived from Anthropic's OWN reset timestamps (the same ones the
// widget's bars use), never from a locally reconstructed block: ccusage's 5h
// block was measured starting 9m23s away from the real one, which misfiled 7%
// of the window's requests.
// The two LIMIT windows are anchored on Anthropic's own resets so the table and
// the widget's bars can never disagree. The two BILLING windows are calendar
// periods — "who spent what this month" is the question you actually invoice on,
// and a 5h block can't answer it.
const WINDOWS = {
  current: { kind: "reset", secs: 5 * 3600, resetKey: "current_reset_min" },
  weekly: { kind: "reset", secs: 7 * 86400, resetKey: "weekly_reset_min" },
  month: { kind: "calendar", offset: 0 },
  lastMonth: { kind: "calendar", offset: -1 },
  all: { kind: "all" },
};

/// [start, end] in unix seconds for the selected window.
function windowBounds(name, usage) {
  const now = Date.now() / 1000;
  const w = WINDOWS[name];
  if (w.kind === "all") return [0, now];
  if (w.kind === "calendar") {
    const d = new Date();
    const start = new Date(d.getFullYear(), d.getMonth() + w.offset, 1);
    // A past month is a CLOSED period — it must end when the month ended, not
    // "now", or last month's invoice would keep absorbing this month's usage.
    const end =
      w.offset === 0 ? now : new Date(d.getFullYear(), d.getMonth(), 1).getTime() / 1000;
    return [start.getTime() / 1000, end];
  }
  const end = now + usage[w.resetKey] * 60;
  return [end - w.secs, end];
}

// $/M tokens, applied to OUR exact token counts. Deliberately local constants
// rather than ccusage's figure: that one comes from a third-party price table
// fetched at runtime, and returns $0.00 offline (measured: $342.93 online vs
// $0.00 with --offline on identical data). Cache reads bill at ~10% of input,
// cache writes at ~125%.
const RATES = {
  opus: { input: 15, output: 75 },
  sonnet: { input: 3, output: 15 },
  haiku: { input: 1, output: 5 },
};
const rateFor = (model) => {
  const m = (model || "").toLowerCase();
  if (m.includes("haiku")) return RATES.haiku;
  if (m.includes("sonnet")) return RATES.sonnet;
  return RATES.opus; // opus / fable / unknown — the expensive assumption, on purpose
};

// Subscription price by plan, mirroring the widget's settings.
const PLANS = { pro: 20, max5: 100, max20: 200 };

// Models that spend usage CREDITS rather than the plan's windows. Anthropic is
// explicit about this in the client: "Fable 5 is now using usage credits instead
// of your plan limits". Keeping them out of the plan split matters for billing —
// otherwise a client who used Fable inflates their share of a subscription their
// Fable work never touched.
const CREDIT_MODEL = /fable|mythos/i;
const drawsCredits = (model) => CREDIT_MODEL.test(model || "");

const UNGROUPED = "__ungrouped__";

const el = {};
const state = {
  // { groups: [{id,name}], projects: {path: groupId} }
  groups: { groups: [], projects: {} },
  window: "current",
  sort: "output",
  filter: "",
  expanded: new Set(),
  collapsed: new Set(), // collapsed GROUP bands
  expandAll: false,
  data: null,
};

const fmtBig = (n) => {
  if (n >= 1e9) return (n / 1e9).toFixed(2).replace(/\.?0+$/, "") + "B";
  if (n >= 1e6) return (n / 1e6).toFixed(1).replace(/\.0$/, "") + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(1).replace(/\.0$/, "") + "k";
  return String(n);
};
const fmtMoney = (v) => (v >= 1000 ? `$${Math.round(v)}` : `$${v.toFixed(2)}`);
const fmtMin = (m) => {
  if (m < 1) return "—";
  const h = Math.floor(m / 60);
  return h > 0 ? `${h}h ${Math.round(m % 60)}m` : `${Math.round(m)}m`;
};
const fmtClock = (unix) =>
  new Date(unix * 1000).toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });

/// API-equivalent cost for a project, priced per model (each model has its own
/// rate, so a single blended number would be wrong whenever the mix shifts).
function estCost(models) {
  let usd = 0;
  for (const m of models || []) {
    const r = rateFor(m.model);
    const t = m.tokens;
    usd +=
      ((t.input + t.cache_write * 1.25 + t.cache_read * 0.1) * r.input) / 1e6 +
      (t.output * r.output) / 1e6;
  }
  return usd;
}

const SORTS = {
  output: (p) => p.tokens.output,
  input: (p) => p.tokens.input,
  cacheWrite: (p) => p.tokens.cache_write,
  cacheRead: (p) => p.tokens.cache_read,
  requests: (p) => p.requests,
  active: (p) => p.active_min,
  sessions: (p) => p.sessions.length,
  fable: (p) => p._fable,
  cost: (p) => p._cost,
  allocated: (p) => p._share,
};

function td(text, cls) {
  const c = document.createElement("td");
  c.className = cls || "c-num";
  c.textContent = text;
  return c;
}

// ---------------------------------------------------------------------------
// Drag a project row onto a group band.
//
// Mouse events, NOT the HTML5 drag-and-drop API: wry installs a file-drop
// handler on the webview, and letting the OS arbitrate an in-page drag is a
// fight not worth having. This also keeps the row's click-to-expand intact —
// a drag only starts once the pointer has actually travelled.
// ---------------------------------------------------------------------------
const DRAG_THRESHOLD_PX = 5;
const drag = { path: null, name: "", x: 0, y: 0, active: false, ghost: null, over: null, suppressClick: false };

function beginDrag(e, p) {
  if (e.button !== 0 || e.target.closest("button, input, select")) return;
  drag.path = p.path;
  drag.name = p.name;
  drag.x = e.clientX;
  drag.y = e.clientY;
  drag.active = false;
  document.addEventListener("mousemove", onDragMove);
  document.addEventListener("mouseup", endDrag);
}

function startVisualDrag() {
  drag.active = true;
  document.body.classList.add("dragging");
  const g = document.createElement("div");
  g.className = "drag-ghost";
  g.textContent = drag.name;
  document.body.append(g);
  drag.ghost = g;
}

function highlight(groupId) {
  if (drag.over === groupId) return;
  for (const r of el.rows.querySelectorAll(".drop-target")) r.classList.remove("drop-target");
  drag.over = groupId;
  if (groupId == null) return;
  // Highlight the BAND, wherever the pointer actually is — dropping on a
  // sibling project means "same group as that one".
  const band = el.rows.querySelector(`tr.g-row[data-group="${CSS.escape(groupId)}"]`);
  if (band) band.classList.add("drop-target");
}

function onDragMove(e) {
  if (!drag.path) return;
  if (!drag.active) {
    if (Math.hypot(e.clientX - drag.x, e.clientY - drag.y) < DRAG_THRESHOLD_PX) return;
    startVisualDrag();
  }
  drag.ghost.style.transform = `translate(${e.clientX + 12}px, ${e.clientY + 10}px)`;
  const row = document.elementFromPoint(e.clientX, e.clientY)?.closest("tr[data-group]");
  highlight(row ? row.dataset.group : null);
}

function endDrag() {
  document.removeEventListener("mousemove", onDragMove);
  document.removeEventListener("mouseup", endDrag);
  const { path, active, over } = drag;
  drag.ghost?.remove();
  document.body.classList.remove("dragging");
  for (const r of el.rows.querySelectorAll(".drop-target")) r.classList.remove("drop-target");
  drag.ghost = null;
  drag.path = null;
  drag.over = null;
  if (!active) return;
  // The mouseup that ends a drag also fires a click on the row; swallow it so
  // the project doesn't expand as a side effect of being moved.
  drag.suppressClick = true;
  setTimeout(() => (drag.suppressClick = false), 0);
  drag.active = false;
  if (over && path) assignProject(path, over);
}

/// The full path is unreadable in a table cell and starves the NAME of space —
/// which is how every project once rendered as "h…", "clau…", "cha…". Show the
/// tail only; the full path stays in the row's tooltip.
function shortPath(path) {
  let t = path;
  if (t.startsWith("/Users/")) t = "~/" + t.split("/").slice(3).join("/");
  const parent = t.split("/").slice(0, -1); // drop the leaf: it's already the name
  return parent.length > 3 ? "…/" + parent.slice(-2).join("/") : parent.join("/");
}

function projectRow(p) {
  const tr = document.createElement("tr");
  tr.className = "p-row";
  const name = document.createElement("td");
  name.className = "c-name";
  const wrap = document.createElement("div");
  wrap.className = "name-cell";
  const tw = document.createElement("span");
  tw.className = "twisty";
  tw.textContent = state.expandAll || state.expanded.has(p.path) ? "▾" : "▸";
  const nm = document.createElement("span");
  nm.className = "p-name";
  nm.textContent = p.name;
  const pa = document.createElement("span");
  pa.className = "p-path";
  pa.textContent = shortPath(p.path);
  wrap.append(tw, nm, pa);
  name.append(wrap);
  name.title = p.path;

  tr.dataset.group = p._group;
  tr.dataset.path = p.path;
  tr.append(
    name,
    td(fmtMoney(p._alloc)),
    td("~" + fmtMoney(p._cost), "c-num muted"),
    td(p._fable ? fmtBig(p._fable) : "—", "c-num credit"),
    td(fmtBig(p.tokens.output)),
    td(fmtBig(p.tokens.input)),
    td(fmtBig(p.tokens.cache_write)),
    td(fmtBig(p.tokens.cache_read)),
    td(String(p.requests)),
    td(fmtMin(p.active_min)),
    td(String(p.sessions.length)),
  );
  tr.addEventListener("mousedown", (e) => beginDrag(e, p));
  tr.addEventListener("click", () => {
    if (drag.suppressClick) return; // that click was the end of a drag
    try {
      state.expanded.has(p.path) ? state.expanded.delete(p.path) : state.expanded.add(p.path);
      render();
    } catch (err) {
      // Never fail silently: a dead row with no explanation is unfixable from
      // the outside (this window has no devtools in a release build).
      el.errMsg.textContent = `row click: ${err}`;
      el.content.dataset.state = "error";
    }
  });
  return tr;
}

function sessionRow(s) {
  const sr = document.createElement("tr");
  sr.className = "s-row";
  const sname = document.createElement("td");
  sname.className = "c-name";
  const t = document.createElement("span");
  t.className = "s-title";
  t.textContent = s.title || s.session_id.slice(0, 8);
  const sub = document.createElement("span");
  sub.className = "s-sub";
  sub.textContent =
    ` · ${fmtClock(s.last_ts)}` +
    (s.subagent_requests ? ` · +${s.subagent_requests} subagent` : "");
  sname.append(t, sub);
  sr.append(
    sname,
    td(""),
    td(""),
    td(""),
    td(fmtBig(s.tokens.output)),
    td(fmtBig(s.tokens.input)),
    td(fmtBig(s.tokens.cache_write)),
    td(fmtBig(s.tokens.cache_read)),
    td(String(s.requests)),
    td(fmtMin(s.active_min)),
    td(""),
  );
  return sr;
}

/// A group's own row: the number you read when asking "what did SlimPass cost
/// me this month". Totals SUM across projects; active time is deliberately NOT
/// summed here (concurrent projects would report more hours than elapsed) —
/// only the grand total unions, so per-group time is left blank rather than
/// printed wrong.
function groupRow(id, members, collapsed) {
  const tr = document.createElement("tr");
  tr.className = "g-row";
  tr.dataset.group = id;
  const sum = (f) => members.reduce((a, p) => a + f(p), 0);

  const name = document.createElement("td");
  name.className = "c-name";
  const wrap = document.createElement("div");
  wrap.className = "name-cell";
  const tw = document.createElement("span");
  tw.className = "twisty";
  tw.textContent = collapsed ? "▸" : "▾";
  const nm = document.createElement("span");
  nm.className = "g-name";
  nm.textContent = groupName(id);
  const cnt = document.createElement("span");
  cnt.className = "p-path";
  cnt.textContent = `${members.length} project${members.length === 1 ? "" : "s"}`;
  wrap.append(tw, nm, cnt);
  if (id !== UNGROUPED) {
    const ren = document.createElement("button");
    ren.className = "mini-btn";
    ren.textContent = "rename";
    ren.addEventListener("click", (e) => {
      e.stopPropagation();
      openGroupInput({ mode: "rename", id }, groupName(id));
    });
    // Two-step delete instead of confirm(): this webview has no confirm dialog,
    // and a one-click destructive control would be a trap.
    const del = document.createElement("button");
    del.className = "mini-btn danger";
    del.textContent = "×";
    del.title = "delete group (click twice)";
    del.addEventListener("click", (e) => {
      e.stopPropagation();
      if (del.dataset.armed === "1") return deleteGroup(id);
      del.dataset.armed = "1";
      del.textContent = "sure?";
      setTimeout(() => {
        del.dataset.armed = "";
        del.textContent = "×";
      }, 2500);
    });
    wrap.append(ren, del);
  }
  name.append(wrap);

  tr.append(
    name,
    td(fmtMoney(sum((p) => p._alloc))),
    td("~" + fmtMoney(sum((p) => p._cost)), "c-num muted"),
    td(sum((p) => p._fable) ? fmtBig(sum((p) => p._fable)) : "—", "c-num credit"),
    td(fmtBig(sum((p) => p.tokens.output))),
    td(fmtBig(sum((p) => p.tokens.input))),
    td(fmtBig(sum((p) => p.tokens.cache_write))),
    td(fmtBig(sum((p) => p.tokens.cache_read))),
    td(String(sum((p) => p.requests))),
    td("—"),
    td(String(sum((p) => p.sessions.length))),
  );
  tr.addEventListener("click", () => {
    state.collapsed.has(id) ? state.collapsed.delete(id) : state.collapsed.add(id);
    render();
  });
  return tr;
}

function render() {
  const d = state.data;
  if (!d) return;
  const q = state.filter.trim().toLowerCase();
  const projects = d.projects.filter(
    (p) => !q || p.name.toLowerCase().includes(q) || p.path.toLowerCase().includes(q),
  );

  const key = SORTS[state.sort] || SORTS.output;
  projects.sort((a, b) => key(b) - key(a));

  // Resolve membership HERE, not at load time: assigning a project changes the
  // mapping without refetching, and reading a stale `_group` was why picking a
  // group saved to disk but left the row sitting in its old band.
  for (const p of projects) p._group = state.groups.projects[p.path] || UNGROUPED;

  // Bucket by group. Ungrouped always sits last: it's the inbox, not a client.
  const buckets = new Map();
  for (const p of projects) {
    if (!buckets.has(p._group)) buckets.set(p._group, []);
    buckets.get(p._group).push(p);
  }
  // Every defined group gets a band even when empty — an empty group with no
  // band would be an impossible drop target, and Ungrouped is the only way to
  // drag a project back out.
  const anyGroups = state.groups.groups.length > 0;
  for (const g of state.groups.groups) if (!buckets.has(g.id)) buckets.set(g.id, []);
  if (anyGroups && !buckets.has(UNGROUPED)) buckets.set(UNGROUPED, []);
  const order = state.groups.groups
    .map((g) => g.id)
    .concat(buckets.has(UNGROUPED) ? [UNGROUPED] : []);

  el.rows.replaceChildren();
  for (const id of order) {
    const members = buckets.get(id);
    const collapsed = state.collapsed.has(id);
    // With no groups defined at all, skip the header entirely — a single
    // "Ungrouped" band over every row is noise.
    if (anyGroups) el.rows.append(groupRow(id, members, collapsed));
    if (anyGroups && collapsed) continue;
    for (const p of members) {
      el.rows.append(projectRow(p));
      if (!state.expandAll && !state.expanded.has(p.path)) continue;
      for (const s of p.sessions) el.rows.append(sessionRow(s));
    }
  }

  // Totals are over the FILTERED set so the footer always matches what's shown.
  const sum = (f) => projects.reduce((a, p) => a + f(p), 0);
  el.tAllocated.textContent = fmtMoney(sum((p) => p._alloc));
  el.tCost.textContent = "~" + fmtMoney(sum((p) => p._cost));
  el.tFable.textContent = sum((p) => p._fable) ? fmtBig(sum((p) => p._fable)) : "—";
  el.tOutput.textContent = fmtBig(sum((p) => p.tokens.output));
  el.tInput.textContent = fmtBig(sum((p) => p.tokens.input));
  el.tCacheWrite.textContent = fmtBig(sum((p) => p.tokens.cache_write));
  el.tCacheRead.textContent = fmtBig(sum((p) => p.tokens.cache_read));
  el.tRequests.textContent = String(sum((p) => p.requests));
  // Active time is a UNION at the top level, never a sum — concurrent sessions
  // would otherwise report more hours than actually elapsed.
  el.tActive.textContent = q ? "—" : fmtMin(state.data.active_min);
  el.tSessions.textContent = String(sum((p) => p.sessions.length));

  for (const th of document.querySelectorAll("th[data-sort]"))
    th.classList.toggle("sorted", th.dataset.sort === state.sort);

  el.content.dataset.state = d.projects.length ? "ok" : "empty";
}

// ---------------------------------------------------------------------------
// Groups — user-named folders ("SlimPass", "Ton", "Saggezza") over projects.
// Assignment is at PROJECT level on purpose: sessions are numerous and
// ephemeral, projects are few and stable, and every session of a project
// inherits its folder. Persisted to disk (groups.json) via Rust, not
// localStorage — this mapping is what turns folders into who-owes-what.
// ---------------------------------------------------------------------------
async function loadGroups() {
  try {
    const raw = await invoke("get_groups");
    if (raw) {
      const g = JSON.parse(raw);
      state.groups = { groups: g.groups || [], projects: g.projects || {} };
    }
  } catch {
    /* first run, or unreadable — start empty rather than block the table */
  }
}
async function saveGroups() {
  try {
    await invoke("save_groups", { json: JSON.stringify(state.groups) });
  } catch (e) {
    el.errMsg.textContent = `couldn't save groups: ${e}`;
    el.content.dataset.state = "error";
  }
}
const groupName = (id) =>
  (state.groups.groups.find((g) => g.id === id) || {}).name || "Ungrouped";

// Inline name editor. window.prompt() is unusable here: wry implements no
// JavaScript panel delegates, so prompt() returns null (and confirm() false)
// without ever showing anything — which is exactly why groups couldn't be
// created at all.
let pendingEdit = null; // {mode:"new"} | {mode:"rename", id} | {mode:"assign", path}

function openGroupInput(edit, value = "") {
  pendingEdit = edit;
  el.groupInput.hidden = false;
  el.groupInput.value = value;
  el.groupInput.placeholder =
    edit.mode === "rename" ? "New name…" : "Group name (SlimPass, Ton, Saggezza…)";
  el.groupInput.focus();
  el.groupInput.select();
}
function closeGroupInput() {
  pendingEdit = null;
  el.groupInput.hidden = true;
  el.groupInput.value = "";
}
function commitGroupInput() {
  const name = el.groupInput.value.trim();
  const edit = pendingEdit;
  closeGroupInput();
  if (!edit || !name) return render();
  if (edit.mode === "rename") {
    const g = state.groups.groups.find((x) => x.id === edit.id);
    if (g) g.name = name;
    saveGroups();
    return render();
  }
  const g = { id: `g${Date.now().toString(36)}`, name };
  state.groups.groups.push(g);
  // "New group…" chosen from a project's dropdown: create it AND drop the
  // project straight in, which is what that gesture means.
  if (edit.mode === "assign") state.groups.projects[edit.path] = g.id;
  saveGroups();
  render();
}

function deleteGroup(id) {
  state.groups.groups = state.groups.groups.filter((g) => g.id !== id);
  for (const [path, gid] of Object.entries(state.groups.projects))
    if (gid === id) delete state.groups.projects[path];
  saveGroups();
  render();
}

function assignProject(path, groupId) {
  if (groupId === UNGROUPED) delete state.groups.projects[path];
  else state.groups.projects[path] = groupId;
  saveGroups();
  render();
}

async function load() {
  el.content.dataset.state = "loading";
  try {
    // Only the limit-anchored windows need the live headers; the calendar ones
    // must still work when the API call fails.
    const needsUsage = WINDOWS[state.window].kind === "reset";
    let usage = null;
    if (needsUsage) {
      usage = await invoke("get_usage");
      if (usage.state !== "active") throw new Error(usage.message || "no usage data");
    }
    const [start, end] = windowBounds(state.window, usage);

    const res = await invoke("get_sessions", { windowStart: start, windowEnd: end });
    if (res.state !== "active") throw new Error(res.message || "scan failed");

    // Derived, per-project: estimated $ and the share used for the allocation.
    // Share is by OUTPUT tokens on purpose: cache reads are ~97% of all tokens,
    // so "total tokens" mostly measures conversation length, not work done.
    // Fable is excluded — see CREDIT_MODEL.
    const plan = PLANS[localStorage.getItem("cuw-plan")] ?? PLANS.max20;
    // How much of a real invoice this window represents. For "This month" it is
    // the whole invoice — allocating all of it by share-so-far answers "if the
    // month ended now, what would each client's slice be?", which is the actual
    // billing question. Shorter/longer windows are pro-rata by duration.
    const windowSecs = res.window_end - res.window_start;
    const invoiceFraction =
      state.window === "month" ? 1 : windowSecs / (30 * 86400);
    // Plan share counts only models that actually consume the plan windows.
    const planOut = (p) =>
      (p.models || []).reduce((a, m) => a + (drawsCredits(m.model) ? 0 : m.tokens.output), 0);
    const fableOut = (p) =>
      (p.models || []).reduce((a, m) => a + (drawsCredits(m.model) ? m.tokens.output : 0), 0);
    const totalPlanOut = res.projects.reduce((a, p) => a + planOut(p), 0) || 1;
    for (const p of res.projects) {
      p._cost = estCost(p.models);
      p._fable = fableOut(p);
      p._share = planOut(p) / totalPlanOut;
      p._alloc = plan * invoiceFraction * p._share;
    }
    state.data = res;

    el.windowLabel.textContent = `${fmtClock(res.window_start)} → ${fmtClock(res.window_end)}`;
    el.planPrice.textContent = `$${plan}/mo`;
    const sessCount = res.projects.reduce((a, p) => a + p.sessions.length, 0);
    el.scanNote.textContent = `${sessCount} sessions across ${res.projects.length} projects, ${res.total_requests} requests, ${res.files_scanned} transcripts scanned`;
    // A session open in a pane but idle spends nothing, so it isn't here. Say so:
    // "where are my other sessions?" is otherwise a reasonable thing to conclude
    // is a bug (measured: 17 sessions open, 6 with activity in the 5h window).
    el.windowNote.textContent =
      state.window === "all"
        ? ""
        : "Lists activity in this window — a session that's open but idle doesn't appear. Use All time to see every session.";
    render();
  } catch (e) {
    el.errMsg.textContent = String(e);
    el.content.dataset.state = "error";
  }
}

window.addEventListener("DOMContentLoaded", () => {
  for (const id of [
    "content", "rows", "search", "windowSeg", "windowLabel", "refreshBtn", "errMsg",
    "planPrice", "basisLabel", "scanNote", "windowNote",
    "tAllocated", "tCost", "tOutput", "tInput", "tCacheWrite", "tCacheRead",
    "tRequests", "tActive", "tSessions", "tFable", "newGroupBtn", "groupInput", "expandAllBtn",
  ]) {
    el[id] = document.getElementById(id);
  }
  // Remember the size the user picked. The window is resizable, but a table you
  // widened once should still be wide next time you open it.
  const W = window.__TAURI__.window;
  try {
    const saved = JSON.parse(localStorage.getItem("cuw-sessions-size") || "null");
    if (saved?.w > 300 && saved?.h > 200) {
      W.getCurrentWindow().setSize(new window.__TAURI__.dpi.LogicalSize(saved.w, saved.h));
    }
  } catch {
    /* first run */
  }
  let sizeTimer = null;
  window.addEventListener("resize", () => {
    clearTimeout(sizeTimer);
    sizeTimer = setTimeout(() => {
      localStorage.setItem(
        "cuw-sessions-size",
        JSON.stringify({ w: window.innerWidth, h: window.innerHeight }),
      );
    }, 400);
  });

  // Inherit the widget's theme so the two windows don't disagree.
  document.documentElement.dataset.theme = localStorage.getItem("cuw-theme") || "dark";

  el.windowSeg.addEventListener("click", (e) => {
    const b = e.target.closest(".seg-btn");
    if (!b) return;
    for (const x of el.windowSeg.querySelectorAll(".seg-btn")) x.classList.remove("active");
    b.classList.add("active");
    state.window = b.dataset.window;
    load();
  });
  el.search.addEventListener("input", () => {
    state.filter = el.search.value;
    render();
  });
  el.refreshBtn.addEventListener("click", load);
  // The table is project-first, so sessions hide until a row is expanded —
  // which reads as "my sessions are missing". One toggle shows them all.
  el.expandAllBtn.addEventListener("click", () => {
    state.expandAll = !state.expandAll;
    if (!state.expandAll) state.expanded.clear();
    el.expandAllBtn.textContent = state.expandAll ? "▸ Sessions" : "▾ Sessions";
    el.expandAllBtn.classList.toggle("on", state.expandAll);
    render();
  });
  el.newGroupBtn.addEventListener("click", () => openGroupInput({ mode: "new" }));
  el.groupInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") commitGroupInput();
    if (e.key === "Escape") {
      closeGroupInput();
      render();
    }
  });
  el.groupInput.addEventListener("blur", () => {
    if (pendingEdit) commitGroupInput();
  });
  // Any uncaught error would otherwise leave the table looking merely inert.
  window.addEventListener("error", (e) => {
    el.errMsg.textContent = `${e.message} (${e.filename}:${e.lineno})`;
    el.content.dataset.state = "error";
  });
  for (const th of document.querySelectorAll("th[data-sort]")) {
    th.addEventListener("click", () => {
      state.sort = th.dataset.sort;
      render();
    });
  }

  loadGroups().then(load);
});

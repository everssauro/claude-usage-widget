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
  month: { kind: "calendar" },
  all: { kind: "all" },
};

/// [start, end] in unix seconds for the selected window.
function windowBounds(name, usage) {
  const now = Date.now() / 1000;
  const w = WINDOWS[name];
  if (w.kind === "all") return [0, now];
  if (w.kind === "calendar") {
    const d = new Date();
    return [new Date(d.getFullYear(), d.getMonth(), 1).getTime() / 1000, now];
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

const el = {};
const state = {
  window: "current",
  sort: "output",
  filter: "",
  expanded: new Set(),
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
  cost: (p) => p._cost,
  allocated: (p) => p._share,
};

function td(text, cls) {
  const c = document.createElement("td");
  c.className = cls || "c-num";
  c.textContent = text;
  return c;
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

  el.rows.replaceChildren();
  for (const p of projects) {
    const tr = document.createElement("tr");
    tr.className = "p-row";
    const name = document.createElement("td");
    name.className = "c-name";
    const wrap = document.createElement("div");
    wrap.className = "name-cell";
    const tw = document.createElement("span");
    tw.className = "twisty";
    tw.textContent = state.expanded.has(p.path) ? "▾" : "▸";
    const nm = document.createElement("span");
    nm.className = "p-name";
    nm.textContent = p.name;
    const pa = document.createElement("span");
    pa.className = "p-path";
    pa.textContent = p.path;
    wrap.append(tw, nm, pa);
    name.append(wrap);
    tr.append(
      name,
      td(fmtMoney(p._alloc)),
      td("~" + fmtMoney(p._cost), "c-num muted"),
      td(fmtBig(p.tokens.output)),
      td(fmtBig(p.tokens.input)),
      td(fmtBig(p.tokens.cache_write)),
      td(fmtBig(p.tokens.cache_read)),
      td(String(p.requests)),
      td(fmtMin(p.active_min)),
      td(String(p.sessions.length)),
    );
    tr.addEventListener("click", () => {
      try {
        state.expanded.has(p.path) ? state.expanded.delete(p.path) : state.expanded.add(p.path);
        render();
      } catch (err) {
        // Never fail silently: a dead row with no explanation is unfixable
        // from the outside (this window has no devtools in a release build).
        el.errMsg.textContent = `row click: ${err}`;
        el.content.dataset.state = "error";
      }
    });
    el.rows.append(tr);

    if (!state.expanded.has(p.path)) continue;
    for (const s of p.sessions) {
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
        td(fmtBig(s.tokens.output)),
        td(fmtBig(s.tokens.input)),
        td(fmtBig(s.tokens.cache_write)),
        td(fmtBig(s.tokens.cache_read)),
        td(String(s.requests)),
        td(fmtMin(s.active_min)),
        td(""),
      );
      el.rows.append(sr);
    }
  }

  // Totals are over the FILTERED set so the footer always matches what's shown.
  const sum = (f) => projects.reduce((a, p) => a + f(p), 0);
  el.tAllocated.textContent = fmtMoney(sum((p) => p._alloc));
  el.tCost.textContent = "~" + fmtMoney(sum((p) => p._cost));
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
    const plan = PLANS[localStorage.getItem("cuw-plan")] ?? PLANS.max20;
    const totalOut = res.projects.reduce((a, p) => a + p.tokens.output, 0) || 1;
    // How much of a real invoice this window represents. For "This month" it is
    // the whole invoice — allocating all of it by share-so-far answers "if the
    // month ended now, what would each client's slice be?", which is the actual
    // billing question. Shorter/longer windows are pro-rata by duration.
    const windowSecs = res.window_end - res.window_start;
    const invoiceFraction =
      state.window === "month" ? 1 : windowSecs / (30 * 86400);
    for (const p of res.projects) {
      p._cost = estCost(p.models);
      p._share = p.tokens.output / totalOut;
      p._alloc = plan * invoiceFraction * p._share;
    }
    state.data = res;

    el.windowLabel.textContent = `${fmtClock(res.window_start)} → ${fmtClock(res.window_end)}`;
    el.planPrice.textContent = `$${plan}/mo`;
    el.scanNote.textContent = `${res.projects.length} projects, ${res.total_requests} requests, ${res.files_scanned} transcripts scanned`;
    render();
  } catch (e) {
    el.errMsg.textContent = String(e);
    el.content.dataset.state = "error";
  }
}

window.addEventListener("DOMContentLoaded", () => {
  for (const id of [
    "content", "rows", "search", "windowSeg", "windowLabel", "refreshBtn", "errMsg",
    "planPrice", "basisLabel", "scanNote",
    "tAllocated", "tCost", "tOutput", "tInput", "tCacheWrite", "tCacheRead",
    "tRequests", "tActive", "tSessions",
  ]) {
    el[id] = document.getElementById(id);
  }
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

  load();
});

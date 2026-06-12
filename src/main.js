const { invoke } = window.__TAURI__.core;

// Poll cadence: 30s while utilization is moving; decay to 2min once several
// consecutive samples are identical (each poll costs a sliver of the very
// quota it measures), snap back on change.
const POLL_MS = 30_000;
const POLL_SLOW_MS = 120_000;
const FLAT_SAMPLES_TO_SLOW = 5;

const ANIMS = [
  "idle_breathe",
  "idle_blink",
  "idle_look_around",
  "expression_wink",
  "expression_surprise",
  "expression_sleep",
  "work_think",
  "work_coding",
  "dance_sway",
  "dance_bounce",
  "dance_sway_dj",
  "dance_bounce_dj",
  "dance_djmix",
].map((n) => `${n}.json`);

// Usage band → animation (busier as usage climbs).
const BAND_ANIM = [
  "idle_breathe",
  "work_think",
  "work_coding",
  "dance_bounce",
  "dance_djmix",
].map((n) => `${n}.json`);

const STATUS_WORDS = [
  "Divining…",
  "Baking…",
  "Brewing…",
  "Pondering…",
  "Summoning…",
  "Conjuring…",
  "Percolating…",
  "Ruminating…",
];

const SIZES = {
  compact: [280, 300],
  info: [280, 500],
  creature: [280, 300],
  settings: [280, 400],
  connect: [280, 340],
};

// Subscription tiers (USD/month). Plan is user-selected in settings — the
// rate-limit % doesn't map to token counts in any documented way, so
// auto-detection was unreliable and removed.
const PLANS = {
  pro: { label: "Pro", price: 20 },
  max5: { label: "Max 5×", price: 100 },
  max20: { label: "Max 20×", price: 200 },
};

const clamp = (n, lo, hi) => Math.min(hi, Math.max(lo, n));
const bandFor = (pct) =>
  pct < 30 ? 0 : pct < 55 ? 1 : pct < 75 ? 2 : pct < 90 ? 3 : 4;

const heat = (pct) => `hsl(${90 - (clamp(pct, 0, 100) / 100) * 90} 55% 48%)`;

const fmtDur = (min) => {
  if (min <= 0) return "0m";
  const d = Math.floor(min / 1440);
  const h = Math.floor((min % 1440) / 60);
  const m = min % 60;
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
};
const fmtReset = (min) => (min <= 0 ? "Resetting…" : `Resets in ${fmtDur(min)}`);
const fmtClock = (min) =>
  new Date(Date.now() + min * 60000).toLocaleTimeString([], {
    hour: "2-digit", minute: "2-digit", hour12: false,
  });

function fmtBig(n) {
  if (n >= 1e6) return (n / 1e6).toFixed(1).replace(/\.0$/, "") + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(1).replace(/\.0$/, "") + "k";
  return String(n);
}

function shortModels(models) {
  const s = new Set((models || []).map((m) => m.replace(/^claude-/, "").replace(/-\d.*$/, "")));
  return [...s].join(" · ") || "—";
}

// localStorage boolean prefs ("1"/"0", default-true unless stored "0").
const pref = {
  getBool: (k) => localStorage.getItem(k) !== "0",
  setBool: (k, v) => localStorage.setItem(k, v ? "1" : "0"),
};

const el = {};
function cache() {
  for (const id of [
    "card", "mascot", "mascotBig", "pinBtn", "expandBtn", "closeBtn", "creatureBack",
    "settingsBtn", "themeSeg", "planSeg", "notifToggle", "sSub", "sBlock", "sMonth", "sValue",
    "curPct", "curBar", "curReset", "wkPct", "wkBar", "wkReset",
    "statusText", "errMsg", "dCost", "dBurn", "dProj", "dModels", "dTokens", "dCache",
    "connectBtn", "connectStart", "codeInput", "codeSubmit", "connectHint", "connectBack",
    "accountBtn",
  ]) {
    el[id] = document.getElementById(id);
  }
}

// Segmented controls (theme, plan).
function bindSeg(seg, dataKey, onPick) {
  seg.addEventListener("click", (e) => {
    const b = e.target.closest(".seg-btn");
    if (b) onPick(b.dataset[dataKey]);
  });
}
function setSegActive(seg, dataKey, value) {
  for (const b of seg.querySelectorAll(".seg-btn"))
    b.classList.toggle("active", b.dataset[dataKey] === value);
}

// ---------------------------------------------------------------------------
// Mascot animation engine — 20×20 palette-indexed frames, pre-rendered once to
// offscreen canvases (one drawImage per tick instead of ~400 fillRects), JSON
// cached per file, paused while the document is hidden.
// ---------------------------------------------------------------------------
const animCache = new Map(); // file → [{hold, bmp}]
const anim = { file: null, frames: [], idx: 0, timer: null, canvas: null, ctx: null };

function prerenderFrame(grid, palette) {
  const c = document.createElement("canvas");
  c.width = 20;
  c.height = 20;
  const x = c.getContext("2d");
  for (let y = 0; y < grid.length; y++) {
    const row = grid[y];
    for (let i = 0; i < row.length; i++) {
      const color = palette[row[i]];
      if (!color || color === "transparent") continue;
      x.fillStyle = color;
      x.fillRect(i, y, 1, 1);
    }
  }
  return c;
}

async function loadAnim(file) {
  if (!animCache.has(file)) {
    const data = await (await fetch(`/assets/animations/${file}`)).json();
    const palette = data.palette || ["transparent", "#CD7F6A", "#0f0f0f"];
    animCache.set(
      file,
      (data.frames || []).map((f) => ({ hold: f.hold || 120, bmp: prerenderFrame(f.grid, palette) })),
    );
  }
  return animCache.get(file);
}

function redraw() {
  if (!anim.ctx || !anim.frames[anim.idx]) return;
  anim.ctx.clearRect(0, 0, 20, 20);
  anim.ctx.drawImage(anim.frames[anim.idx].bmp, 0, 0);
}

function setCanvas(canvasEl) {
  if (anim.ctx && anim.canvas !== canvasEl) anim.ctx.clearRect(0, 0, 20, 20);
  anim.canvas = canvasEl;
  anim.ctx = canvasEl.getContext("2d");
  redraw();
}

function playStep() {
  if (!anim.frames.length) return;
  redraw();
  const hold = anim.frames[anim.idx].hold;
  anim.idx = (anim.idx + 1) % anim.frames.length;
  anim.timer = setTimeout(playStep, hold);
}

async function setAnim(file) {
  if (file === anim.file) return;
  anim.file = file;
  try {
    const frames = await loadAnim(file);
    if (anim.file !== file) return; // superseded while loading
    anim.frames = frames;
    anim.idx = 0;
    clearTimeout(anim.timer);
    if (!document.hidden) playStep();
  } catch {
    /* keep previous animation */
  }
}

function cycleAnim() {
  const i = ANIMS.indexOf(anim.file);
  setAnim(ANIMS[(i + 1) % ANIMS.length]);
}

// Pause the ~8fps loop while the webview isn't visible (battery).
document.addEventListener("visibilitychange", () => {
  clearTimeout(anim.timer);
  if (!document.hidden) playStep();
});

// ---------------------------------------------------------------------------
// View state machine: compact | info | creature | settings
// ---------------------------------------------------------------------------
let view = "compact";
let baseBeforeCreature = "compact";

async function resizeWindow(w, h) {
  try {
    const T = window.__TAURI__;
    // LogicalSize lives in __TAURI__.dpi (not .window).
    const LogicalSize = (T.dpi && T.dpi.LogicalSize) || T.window.LogicalSize;
    await T.window.getCurrentWindow().setSize(new LogicalSize(w, h));
  } catch {
    /* not in Tauri (headless) */
  }
}

async function setView(mode) {
  const prev = view;
  view = mode;
  el.card.dataset.view = mode;
  resizeWindow(...SIZES[mode]);

  mode === "info" ? startCost() : stopCost();

  if (mode === "settings") {
    refreshCost(); // block API-equiv (once)
    refreshMonth(); // month API-equiv
    renderSettings();
    refreshAuthRow();
  }

  if (mode === "creature") {
    setCanvas(el.mascotBig);
    setAnim("idle_breathe.json");
  } else if (prev === "creature") {
    setCanvas(el.mascot);
    const b = lastActive ? bandFor(lastActive.current_pct) : 0;
    lastBand = b;
    setAnim(BAND_ANIM[b]);
  }
  el.expandBtn.textContent = mode === "info" ? "⤡" : "⤢";
}

function closeApp() {
  try {
    window.__TAURI__.window.getCurrentWindow().close();
  } catch {
    /* headless */
  }
}

// Pin = PiP mode: float on top, on every Space, over fullscreen apps. Toggled by
// the header pin button, remembered in localStorage. Default on.
let pinned = true;
async function applyPinned(on) {
  pinned = on;
  try {
    await invoke("set_pinned", { on });
  } catch {
    /* headless */
  }
  el.pinBtn.classList.toggle("active", on);
  el.pinBtn.title = on
    ? "floating popover (all desktops) — click to unpin"
    : "click to float on all desktops";
  pref.setBool("cuw-pinned", on);
}

// ---------------------------------------------------------------------------
// Settings: theme, notifications, plan + cost comparison
// ---------------------------------------------------------------------------
let theme = "dark";
let notifEnabled = true;
let planManual = null; // user-selected plan; null until chosen
let lastCost = null; // last get_cost active payload
let monthCost = null; // current month's API-equivalent $ (get_month_cost)

const effectivePlan = () => planManual || "max20";

function applyTheme(t) {
  theme = t === "light" ? "light" : "dark";
  document.documentElement.dataset.theme = theme;
  localStorage.setItem("cuw-theme", theme);
  setSegActive(el.themeSeg, "themeVal", theme);
}

function applyNotif(on) {
  notifEnabled = !!on;
  el.notifToggle.setAttribute("aria-checked", notifEnabled ? "true" : "false");
  pref.setBool("cuw-notif", notifEnabled);
}

function setPlan(p) {
  planManual = p;
  localStorage.setItem("cuw-plan", p);
  renderSettings();
}

// ---------------------------------------------------------------------------
// Account / "Sign in with Claude" (OAuth PKCE — auth.rs)
// ---------------------------------------------------------------------------
let authState = "none"; // own | claude_code | none

async function refreshAuthRow() {
  try {
    authState = await invoke("auth_status");
  } catch {
    authState = "none";
  }
  el.accountBtn.textContent =
    authState === "own"
      ? "Connected — sign out"
      : authState === "claude_code"
        ? "Claude Code ✓ · switch"
        : "Sign in";
}

async function startConnect() {
  setView("connect");
  el.codeInput.hidden = false;
  el.codeSubmit.hidden = false;
  el.connectHint.textContent = "Opening browser… approve, copy the code, paste it above.";
  try {
    await invoke("start_login");
  } catch (e) {
    el.connectHint.textContent = String(e);
  }
}

async function submitCode() {
  const code = el.codeInput.value.trim();
  if (!code) return;
  el.connectHint.textContent = "Connecting…";
  try {
    await invoke("finish_login", { code });
    el.connectHint.textContent = "Connected ✓";
    el.codeInput.value = "";
    lastActive = null; // force a fresh fetch with the new account
    setView("compact");
    el.card.dataset.state = "loading";
    refresh();
    refreshAuthRow();
  } catch (e) {
    el.connectHint.textContent = String(e);
  }
}

function renderSettings() {
  const plan = effectivePlan();
  setSegActive(el.planSeg, "plan", plan);
  const sub = PLANS[plan].price;
  el.sSub.textContent = `$${sub}/mo`;
  el.sBlock.textContent = lastCost ? `$${lastCost.cost_usd.toFixed(2)}` : "—";
  if (monthCost != null) {
    el.sMonth.textContent = `$${monthCost.toFixed(2)}`;
    el.sValue.textContent = `${(monthCost / sub).toFixed(1)}× subscription`;
  } else {
    el.sMonth.textContent = "—";
    el.sValue.textContent = "—";
  }
}

// ---------------------------------------------------------------------------
// Data fetching — one guarded-invoke pattern for all three commands.
// ---------------------------------------------------------------------------
function guardedInvoke(cmd, onResult, onError) {
  let busy = false;
  return async () => {
    if (busy) return; // don't stack calls if one is slow
    busy = true;
    try {
      onResult(await invoke(cmd));
    } catch (e) {
      onError(e);
    } finally {
      busy = false;
    }
  };
}

const refresh = guardedInvoke("get_usage", (d) => render(d), (e) => showDegraded(String(e)));
const refreshCost = guardedInvoke("get_cost", (c) => renderCost(c), (e) =>
  renderCost({ state: "error", message: String(e) }),
);
const refreshMonth = guardedInvoke(
  "get_month_cost",
  (m) => {
    monthCost = m.state === "active" ? m.cost_usd : null;
    renderSettings();
  },
  () => {
    monthCost = null;
    renderSettings();
  },
);

// Cost polling — only while the info panel is open (ccusage is local/free).
let costTimer = null;
function startCost() {
  if (costTimer) return;
  refreshCost();
  costTimer = setInterval(refreshCost, POLL_MS);
}
function stopCost() {
  clearInterval(costTimer);
  costTimer = null;
}

function renderCost(c) {
  const dash = () => {
    el.dCost.textContent = el.dBurn.textContent = el.dProj.textContent =
      el.dTokens.textContent = el.dCache.textContent = "—";
  };
  if (c.state === "active") {
    el.dCost.textContent = `$${c.cost_usd.toFixed(2)}`;
    el.dBurn.textContent = `$${c.cost_per_hour.toFixed(1)}/h`;
    el.dProj.textContent = `$${c.projected_cost.toFixed(2)}`;
    el.dModels.textContent = shortModels(c.models);
    el.dTokens.textContent = fmtBig(c.total_tokens);
    // Cache hit = share of input-side tokens served from cache ("MPG" of Claude Code).
    const inAll = c.input_tokens + c.cache_read_tokens + c.cache_creation_tokens;
    el.dCache.textContent = inAll > 0 ? `${Math.round((c.cache_read_tokens / inAll) * 100)}%` : "—";
    lastCost = c;
    if (view === "settings") renderSettings();
  } else if (c.state === "idle") {
    dash();
    el.dModels.textContent = "no active block";
  } else {
    dash();
    el.dModels.textContent = "ccusage error";
  }
}

// ---------------------------------------------------------------------------
// ETA-to-limit + risk + notification
// ---------------------------------------------------------------------------

// Estimate when the 5h utilization hits 100% from its recent slope. Returns
// minutes-to-limit, or null if not climbing / not enough samples yet.
let pctSamples = [];
function estimateEtaMin(currentPct) {
  const now = Date.now();
  const last = pctSamples[pctSamples.length - 1];
  if (last && currentPct < last.pct - 3) pctSamples = []; // block reset → restart
  pctSamples.push({ t: now, pct: currentPct });
  pctSamples = pctSamples.filter((s) => now - s.t <= 10 * 60000);
  if (pctSamples.length < 2) return null;
  const a = pctSamples[0], b = pctSamples[pctSamples.length - 1];
  const dtMin = (b.t - a.t) / 60000;
  if (dtMin < 0.8) return null;
  const rate = (b.pct - a.pct) / dtMin; // % per minute
  if (rate <= 0.05) return null;
  return Math.max(0, Math.round((100 - currentPct) / rate));
}

// "rejected" (over/throttled) | "risk" (will hit before reset) | "warning" | "ok"
function riskState(u, etaMin) {
  if (u.current_pct >= 100 || u.status === "rejected") return "rejected";
  if (etaMin != null && etaMin < u.current_reset_min) return "risk";
  if (u.status === "allowed_warning" || u.current_pct >= 80) return "warning";
  return "ok";
}

let notifGranted = null; // resolved once, cached
let notified80 = false;
async function notify(title, body) {
  try {
    const N = window.__TAURI__.notification;
    if (notifGranted === null) notifGranted = await N.isPermissionGranted();
    if (!notifGranted) notifGranted = (await N.requestPermission()) === "granted";
    if (notifGranted) await N.sendNotification({ title, body });
  } catch {
    /* headless / denied */
  }
}
function maybeNotify(u) {
  if (u.current_pct < 70) notified80 = false; // re-arm after reset
  if (notifEnabled && u.current_pct >= 80 && !notified80) {
    notified80 = true;
    notify(
      "Claude Usage Widget",
      `Your Claude Code 5h usage block is at ${u.current_pct}% — resets ${fmtClock(u.current_reset_min)} (in ${fmtDur(u.current_reset_min)}). Consider wrapping up.`,
    );
  }
}

// ---------------------------------------------------------------------------
// Usage rendering — drives bars + band animation + risk footer.
// ---------------------------------------------------------------------------
let lastBand = -1;
let statusIdx = 0;
let lastActive = null;
let lastEtaMin = null;
let lastRisk = "ok";
let flatCount = 0;
let lastPct = null;

const setStale = (on) => (el.card.dataset.stale = on ? "true" : "false");

function renderActive(u) {
  el.curPct.textContent = `${u.current_pct}%`;
  el.curBar.style.width = `${clamp(u.current_pct, 0, 100)}%`;
  el.curBar.style.background = heat(u.current_pct);
  // 5h reset shows the clock time too ("plan a break around it"); weekly stays a countdown.
  el.curReset.textContent =
    u.current_reset_min > 0
      ? `${fmtReset(u.current_reset_min)} (${fmtClock(u.current_reset_min)})`
      : fmtReset(u.current_reset_min);

  el.wkPct.textContent = `${u.weekly_pct}%`;
  el.wkBar.style.width = `${clamp(u.weekly_pct, 0, 100)}%`;
  el.wkBar.style.background = heat(u.weekly_pct);
  el.wkReset.textContent = fmtReset(u.weekly_reset_min);

  // ETA-to-limit + throttle status drive the footer + card color (computed on
  // fresh data in render(); see lastRisk/lastEtaMin).
  el.card.dataset.status =
    lastRisk === "rejected" ? "rejected" : lastRisk === "ok" ? "ok" : "warning";
  el.statusText.textContent =
    lastRisk === "rejected"
      ? "limit reached"
      : lastRisk === "risk"
        ? `limit in ${fmtDur(lastEtaMin)}`
        : lastRisk === "warning"
          ? "approaching limit"
          : STATUS_WORDS[statusIdx % STATUS_WORDS.length];

  if (view !== "creature") {
    const b = bandFor(u.current_pct);
    if (b !== lastBand) {
      setAnim(BAND_ANIM[b]);
      lastBand = b;
    }
  }
  el.card.dataset.state = "active";
}

function showDegraded(message) {
  if (lastActive) {
    renderActive(lastActive);
    setStale(true);
  } else {
    el.card.dataset.state = "error";
    el.errMsg.textContent = message ?? "";
  }
}

function render(data) {
  if (data.state === "active") {
    lastActive = data;
    statusIdx++;
    // Track flat streaks for adaptive poll cadence.
    flatCount = data.current_pct === lastPct ? flatCount + 1 : 0;
    lastPct = data.current_pct;
    // Sample utilization + evaluate risk only on FRESH data (not stale re-renders).
    lastEtaMin = estimateEtaMin(data.current_pct);
    lastRisk = riskState(data, lastEtaMin);
    maybeNotify(data);
    renderActive(data);
    setStale(false);
  } else {
    showDegraded(data.message);
  }
}

async function pollLoop() {
  await refresh();
  const delay = flatCount >= FLAT_SAMPLES_TO_SLOW ? POLL_SLOW_MS : POLL_MS;
  setTimeout(pollLoop, delay);
}

window.addEventListener("DOMContentLoaded", () => {
  cache();
  setCanvas(el.mascot);
  setAnim("idle_breathe.json");

  el.mascot.addEventListener("click", () => {
    baseBeforeCreature = view; // compact | info | settings
    setView("creature");
  });
  el.mascotBig.addEventListener("click", cycleAnim);
  el.expandBtn.addEventListener("click", () => setView(view === "info" ? "compact" : "info"));
  el.settingsBtn.addEventListener("click", () =>
    setView(view === "settings" ? "compact" : "settings"),
  );
  el.creatureBack.addEventListener("click", () => setView(baseBeforeCreature));
  el.closeBtn.addEventListener("click", closeApp);
  el.pinBtn.addEventListener("click", () => applyPinned(!pinned));
  bindSeg(el.themeSeg, "themeVal", applyTheme);
  bindSeg(el.planSeg, "plan", setPlan);
  el.notifToggle.addEventListener("click", () => applyNotif(!notifEnabled));
  // Account / connect flow
  el.connectBtn.addEventListener("click", startConnect); // from the error overlay
  el.connectStart.addEventListener("click", startConnect); // re-open browser
  el.codeSubmit.addEventListener("click", submitCode);
  el.codeInput.addEventListener("keydown", (e) => e.key === "Enter" && submitCode());
  el.connectBack.addEventListener("click", () => setView("compact"));
  el.accountBtn.addEventListener("click", async () => {
    if (authState === "own") {
      await invoke("sign_out").catch(() => {});
      refreshAuthRow();
      refresh();
    } else {
      startConnect();
    }
  });

  applyPinned(pref.getBool("cuw-pinned")); // default on
  applyTheme(localStorage.getItem("cuw-theme") || "dark");
  applyNotif(pref.getBool("cuw-notif"));
  planManual = localStorage.getItem("cuw-plan"); // null until the user picks
  renderSettings();

  pollLoop();
});

// Test hooks for headless rendering.
window.__cuwRender = (data) => {
  cache();
  render(data);
};
window.__cuwRenderCost = (c) => {
  cache();
  el.card.dataset.view = "info";
  renderCost(c);
};

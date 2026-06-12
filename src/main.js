const { invoke } = window.__TAURI__.core;

const POLL_MS = 30_000;

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

const SIZES = { compact: [280, 300], info: [280, 500], creature: [280, 300], settings: [280, 360] };

// Subscription tiers: monthly price (USD) + rough 5h token ceiling (for auto-detect).
const PLANS = {
  pro: { label: "Pro", price: 20, ceiling: 44000 },
  max5: { label: "Max 5×", price: 100, ceiling: 88000 },
  max20: { label: "Max 20×", price: 200, ceiling: 220000 },
};

const clamp = (n, lo, hi) => Math.min(hi, Math.max(lo, n));
const bandFor = (pct) =>
  pct < 30 ? 0 : pct < 55 ? 1 : pct < 75 ? 2 : pct < 90 ? 3 : 4;

const heat = (pct) => `hsl(${90 - (clamp(pct, 0, 100) / 100) * 90} 55% 48%)`;

function fmtReset(min) {
  if (min <= 0) return "Resetting…";
  const d = Math.floor(min / 1440);
  const h = Math.floor((min % 1440) / 60);
  const m = min % 60;
  if (d > 0) return `Resets in ${d}d ${h}h`;
  if (h > 0) return `Resets in ${h}h ${m}m`;
  return `Resets in ${m}m`;
}

function fmtBig(n) {
  if (n >= 1e6) return (n / 1e6).toFixed(1).replace(/\.0$/, "") + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(1).replace(/\.0$/, "") + "k";
  return String(n);
}

function shortModels(models) {
  const s = new Set((models || []).map((m) => m.replace(/^claude-/, "").replace(/-\d.*$/, "")));
  return [...s].join(" · ") || "—";
}

const fmtDur = (min) => {
  if (min <= 0) return "0m";
  const h = Math.floor(min / 60), m = min % 60;
  return h > 0 ? `${h}h ${m}m` : `${m}m`;
};
const fmtClock = (min) =>
  new Date(Date.now() + min * 60000).toLocaleTimeString([], {
    hour: "2-digit", minute: "2-digit", hour12: false,
  });

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

let notified80 = false;
async function notify(title, body) {
  try {
    const N = window.__TAURI__.notification;
    let granted = await N.isPermissionGranted();
    if (!granted) granted = (await N.requestPermission()) === "granted";
    if (granted) await N.sendNotification({ title, body });
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

const el = {};
function cache() {
  for (const id of [
    "card", "mascot", "mascotBig", "pinBtn", "expandBtn", "closeBtn", "creatureBack",
    "settingsBtn", "themeSeg", "planSeg", "notifToggle", "sSub", "sBlock", "sMonth", "sValue",
    "curPct", "curBar", "curReset", "wkPct", "wkBar", "wkReset",
    "statusText", "statusBar", "errMsg", "dCost", "dBurn", "dProj", "dModels", "dTokens", "dCache",
  ]) {
    el[id] = document.getElementById(id);
  }
}

// ---------------------------------------------------------------------------
// Mascot animation engine — palette-indexed 20×20 frames; draws to anim.canvas.
// ---------------------------------------------------------------------------
const anim = { file: null, frames: [], palette: [], idx: 0, timer: null, canvas: null };

function drawFrame(frame) {
  if (!anim.canvas) return;
  const ctx = anim.canvas.getContext("2d");
  ctx.clearRect(0, 0, 20, 20);
  for (let y = 0; y < frame.grid.length; y++) {
    const row = frame.grid[y];
    for (let x = 0; x < row.length; x++) {
      const color = anim.palette[row[x]];
      if (!color || color === "transparent") continue;
      ctx.fillStyle = color;
      ctx.fillRect(x, y, 1, 1);
    }
  }
}

function redraw() {
  if (anim.frames[anim.idx]) drawFrame(anim.frames[anim.idx]);
}

function setCanvas(canvasEl) {
  if (anim.canvas && anim.canvas !== canvasEl) {
    anim.canvas.getContext("2d").clearRect(0, 0, 20, 20);
  }
  anim.canvas = canvasEl;
  redraw();
}

function playStep() {
  if (!anim.frames.length) return;
  const frame = anim.frames[anim.idx];
  drawFrame(frame);
  anim.idx = (anim.idx + 1) % anim.frames.length;
  anim.timer = setTimeout(playStep, frame.hold || 120);
}

async function setAnim(file) {
  if (file === anim.file) return;
  anim.file = file;
  try {
    const data = await (await fetch(`/assets/animations/${file}`)).json();
    anim.frames = data.frames || [];
    anim.palette = data.palette || ["transparent", "#CD7F6A", "#0f0f0f"];
    anim.idx = 0;
    if (anim.timer) clearTimeout(anim.timer);
    playStep();
  } catch {
    /* keep previous animation */
  }
}

function cycleAnim() {
  const i = ANIMS.indexOf(anim.file);
  anim.file = null; // force reload onto the (possibly new) canvas
  setAnim(ANIMS[(i + 1) % ANIMS.length]);
}

// ---------------------------------------------------------------------------
// View state machine: compact | info | creature
// ---------------------------------------------------------------------------
let view = "compact";
let baseBeforeCreature = "compact";

async function resizeWindow(w, h) {
  try {
    const T = window.__TAURI__;
    // LogicalSize lives in __TAURI__.dpi (not .window) — using the wrong module
    // made setSize throw, so the window never grew and the info panel clipped.
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
  }

  if (mode === "creature") {
    setCanvas(el.mascotBig);
    setAnim("idle_breathe.json");
  } else if (prev === "creature") {
    setCanvas(el.mascot);
    const b = lastActive ? bandFor(lastActive.current_pct) : 0;
    lastBand = b;
    anim.file = null;
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
  localStorage.setItem("cuw-pinned", on ? "1" : "0");
}

// ---------------------------------------------------------------------------
// Settings: theme, notifications, plan + cost comparison
// ---------------------------------------------------------------------------
let theme = "dark";
let notifEnabled = true;
let planManual = null; // user override ('pro'|'max5'|'max20'); null = auto
let planAuto = null; // best-effort detected
let lastCost = null; // last get_cost active payload
let monthCost = null; // current month's API-equivalent $ (get_month_cost)

const effectivePlan = () => planManual || planAuto || "max20";

function applyTheme(t) {
  theme = t === "light" ? "light" : "dark";
  document.documentElement.dataset.theme = theme;
  localStorage.setItem("cuw-theme", theme);
  for (const b of el.themeSeg.querySelectorAll(".seg-btn"))
    b.classList.toggle("active", b.dataset.themeVal === theme);
}

function applyNotif(on) {
  notifEnabled = !!on;
  el.notifToggle.setAttribute("aria-checked", notifEnabled ? "true" : "false");
  localStorage.setItem("cuw-notif", notifEnabled ? "1" : "0");
}

function setPlan(p) {
  planManual = p;
  localStorage.setItem("cuw-plan", p);
  renderSettings();
}

// Estimate the plan: 5h ceiling ≈ block weighted tokens / utilization%. Cache
// reads are cheap so they're excluded from the weight. Best-effort only.
function detectPlan() {
  if (!lastActive || lastActive.current_pct < 10 || !lastCost) return;
  const weighted =
    lastCost.input_tokens + lastCost.output_tokens + lastCost.cache_creation_tokens;
  const ceiling = weighted / (lastActive.current_pct / 100);
  planAuto = ceiling < 66000 ? "pro" : ceiling < 154000 ? "max5" : "max20";
  if (!planManual) renderSettings();
}

function renderSettings() {
  const plan = effectivePlan();
  for (const b of el.planSeg.querySelectorAll(".seg-btn"))
    b.classList.toggle("active", b.dataset.plan === plan);
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

async function refreshMonth() {
  try {
    const m = await invoke("get_month_cost");
    monthCost = m.state === "active" ? m.cost_usd : null;
  } catch {
    monthCost = null;
  }
  renderSettings();
}

// ---------------------------------------------------------------------------
// Cost data (ccusage) — only polled while the info panel is open.
// ---------------------------------------------------------------------------
let costTimer = null;

function startCost() {
  if (costTimer) return;
  refreshCost();
  costTimer = setInterval(refreshCost, POLL_MS);
}
function stopCost() {
  if (costTimer) {
    clearInterval(costTimer);
    costTimer = null;
  }
}
let costInFlight = false;
async function refreshCost() {
  if (costInFlight) return; // don't stack ccusage spawns if one is slow
  costInFlight = true;
  try {
    renderCost(await invoke("get_cost"));
  } catch (e) {
    renderCost({ state: "error", message: String(e) });
  } finally {
    costInFlight = false;
  }
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
    detectPlan();
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
// Usage data (rate-limit %) — drives bars + band animation.
// ---------------------------------------------------------------------------
let lastBand = -1;
let statusIdx = 0;
let lastActive = null;

const setStale = (on) => (el.card.dataset.stale = on ? "true" : "false");

let lastEtaMin = null;
let lastRisk = "ok";

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

  // ETA-to-limit + throttle status drives the footer + card color (computed on
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

let usageInFlight = false;
async function refresh() {
  if (usageInFlight) return; // don't stack API calls if one is slow
  usageInFlight = true;
  try {
    render(await invoke("get_usage"));
  } catch (e) {
    showDegraded(String(e));
  } finally {
    usageInFlight = false;
  }
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
  el.themeSeg.addEventListener("click", (e) => {
    const b = e.target.closest(".seg-btn");
    if (b) applyTheme(b.dataset.themeVal);
  });
  el.planSeg.addEventListener("click", (e) => {
    const b = e.target.closest(".seg-btn");
    if (b) setPlan(b.dataset.plan);
  });
  el.notifToggle.addEventListener("click", () => applyNotif(!notifEnabled));

  applyPinned(localStorage.getItem("cuw-pinned") !== "0"); // default on
  applyTheme(localStorage.getItem("cuw-theme") || "dark");
  applyNotif(localStorage.getItem("cuw-notif") !== "0");
  planManual = localStorage.getItem("cuw-plan"); // null → auto-detect
  renderSettings();

  refresh();
  setInterval(refresh, POLL_MS);
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

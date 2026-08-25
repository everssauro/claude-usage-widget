//! Live Claude Code usage via the unified rate-limit headers + cost via ccusage.
//!
//! Mirrors the Clawdmeter daemon: read the Claude Code OAuth token (macOS
//! Keychain, or `~/.claude/.credentials.json`), make one minimal `/v1/messages`
//! call, and read the `anthropic-ratelimit-unified-*` response headers — the
//! same 5h (Current) + 7d (Weekly) utilization the subscription enforces. This
//! is subscription auth (not API-billed). All parsers are pure, unit-tested
//! functions; the network/keychain/subprocess I/O is not.
//!
//! Commands are `async` + `spawn_blocking` so the blocking I/O (HTTPS round-trip,
//! `security`/`npx` subprocesses) never runs on the macOS main thread — running
//! them there froze the window during every poll.

use std::path::PathBuf;
use std::process::Command;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
const TOKEN_TTL: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// View model handed to the frontend. Internally tagged → `{ "state": "...", ... }`.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UsageView {
    Active(Usage),
    Error { message: String },
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Usage {
    /// Current 5h block utilization, 0–100 (may exceed 100 if over).
    pub current_pct: i64,
    pub current_reset_min: i64,
    /// Weekly (7d) utilization, 0–100.
    pub weekly_pct: i64,
    pub weekly_reset_min: i64,
    /// e.g. "allowed", "allowed_warning", "rejected", "unknown". 5h window.
    pub status: String,
    /// Same, for the WEEKLY window. Without this a weekly lockout — the
    /// multi-day block this widget exists to warn about — could never trigger
    /// the blocked takeover, because only the 5h status was ever read.
    pub weekly_status: String,
    /// Anthropic's own answer to "which limit is binding": "five_hour" |
    /// "seven_day" (and possibly others). Beats our max() guess when present;
    /// empty when the header is absent, and the frontend falls back.
    pub representative: String,
    /// Usage credits ("overage"). A SECOND currency, not part of the plan:
    /// Fable 5 does not consume the 5h/7d windows at all — the official client
    /// says so outright ("Fable 5 is now using usage credits instead of your
    /// plan limits"). A widget that only shows the plan windows is therefore
    /// blind to every Fable token spent.
    pub credits: Credits,
    /// Grace period: over the limit but still being served, temporarily.
    pub grace_status: String,
    /// Anthropic flagged the window as past its warning threshold. More
    /// authoritative than comparing our own percentages to a guessed number.
    pub surpassed_5h: bool,
    pub surpassed_7d: bool,
    /// Every window Anthropic reports, including model-scoped ones (Fable).
    /// Empty on the header fallback path — the headers don't carry them.
    pub limits: Vec<ScopedLimit>,
    /// Credits as real money. Preferred over `credits` (header-derived).
    pub spend: Spend,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Credits {
    /// Whether the response said anything about credits at all. Absent headers
    /// must not read as "0% of credits used".
    pub present: bool,
    /// "allowed" | "rejected" | …
    pub status: String,
    /// Credits are actively being drawn right now.
    pub in_use: bool,
    /// Utilization of the current credit period, 0–100.
    pub pct: i64,
    /// Utilization of the monthly credit allowance, 0–100.
    pub monthly_pct: i64,
    pub reset_min: i64,
    /// Why credits are unavailable, e.g. "out_of_credits".
    pub disabled_reason: String,
}

/// One entry of the API's generic `limits[]` array. Keeping it generic is the
/// point: Anthropic adds model- and surface-scoped windows over time (Fable has
/// its own weekly bucket today), and a generic list surfaces new ones without
/// us having to guess header names.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScopedLimit {
    /// "Fable", "Opus", … — empty for the plain session/weekly windows.
    pub label: String,
    /// "session" | "weekly_all" | "weekly_scoped".
    pub kind: String,
    pub group: String,
    pub pct: i64,
    pub reset_min: i64,
    /// Anthropic's own severity ("normal" | "warning" | …) — better than
    /// comparing percentages to thresholds we invented.
    pub severity: String,
    /// Anthropic says THIS is the window currently binding you.
    pub active: bool,
}

/// Usage credits, in real money, in the user's own currency.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct Spend {
    pub present: bool,
    pub used_minor: i64,
    pub limit_minor: i64,
    pub currency: String,
    pub exponent: u32,
    pub pct: i64,
    pub enabled: bool,
    pub disabled_reason: String,
}

fn minor(v: &serde_json::Value, key: &str) -> i64 {
    v.get(key)
        .and_then(|m| m.get("amount_minor"))
        .and_then(|x| x.as_i64())
        .unwrap_or(0)
}

/// Pure: `GET /api/oauth/usage` body → view model.
///
/// This is the preferred source over the rate-limit response headers, because
/// (a) it is a GET, so reading usage no longer spends any, (b) it exposes
/// model-scoped windows (Fable's weekly bucket is invisible in the headers),
/// and (c) it reports credits as actual money in the account's currency.
pub fn parse_usage_api(json: &str, now_unix: f64) -> Result<Usage, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("bad usage payload: {e}"))?;

    let reset_min = |iso: Option<&str>| -> i64 {
        iso.and_then(crate::sessions::parse_iso_unix)
            .map(|t| (((t - now_unix) / 60.0).round() as i64).max(0))
            .unwrap_or(0)
    };

    let mut limits: Vec<ScopedLimit> = Vec::new();
    for l in v.get("limits").and_then(|x| x.as_array()).into_iter().flatten() {
        let s = |k: &str| l.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        limits.push(ScopedLimit {
            label: l
                .get("scope")
                .and_then(|x| x.get("model"))
                .and_then(|x| x.get("display_name"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            kind: s("kind"),
            group: s("group"),
            pct: l.get("percent").and_then(|x| x.as_f64()).unwrap_or(0.0).round() as i64,
            reset_min: reset_min(l.get("resets_at").and_then(|x| x.as_str())),
            severity: s("severity"),
            active: l.get("is_active").and_then(|x| x.as_bool()).unwrap_or(false),
        });
    }
    if limits.is_empty() {
        return Err("usage payload carried no limits".into());
    }

    let find = |kind: &str| limits.iter().find(|l| l.kind == kind).cloned();
    let session = find("session");
    let weekly = find("weekly_all");

    // Severity is Anthropic's word, so it drives our status directly.
    let status_of = |l: &Option<ScopedLimit>| match l.as_ref().map(|x| (x.severity.as_str(), x.pct))
    {
        Some(("normal", _)) => "allowed".to_string(),
        Some(("warning", _)) => "allowed_warning".to_string(),
        Some((_, pct)) if pct >= 100 => "rejected".to_string(),
        Some((other, _)) => other.to_string(),
        None => "unknown".to_string(),
    };

    let spend = v.get("spend").map(|s| Spend {
        present: true,
        used_minor: minor(s, "used"),
        limit_minor: minor(s, "limit"),
        currency: s
            .get("used")
            .and_then(|u| u.get("currency"))
            .and_then(|x| x.as_str())
            .unwrap_or("USD")
            .to_string(),
        exponent: s
            .get("used")
            .and_then(|u| u.get("exponent"))
            .and_then(|x| x.as_u64())
            .unwrap_or(2) as u32,
        pct: s.get("percent").and_then(|x| x.as_f64()).unwrap_or(0.0).round() as i64,
        enabled: s.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false),
        disabled_reason: s
            .get("disabled_reason")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
    });

    // Which window is binding, straight from `is_active` — no max() guessing.
    let representative = match limits.iter().find(|l| l.active) {
        Some(l) if l.group == "session" => "five_hour".to_string(),
        Some(_) => "seven_day".to_string(),
        None => String::new(),
    };

    Ok(Usage {
        current_pct: session.as_ref().map(|l| l.pct).unwrap_or(0),
        current_reset_min: session.as_ref().map(|l| l.reset_min).unwrap_or(0),
        weekly_pct: weekly.as_ref().map(|l| l.pct).unwrap_or(0),
        weekly_reset_min: weekly.as_ref().map(|l| l.reset_min).unwrap_or(0),
        status: status_of(&session),
        weekly_status: status_of(&weekly),
        representative,
        grace_status: String::new(),
        surpassed_5h: session.map(|l| l.severity != "normal").unwrap_or(false),
        surpassed_7d: weekly.map(|l| l.severity != "normal").unwrap_or(false),
        credits: Credits::default(), // superseded by `spend` on this path
        spend: spend.unwrap_or_default(),
        limits,
    })
}

/// Pure: turn a header lookup + a reference time into the view model.
/// `get(name)` returns the header value; `now_unix` is seconds since epoch.
fn parse_rate_limit<F>(get: F, now_unix: f64) -> Usage
where
    F: Fn(&str) -> Option<String>,
{
    let pct = |name: &str| {
        get(name)
            .and_then(|v| v.parse::<f64>().ok())
            .map(|f| (f * 100.0).round() as i64)
            .unwrap_or(0)
    };
    // Boolean headers arrive as "true"/"false"; anything else is not a yes.
    let flag = |name: &str| get(name).map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(false);
    let reset_min = |name: &str| {
        get(name)
            .and_then(|v| v.parse::<f64>().ok())
            .map(|ts| {
                let mins = (ts - now_unix) / 60.0;
                if mins > 0.0 {
                    mins.round() as i64
                } else {
                    0
                }
            })
            .unwrap_or(0)
    };

    Usage {
        current_pct: pct("anthropic-ratelimit-unified-5h-utilization"),
        current_reset_min: reset_min("anthropic-ratelimit-unified-5h-reset"),
        weekly_pct: pct("anthropic-ratelimit-unified-7d-utilization"),
        weekly_reset_min: reset_min("anthropic-ratelimit-unified-7d-reset"),
        status: get("anthropic-ratelimit-unified-5h-status")
            .unwrap_or_else(|| "unknown".to_string()),
        weekly_status: get("anthropic-ratelimit-unified-7d-status")
            .unwrap_or_else(|| "unknown".to_string()),
        representative: get("anthropic-ratelimit-unified-representative-claim")
            .unwrap_or_default(),
        grace_status: get("anthropic-ratelimit-unified-grace-status").unwrap_or_default(),
        surpassed_5h: flag("anthropic-ratelimit-unified-5h-surpassed-threshold"),
        surpassed_7d: flag("anthropic-ratelimit-unified-7d-surpassed-threshold"),
        credits: Credits {
            // `present` keys on the status header: the utilization ones are
            // simply absent when the account has no credit channel, and
            // defaulting those to 0 would render as "0% of credits used".
            present: get("anthropic-ratelimit-unified-overage-status").is_some(),
            status: get("anthropic-ratelimit-unified-overage-status").unwrap_or_default(),
            in_use: flag("anthropic-ratelimit-unified-overage-in-use"),
            pct: pct("anthropic-ratelimit-unified-overage-utilization"),
            monthly_pct: pct("anthropic-ratelimit-unified-overage-period-monthly-utilization"),
            reset_min: reset_min("anthropic-ratelimit-unified-overage-reset"),
            disabled_reason: get("anthropic-ratelimit-unified-overage-disabled-reason")
                .unwrap_or_default(),
        },
        // Headers carry neither the model-scoped windows nor money.
        limits: Vec::new(),
        spend: Spend::default(),
    }
}

/// Pull the `accessToken` out of a Claude Code credentials blob — direct,
/// nested under any key, or a bare token.
fn extract_access_token(blob: &str) -> Option<String> {
    let blob = blob.trim();
    if blob.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(blob) {
        if let Some(t) = v.get("accessToken").and_then(|t| t.as_str()) {
            return Some(t.to_string());
        }
        if let Some(obj) = v.as_object() {
            for val in obj.values() {
                if let Some(t) = val.get("accessToken").and_then(|t| t.as_str()) {
                    return Some(t.to_string());
                }
            }
        }
    }
    // Bare token (no JSON wrapper).
    if blob.len() >= 20
        && blob
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.~+/=".contains(c))
    {
        return Some(blob.to_string());
    }
    None
}

fn read_token_uncached() -> Option<String> {
    if cfg!(target_os = "macos") {
        let user = std::env::var("USER").ok()?;
        let out = Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                KEYCHAIN_SERVICE,
                "-a",
                &user,
                "-w",
            ])
            .output()
            .ok()?;
        if out.status.success() {
            if let Some(t) = extract_access_token(&String::from_utf8_lossy(&out.stdout)) {
                return Some(t);
            }
        }
    }
    let home = std::env::var_os("HOME")?;
    let path = std::path::Path::new(&home).join(".claude/.credentials.json");
    let raw = std::fs::read_to_string(path).ok()?;
    extract_access_token(&raw)
}

/// Read the Claude Code OAuth token. `CLAUDE_CODE_TOKEN` env wins (handy for
/// headless testing), then macOS Keychain, then `~/.claude/.credentials.json`.
/// Keychain/file reads are cached for [`TOKEN_TTL`] so we don't fork `security`
/// on every poll; cache is invalidated on HTTP 401/403.
fn token_cache() -> &'static Mutex<Option<(String, Instant)>> {
    static CACHE: OnceLock<Mutex<Option<(String, Instant)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn read_token() -> Option<String> {
    if let Ok(t) = std::env::var("CLAUDE_CODE_TOKEN") {
        if !t.trim().is_empty() {
            return Some(t.trim().to_string());
        }
    }
    // The widget's own "Sign in with Claude" login takes precedence (it manages
    // its own expiry/refresh, so it skips the TTL cache below).
    if let Some(t) = crate::auth::current_access_token() {
        return Some(t);
    }
    let mut cache = token_cache().lock().unwrap();
    if let Some((token, at)) = cache.as_ref() {
        if at.elapsed() < TOKEN_TTL {
            return Some(token.clone());
        }
    }
    let token = read_token_uncached()?;
    *cache = Some((token.clone(), Instant::now()));
    Some(token)
}

fn invalidate_token_cache() {
    token_cache().lock().unwrap().take();
}

/// Whether a Claude Code login exists on this machine (Keychain/credentials.json).
pub(crate) fn has_native_token() -> bool {
    read_token_uncached().is_some()
}

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// One shared HTTP agent → connection keep-alive between polls (instead of a
/// fresh DNS+TCP+TLS handshake every 30s).
fn http_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(10)) // don't hang the poll on a slow network
            .build()
    })
}

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// Preferred source: a GET that reports every window (including model-scoped
/// ones like Fable) plus credits in real money — and, unlike the header probe
/// below, spends none of the quota it is measuring.
fn fetch_usage_api(token: &str) -> Result<Usage, String> {
    let resp = http_agent()
        .get(USAGE_URL)
        .set("anthropic-beta", "oauth-2025-04-20")
        .set("user-agent", "claude-code/2.1.5")
        .set("authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|e| {
            if let ureq::Error::Status(401 | 403, _) = e {
                invalidate_token_cache();
            }
            format!("usage API: {e}")
        })?;
    let body = resp.into_string().map_err(|e| format!("usage API body: {e}"))?;
    parse_usage_api(&body, now_unix())
}

fn fetch_usage() -> UsageView {
    let Some(token) = read_token() else {
        return UsageView::Error {
            message: "No Claude account connected".to_string(),
        };
    };

    // Try the rich endpoint first; fall back to the rate-limit headers if it
    // ever goes away (it is undocumented, like the headers themselves).
    match fetch_usage_api(&token) {
        Ok(u) => return UsageView::Active(u),
        Err(e) => eprintln!("usage API unavailable, falling back to headers: {e}"),
    }

    let body = serde_json::json!({
        "model": "claude-haiku-4-5-20251001",
        "max_tokens": 1,
        "messages": [{ "role": "user", "content": "hi" }],
    });

    let result = http_agent()
        .post(API_URL)
        .set("anthropic-version", "2023-06-01")
        .set("anthropic-beta", "oauth-2025-04-20")
        .set("content-type", "application/json")
        .set("user-agent", "claude-code/2.1.5")
        .set("authorization", &format!("Bearer {token}"))
        .send_json(body);

    let resp = match result {
        Ok(r) => r,
        // Rate-limit headers are present even on 4xx (e.g. 429) — use them if so.
        Err(ureq::Error::Status(code, r)) => {
            if matches!(code, 401 | 403) {
                invalidate_token_cache(); // token refreshed/revoked — re-read next poll
            }
            if r.header("anthropic-ratelimit-unified-5h-utilization")
                .is_some()
            {
                r
            } else {
                return UsageView::Error {
                    message: format!("API HTTP {code}"),
                };
            }
        }
        Err(ureq::Error::Transport(t)) => {
            return UsageView::Error {
                message: format!("API call failed: {t}"),
            }
        }
    };

    let usage = parse_rate_limit(|name| resp.header(name).map(|s| s.to_string()), now_unix());
    UsageView::Active(usage)
}

/// Tauri command: read the token, make one minimal call, parse the rate-limit
/// headers. Async + spawn_blocking → off the main thread.
#[tauri::command]
pub async fn get_usage() -> UsageView {
    tauri::async_runtime::spawn_blocking(fetch_usage)
        .await
        .unwrap_or_else(|e| UsageView::Error {
            message: format!("worker failed: {e}"),
        })
}

// ===========================================================================
// Cost / burn / projection — from `ccusage` (the expanded info panel).
// The rate-limit headers don't carry $.
// ===========================================================================

#[derive(Debug, Deserialize)]
struct CcusageOutput {
    #[serde(default)]
    blocks: Vec<CostBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CostBlock {
    #[serde(default)]
    is_active: bool,
    // ccusage spells this `costUSD` (uppercase acronym), not camelCase.
    #[serde(rename = "costUSD", default)]
    cost_usd: f64,
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    total_tokens: u64,
    burn_rate: Option<BurnRate>,
    projection: Option<Projection>,
    token_counts: Option<TokenCounts>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BurnRate {
    #[serde(default)]
    cost_per_hour: f64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Projection {
    #[serde(default)]
    total_cost: f64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenCounts {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CostView {
    Active(Cost),
    Idle,
    Error { message: String },
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Cost {
    pub cost_usd: f64,
    pub cost_per_hour: f64,
    pub projected_cost: f64,
    pub models: Vec<String>,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

/// Pure: ccusage stdout → cost view model. Unit-tested against the fixture.
pub fn parse_cost(json: &str) -> CostView {
    let parsed: CcusageOutput = match serde_json::from_str(json) {
        Ok(p) => p,
        Err(e) => {
            return CostView::Error {
                message: format!("could not parse ccusage output: {e}"),
            }
        }
    };
    match parsed.blocks.into_iter().find(|b| b.is_active) {
        None => CostView::Idle,
        Some(b) => {
            let burn = b.burn_rate.unwrap_or_default();
            let proj = b.projection.unwrap_or_default();
            let tc = b.token_counts.unwrap_or_default();
            CostView::Active(Cost {
                cost_usd: b.cost_usd,
                cost_per_hour: burn.cost_per_hour,
                projected_cost: proj.total_cost,
                models: b.models,
                total_tokens: b.total_tokens,
                input_tokens: tc.input_tokens,
                output_tokens: tc.output_tokens,
                cache_read_tokens: tc.cache_read_input_tokens,
                cache_creation_tokens: tc.cache_creation_input_tokens,
            })
        }
    }
}

// ---- Monthly API-equivalent cost (subscription vs "real tokens" comparison) ----

#[derive(Debug, Deserialize)]
struct CcusageMonthly {
    #[serde(default)]
    monthly: Vec<MonthRow>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MonthRow {
    #[serde(default)]
    month: String,
    #[serde(default)]
    total_cost: f64,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MonthCostView {
    Active { month: String, cost_usd: f64 },
    Error { message: String },
}

/// Pure: ccusage monthly stdout → the current month's total. Last row = current.
pub fn parse_month_cost(json: &str) -> MonthCostView {
    match serde_json::from_str::<CcusageMonthly>(json) {
        Ok(m) => match m.monthly.into_iter().last() {
            Some(row) => MonthCostView::Active {
                month: row.month,
                cost_usd: row.total_cost,
            },
            None => MonthCostView::Active {
                month: String::new(),
                cost_usd: 0.0,
            },
        },
        Err(e) => MonthCostView::Error {
            message: format!("could not parse ccusage monthly: {e}"),
        },
    }
}

// ---------------------------------------------------------------------------
// ccusage runner — shared by get_cost / get_month_cost.
// ---------------------------------------------------------------------------

/// Node install dirs to prepend to PATH so a Finder-launched `.app` (which only
/// inherits `/usr/bin:/bin:...`) can find `npx`/`node`.
fn extra_node_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".bun/bin"));
        dirs.push(home.join(".volta/bin"));
        if let Ok(entries) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            dirs.extend(entries.flatten().map(|e| e.path().join("bin")));
        }
    }
    dirs.retain(|d| d.exists());
    dirs
}

/// Resolve a bare program name (`npx`) to an absolute path — the program is
/// looked up via the *parent* PATH at spawn, so augmenting only the child PATH
/// wouldn't find it. Searches node dirs first, then PATH.
fn resolve_program(program: &str, node_dirs: &[PathBuf]) -> String {
    if program.contains('/') {
        return program.to_string();
    }
    let mut search: Vec<PathBuf> = node_dirs.to_vec();
    if let Ok(path) = std::env::var("PATH") {
        search.extend(std::env::split_paths(&path));
    }
    for dir in search {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    program.to_string()
}

struct CcusageCmd {
    program: String,
    base_args: Vec<String>,
    path: String,
}

/// Parse CCUSAGE_CMD + resolve program + build PATH **once** — node installs
/// don't move mid-session, and the resolution walks the filesystem.
fn ccusage_cmd() -> &'static Result<CcusageCmd, String> {
    static CMD: OnceLock<Result<CcusageCmd, String>> = OnceLock::new();
    CMD.get_or_init(|| {
        // Pinned to @14: v15+ ships a native (Bun-compiled) binary that, on some
        // Macs, hardcodes a nonexistent nix libiconv path and crashes (dyld). v14
        // is the last pure-JS release and runs fine via node. Override with CCUSAGE_CMD.
        let cmd_str =
            std::env::var("CCUSAGE_CMD").unwrap_or_else(|_| "npx -y ccusage@14".to_string());
        let mut parts = cmd_str.split_whitespace();
        let Some(program) = parts.next() else {
            return Err("CCUSAGE_CMD is empty".to_string());
        };
        let node_dirs = extra_node_dirs();
        let mut path_parts: Vec<String> = node_dirs
            .iter()
            .map(|d| d.to_string_lossy().into_owned())
            .collect();
        if let Ok(current) = std::env::var("PATH") {
            if !current.is_empty() {
                path_parts.push(current);
            }
        }
        Ok(CcusageCmd {
            program: resolve_program(program, &node_dirs),
            base_args: parts.map(str::to_string).collect(),
            path: path_parts.join(":"),
        })
    })
}

/// Run `ccusage <subcommand args>` and return its stdout (or an error message).
/// A ccusage run takes ~8-10s here: it rescans the whole transcript archive
/// (~1 GB) every invocation. Two guards make that survivable.
///
/// TTL cache: opening the settings view fires two runs back to back, and
/// toggling views re-fires them; without this the app can spend minutes of CPU
/// answering the same question.
const CCUSAGE_TTL: Duration = Duration::from_secs(120);
/// Kill deadline. `Command::output()` has none, so a wedged child blocked its
/// worker forever — and the JS `busy` guard, which only clears in `finally`,
/// latched permanently: the cost figures froze with no error shown.
const CCUSAGE_TIMEOUT: Duration = Duration::from_secs(25);

fn ccusage_cache() -> &'static Mutex<HashMap<String, (Instant, String)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, String)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn run_ccusage(args: &[&str]) -> Result<String, String> {
    let key = args.join(" ");
    if let Some((at, body)) = ccusage_cache().lock().unwrap().get(&key) {
        if at.elapsed() < CCUSAGE_TTL {
            return Ok(body.clone());
        }
    }
    let cmd = match ccusage_cmd() {
        Ok(c) => c,
        Err(e) => return Err(e.clone()),
    };
    let mut child = Command::new(&cmd.program)
        .args(&cmd.base_args)
        .args(args)
        .env("PATH", &cmd.path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to run ccusage: {e}"))?;

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Err(e) => return Err(format!("ccusage wait failed: {e}")),
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() > CCUSAGE_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("ccusage timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    let out = child
        .wait_with_output()
        .map_err(|e| format!("ccusage output failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ccusage exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let body = String::from_utf8_lossy(&out.stdout).into_owned();
    ccusage_cache()
        .lock()
        .unwrap()
        .insert(key, (Instant::now(), body.clone()));
    Ok(body)
}

/// Tauri command: active-block cost/burn/projection. Polled only while the info
/// panel is open. Async + spawn_blocking → off the main thread.
#[tauri::command]
pub async fn get_cost() -> CostView {
    tauri::async_runtime::spawn_blocking(|| {
        run_ccusage(&["blocks", "--active", "--json"])
            .map_or_else(|message| CostView::Error { message }, |o| parse_cost(&o))
    })
    .await
    .unwrap_or_else(|e| CostView::Error {
        message: format!("worker failed: {e}"),
    })
}

/// Tauri command: current month's API-equivalent cost. Fetched on demand
/// (settings view), not polled.
#[tauri::command]
pub async fn get_month_cost() -> MonthCostView {
    tauri::async_runtime::spawn_blocking(|| {
        run_ccusage(&["monthly", "--json"]).map_or_else(
            |message| MonthCostView::Error { message },
            |o| parse_month_cost(&o),
        )
    })
    .await
    .unwrap_or_else(|e| MonthCostView::Error {
        message: format!("worker failed: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn getter(map: HashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<String> {
        move |name| map.get(name).map(|s| s.to_string())
    }

    #[test]
    fn parses_utilization_and_reset() {
        let now = 1_000_000.0;
        let map = HashMap::from([
            ("anthropic-ratelimit-unified-5h-utilization", "0.29"),
            ("anthropic-ratelimit-unified-5h-reset", "1008520"), // now + 8520s = 2h22m
            ("anthropic-ratelimit-unified-7d-utilization", "0.04"),
            ("anthropic-ratelimit-unified-7d-reset", "1500000"),
            ("anthropic-ratelimit-unified-5h-status", "allowed"),
        ]);
        let u = parse_rate_limit(getter(map), now);
        assert_eq!(u.current_pct, 29);
        assert_eq!(u.current_reset_min, 142); // 8520 / 60
        assert_eq!(u.weekly_pct, 4);
        assert_eq!(u.status, "allowed");
    }

    #[test]
    fn parses_weekly_status_and_representative_claim() {
        // A weekly lockout must be visible: reading only the 5h status made the
        // multi-day block the one state the widget could not show.
        let map = HashMap::from([
            ("anthropic-ratelimit-unified-5h-status", "allowed"),
            ("anthropic-ratelimit-unified-7d-status", "rejected"),
            (
                "anthropic-ratelimit-unified-representative-claim",
                "seven_day",
            ),
        ]);
        let u = parse_rate_limit(getter(map), 0.0);
        assert_eq!(u.status, "allowed");
        assert_eq!(u.weekly_status, "rejected");
        assert_eq!(u.representative, "seven_day");
    }

    #[test]
    fn ccusage_cache_serves_repeats_within_ttl() {
        // Proves the guard that matters: the settings view fires two runs back
        // to back and view toggles re-fire them, each costing ~8-10s of CPU.
        let key = "test blocks".to_string();
        ccusage_cache()
            .lock()
            .unwrap()
            .insert(key.clone(), (Instant::now(), "{\"cached\":true}".into()));
        let hit = {
            let c = ccusage_cache().lock().unwrap();
            let (at, body) = c.get(&key).unwrap();
            (at.elapsed() < CCUSAGE_TTL).then(|| body.clone())
        };
        assert_eq!(hit.as_deref(), Some("{\"cached\":true}"));

        // An entry older than the TTL must NOT be served — stale cost figures
        // presented as current is the failure this cache could introduce.
        let stale = Instant::now() - CCUSAGE_TTL - Duration::from_secs(1);
        ccusage_cache()
            .lock()
            .unwrap()
            .insert(key.clone(), (stale, "old".into()));
        let c = ccusage_cache().lock().unwrap();
        let (at, _) = c.get(&key).unwrap();
        assert!(at.elapsed() >= CCUSAGE_TTL, "stale entry must expire");
    }

    #[test]
    fn parses_the_real_oauth_usage_payload() {
        // Captured live from GET /api/oauth/usage.
        let u = parse_usage_api(
            include_str!("../tests/fixtures/oauth-usage.json"),
            0.0, // epoch → resets are far in the future, so reset_min > 0
        )
        .expect("should parse");

        assert_eq!(u.current_pct, 41);
        assert_eq!(u.weekly_pct, 54);
        assert_eq!(u.status, "allowed");

        // The whole point: a model-scoped window the headers never exposed.
        let fable = u
            .limits
            .iter()
            .find(|l| l.label == "Fable")
            .expect("Fable window must survive parsing");
        assert_eq!(fable.pct, 84);
        assert_eq!(fable.kind, "weekly_scoped");
        assert_eq!(fable.severity, "warning");
        assert!(fable.active);

        // Anthropic says Fable is what's binding — no max() heuristic needed.
        assert_eq!(u.representative, "seven_day");

        // Credits are real money, in the account's own currency.
        assert!(u.spend.present);
        assert_eq!(u.spend.used_minor, 1050);
        assert_eq!(u.spend.limit_minor, 2000);
        assert_eq!(u.spend.currency, "BRL");
        assert_eq!(u.spend.pct, 52);
        assert!(!u.spend.enabled);
        assert_eq!(u.spend.disabled_reason, "out_of_credits");
    }

    #[test]
    fn usage_api_rejects_a_payload_with_no_limits() {
        // Must degrade to the header fallback rather than render zeros.
        assert!(parse_usage_api(r#"{"limits":[]}"#, 0.0).is_err());
        assert!(parse_usage_api("not json", 0.0).is_err());
    }

    #[test]
    fn parses_usage_credits() {
        // Fable 5 spends these, not the plan windows — so "no credit headers"
        // and "0% of credits used" must never look the same.
        let map = HashMap::from([
            ("anthropic-ratelimit-unified-overage-status", "allowed"),
            ("anthropic-ratelimit-unified-overage-in-use", "true"),
            ("anthropic-ratelimit-unified-overage-utilization", "0.42"),
            (
                "anthropic-ratelimit-unified-overage-period-monthly-utilization",
                "0.07",
            ),
        ]);
        let c = parse_rate_limit(getter(map), 0.0).credits;
        assert!(c.present);
        assert!(c.in_use);
        assert_eq!(c.pct, 42);
        assert_eq!(c.monthly_pct, 7);
    }

    #[test]
    fn credits_absent_is_not_zero_credits() {
        let c = parse_rate_limit(|_| None, 0.0).credits;
        assert!(!c.present);
        assert!(!c.in_use);
        assert_eq!(c.pct, 0); // meaningless — `present` is what the UI must gate on
    }

    #[test]
    fn out_of_credits_is_reported_with_reason() {
        // This account's real state today.
        let map = HashMap::from([
            ("anthropic-ratelimit-unified-overage-status", "rejected"),
            (
                "anthropic-ratelimit-unified-overage-disabled-reason",
                "out_of_credits",
            ),
        ]);
        let c = parse_rate_limit(getter(map), 0.0).credits;
        assert!(c.present);
        assert_eq!(c.status, "rejected");
        assert_eq!(c.disabled_reason, "out_of_credits");
        assert!(!c.in_use);
    }

    #[test]
    fn parses_grace_and_surpassed_flags() {
        let map = HashMap::from([
            ("anthropic-ratelimit-unified-grace-status", "active"),
            ("anthropic-ratelimit-unified-5h-surpassed-threshold", "true"),
            ("anthropic-ratelimit-unified-7d-surpassed-threshold", "false"),
        ]);
        let u = parse_rate_limit(getter(map), 0.0);
        assert_eq!(u.grace_status, "active");
        assert!(u.surpassed_5h);
        assert!(!u.surpassed_7d);
    }

    #[test]
    fn absent_weekly_headers_degrade_not_break() {
        let u = parse_rate_limit(|_| None, 0.0);
        assert_eq!(u.weekly_status, "unknown");
        assert_eq!(u.representative, ""); // frontend falls back to its own heuristic
    }

    #[test]
    fn missing_headers_default_to_zero_unknown() {
        let u = parse_rate_limit(|_| None, 0.0);
        assert_eq!(u.current_pct, 0);
        assert_eq!(u.weekly_reset_min, 0);
        assert_eq!(u.status, "unknown");
    }

    #[test]
    fn past_reset_clamps_to_zero() {
        let map = HashMap::from([("anthropic-ratelimit-unified-5h-reset", "500")]);
        let u = parse_rate_limit(getter(map), 1000.0);
        assert_eq!(u.current_reset_min, 0);
    }

    #[test]
    fn extract_token_direct() {
        assert_eq!(
            extract_access_token(r#"{"accessToken":"sk-ant-abc123"}"#).as_deref(),
            Some("sk-ant-abc123")
        );
    }

    #[test]
    fn extract_token_nested() {
        assert_eq!(
            extract_access_token(r#"{"claudeAiOauth":{"accessToken":"tok-xyz"}}"#).as_deref(),
            Some("tok-xyz")
        );
    }

    #[test]
    fn extract_token_bare() {
        assert_eq!(
            extract_access_token("abcdefABCDEF0123456789-_").as_deref(),
            Some("abcdefABCDEF0123456789-_")
        );
    }

    #[test]
    fn extract_token_garbage_is_none() {
        assert_eq!(extract_access_token("not a token!!!"), None);
        assert_eq!(extract_access_token(""), None);
    }

    #[test]
    fn parses_cost_block() {
        match parse_cost(include_str!("../tests/fixtures/active.json")) {
            CostView::Active(c) => {
                assert_eq!(c.cost_usd, 119.1837494);
                assert_eq!(c.cost_per_hour, 48.25125492522409);
                assert_eq!(c.projected_cost, 239.81);
                assert_eq!(c.models.len(), 3);
                assert_eq!(c.total_tokens, 156485656);
                assert_eq!(c.input_tokens, 199873);
                assert_eq!(c.output_tokens, 694386);
                assert_eq!(c.cache_read_tokens, 151742315);
                assert_eq!(c.cache_creation_tokens, 3849082);
            }
            other => panic!("expected Active, got {other:?}"),
        }
    }

    #[test]
    fn cost_empty_blocks_is_idle() {
        assert_eq!(
            parse_cost(include_str!("../tests/fixtures/idle.json")),
            CostView::Idle
        );
    }

    #[test]
    fn cost_bad_json_is_error() {
        match parse_cost("nope") {
            CostView::Error { .. } => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn month_cost_takes_last_row() {
        let json = r#"{"monthly":[
            {"month":"2026-04","totalCost":10.0},
            {"month":"2026-05","totalCost":20.5},
            {"month":"2026-06","totalCost":2874.57}
        ]}"#;
        match parse_month_cost(json) {
            MonthCostView::Active { month, cost_usd } => {
                assert_eq!(month, "2026-06");
                assert_eq!(cost_usd, 2874.57);
            }
            other => panic!("expected Active, got {other:?}"),
        }
    }

    #[test]
    fn month_cost_empty_is_zero() {
        match parse_month_cost(r#"{"monthly":[]}"#) {
            MonthCostView::Active { cost_usd, .. } => assert_eq!(cost_usd, 0.0),
            other => panic!("expected Active, got {other:?}"),
        }
    }
}

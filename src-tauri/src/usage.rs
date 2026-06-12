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
    /// e.g. "allowed", "allowed_warning", "rejected", "unknown".
    pub status: String,
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

fn fetch_usage() -> UsageView {
    let Some(token) = read_token() else {
        return UsageView::Error {
            message: "No Claude account connected".to_string(),
        };
    };

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
fn run_ccusage(args: &[&str]) -> Result<String, String> {
    let cmd = match ccusage_cmd() {
        Ok(c) => c,
        Err(e) => return Err(e.clone()),
    };
    let output = Command::new(&cmd.program)
        .args(&cmd.base_args)
        .args(args)
        .env("PATH", &cmd.path)
        .output();
    match output {
        Err(e) => Err(format!("failed to run ccusage: {e}")),
        Ok(out) if !out.status.success() => Err(format!(
            "ccusage exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Ok(out) => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
    }
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

//! Per-project / per-session usage, aggregated from Claude Code's own JSONL
//! transcripts in `~/.claude/projects`.
//!
//! WHY NOT ccusage (which we already shell out to for cost):
//!  1. Its `session` rows are keyed on the containing directory's last path
//!     segment, so subagent/workflow transcripts land under bogus ids like
//!     `"subagents"` or `"wf_…"` and collide across different projects.
//!  2. Its 5h "blocks" are anchored on first-activity-after-idle, which was
//!     measured drifting 9m23s from Anthropic's real window — misfiling 7% of
//!     that window's requests. We derive the window from the SAME headers the
//!     bars use (`unified-{5h,7d}-reset`), so the table and the bars agree.
//!  3. A run costs ~9.5s wall because it rescans the whole archive every time.
//!     Reading the mtime-filtered slice directly measured ~1s in a Python
//!     prototype (Rust is faster still) and reconciled with ccusage to 0.07%
//!     (input and cache still match it exactly; output is deliberately higher
//!     — see the dedup rules below).
//!
//! CORRECTNESS RULES THAT ARE NOT OPTIONAL (each was measured going wrong):
//!  * DEDUP IS GLOBAL. The same assistant message appears in several files
//!    (2.3x on a main session, 4.1x on a subagent file). Deduping per file
//!    inflated totals by 8.19% and doubled one project that is reachable
//!    through a symlinked directory. Key = `(message.id, requestId)`.
//!  * FIRST-WINS LOSES OUTPUT. Within one file a streamed message is written
//!    once per content block, and only the last line carries the real
//!    `output_tokens` — the early ones hold the `message_start` snapshot (1–3).
//!    Keeping the first line lost 25.2% of all output tokens on this archive;
//!    the most complete snapshot wins instead (`Dedup`). ccusage@14 has the
//!    same first-wins policy, which is why the 0.07% reconciliation never
//!    caught it: our output now legitimately exceeds its (+37% on the day it
//!    was fixed), while input/cache_write/cache_read still match exactly.
//!  * DON'T FOLLOW SYMLINKS when walking. Some project dirs are symlinks to
//!    others (created when a project is renamed/moved).
//!  * PROJECT = THE GIT ROOT of `cwd`, not `cwd` itself: 10% of sessions record
//!    requests under several `cwd`s (subfolders), which would shatter one piece
//!    of work into phantom projects. And never the mangled directory name —
//!    it is stale after a rename and lossy (spaces collapse to `-`).
//!  * `cwd` IS NOT ON THE FIRST LINE. The first lines are metadata; measured
//!    first occurrence at line 1..=8. Scan a prefix, don't peek at line 1.
//!
//! All aggregation is pure and unit-tested; only the walk/read is I/O.

use std::collections::HashMap;

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Files whose mtime is older than the window start minus this margin can't
/// contain in-window records. Generous, to absorb clock skew and tools that
/// rewrite mtimes.
const MTIME_MARGIN_SECS: f64 = 3600.0;

/// A gap longer than this between two requests is not "working time".
const IDLE_GAP_SECS: f64 = 300.0;

// ---------------------------------------------------------------------------
// View model
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl Tokens {
    fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_write += o.cache_write;
        self.cache_read += o.cache_read;
    }
}

/// Per-model tokens, so the frontend can price each model at its own rate
/// (cost is computed there from user-set $/M rates — see `sessions.js`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelUsage {
    pub model: String,
    pub tokens: Tokens,
    pub requests: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionRow {
    pub session_id: String,
    /// `custom-title` / `agent-name` from the transcript when present — a
    /// human-readable name beats a UUID in the table.
    pub title: Option<String>,
    pub tokens: Tokens,
    pub requests: u64,
    /// Requests that came from subagents of this session (already included in
    /// `requests`/`tokens` — shown as a "+N subagents" detail, not a own row).
    pub subagent_requests: u64,
    pub first_ts: f64,
    pub last_ts: f64,
    /// Wall-clock minutes with gaps > IDLE_GAP_SECS removed.
    pub active_min: f64,
    /// Per-model split, so a row's totals can be explained on hover. In the
    /// overview what matters is tokens and cost; which model produced them is
    /// detail, not a column.
    pub models: Vec<ModelUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectRow {
    /// Absolute git-root path — the stable key a group mapping points at.
    pub path: String,
    /// Display name (git root's basename).
    pub name: String,
    pub tokens: Tokens,
    pub requests: u64,
    pub sessions: Vec<SessionRow>,
    pub models: Vec<ModelUsage>,
    /// Union of this project's active intervals (never a sum — concurrent
    /// sessions would report more hours than actually elapsed).
    pub active_min: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionsSummary {
    pub projects: Vec<ProjectRow>,
    pub totals: Tokens,
    pub total_requests: u64,
    /// Union across every project — the honest "time at the keyboard".
    pub active_min: f64,
    pub files_scanned: usize,
    pub window_start: f64,
    pub window_end: f64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SessionsView {
    Active(SessionsSummary),
    Error { message: String },
}

// ---------------------------------------------------------------------------
// Timestamps — parsed by hand so this module needs no date dependency.
// ---------------------------------------------------------------------------

/// Days from the Unix epoch for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `"2026-08-08T17:11:27.414Z"` → unix seconds. `None` for anything that isn't
/// that shape (Claude Code writes UTC ISO-8601 for every record).
pub fn parse_iso_unix(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let num = |from: usize, to: usize| s.get(from..to)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    let mut t = (days * 86_400 + h * 3600 + mi * 60 + sec) as f64;
    // Fractional seconds, if present.
    if b.len() > 20 && b[19] == b'.' {
        let frac: String = s[20..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if !frac.is_empty() {
            if let Ok(v) = frac.parse::<f64>() {
                t += v / 10f64.powi(frac.len() as i32);
            }
        }
    }
    Some(t)
}

// ---------------------------------------------------------------------------
// Pure aggregation
// ---------------------------------------------------------------------------

/// One usage-bearing record, already extracted from a JSONL line.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub ts: f64,
    pub session_id: String,
    pub cwd: String,
    pub model: String,
    pub tokens: Tokens,
    pub is_sidechain: bool,
    /// Global dedup key. `None` when the line carries neither id (rare) — those
    /// can't be deduped, so they're always counted.
    pub dedup_key: Option<String>,
}

/// Pull a usage record out of one JSONL line. `None` for the ~97% of lines that
/// aren't assistant messages with usage, or that fall outside the window.
pub fn parse_record(line: &str) -> Option<Record> {
    // Cheap reject before paying for JSON parsing.
    if !line.contains("\"usage\"") || !line.contains("\"timestamp\"") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let msg = v.get("message")?;
    let usage = msg.get("usage")?;
    let n = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let tokens = Tokens {
        input: n("input_tokens"),
        output: n("output_tokens"),
        cache_write: n("cache_creation_input_tokens"),
        cache_read: n("cache_read_input_tokens"),
    };
    if tokens == Tokens::default() {
        return None;
    }
    let ts = parse_iso_unix(v.get("timestamp")?.as_str()?)?;
    let str_of = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    // `(message.id, requestId)`: message.id is the more discriminating half —
    // deduping on requestId alone silently merges distinct messages.
    let mid = msg.get("id").and_then(|x| x.as_str());
    let rid = v.get("requestId").and_then(|x| x.as_str());
    let dedup_key = match (mid, rid) {
        (None, None) => None,
        (a, b) => Some(format!("{}:{}", a.unwrap_or(""), b.unwrap_or(""))),
    };
    Some(Record {
        ts,
        session_id: str_of("sessionId"),
        cwd: str_of("cwd"),
        model: msg
            .get("model")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .to_string(),
        tokens,
        is_sidechain: v
            .get("isSidechain")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        dedup_key,
    })
}

/// Minutes covered by these timestamps, treating a gap longer than
/// [`IDLE_GAP_SECS`] as "not working". Input need not be sorted.
fn active_minutes(mut ts: Vec<f64>) -> f64 {
    if ts.is_empty() {
        return 0.0;
    }
    ts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut total = 0.0;
    for w in ts.windows(2) {
        let gap = w[1] - w[0];
        if gap <= IDLE_GAP_SECS {
            total += gap;
        }
    }
    total / 60.0
}

/// Aggregate deduped, in-window records into the view model.
/// `project_of` maps a `cwd` to its project key (git root) — injected so the
/// pure aggregation stays testable without touching the filesystem.
pub fn aggregate<F>(records: Vec<Record>, window: (f64, f64), mut project_of: F) -> SessionsSummary
where
    // FnMut so callers can memoize: resolving a git root walks the filesystem,
    // and the same `cwd` repeats across most records.
    F: FnMut(&str) -> String,
{
    struct Acc {
        tokens: Tokens,
        requests: u64,
        ts: Vec<f64>,
        sessions: HashMap<String, SessAcc>,
        models: HashMap<String, (Tokens, u64)>,
    }
    #[derive(Default)]
    struct SessAcc {
        tokens: Tokens,
        requests: u64,
        subagent_requests: u64,
        ts: Vec<f64>,
        models: HashMap<String, (Tokens, u64)>,
    }

    let mut by_project: HashMap<String, Acc> = HashMap::new();
    let mut totals = Tokens::default();
    let mut total_requests = 0u64;
    let mut all_ts: Vec<f64> = Vec::new();

    for r in records {
        let key = project_of(&r.cwd);
        let acc = by_project.entry(key).or_insert_with(|| Acc {
            tokens: Tokens::default(),
            requests: 0,
            ts: Vec::new(),
            sessions: HashMap::new(),
            models: HashMap::new(),
        });
        acc.tokens.add(&r.tokens);
        acc.requests += 1;
        acc.ts.push(r.ts);
        let m = acc
            .models
            .entry(r.model.clone())
            .or_insert((Tokens::default(), 0));
        m.0.add(&r.tokens);
        m.1 += 1;
        let s = acc.sessions.entry(r.session_id.clone()).or_default();
        s.tokens.add(&r.tokens);
        s.requests += 1;
        if r.is_sidechain {
            s.subagent_requests += 1;
        }
        let sm = s
            .models
            .entry(r.model.clone())
            .or_insert((Tokens::default(), 0));
        sm.0.add(&r.tokens);
        sm.1 += 1;
        s.ts.push(r.ts);

        totals.add(&r.tokens);
        total_requests += 1;
        all_ts.push(r.ts);
    }

    let mut projects: Vec<ProjectRow> = by_project
        .into_iter()
        .map(|(path, acc)| {
            let mut sessions: Vec<SessionRow> = acc
                .sessions
                .into_iter()
                .map(|(session_id, s)| {
                    let first = s.ts.iter().cloned().fold(f64::MAX, f64::min);
                    let last = s.ts.iter().cloned().fold(f64::MIN, f64::max);
                    let mut models: Vec<ModelUsage> = s
                        .models
                        .into_iter()
                        .map(|(model, (tokens, requests))| ModelUsage {
                            model,
                            tokens,
                            requests,
                        })
                        .collect();
                    models.sort_by(|a, b| b.tokens.output.cmp(&a.tokens.output));
                    SessionRow {
                        session_id,
                        title: None, // filled by the reader when the transcript names itself
                        tokens: s.tokens,
                        requests: s.requests,
                        subagent_requests: s.subagent_requests,
                        first_ts: first,
                        last_ts: last,
                        active_min: active_minutes(s.ts),
                        models,
                    }
                })
                .collect();
            sessions.sort_by(|a, b| b.tokens.output.cmp(&a.tokens.output));
            let mut models: Vec<ModelUsage> = acc
                .models
                .into_iter()
                .map(|(model, (tokens, requests))| ModelUsage {
                    model,
                    tokens,
                    requests,
                })
                .collect();
            models.sort_by(|a, b| b.tokens.output.cmp(&a.tokens.output));
            // A session started outside any repo (scheduled/headless runs use
            // cwd "/") has no basename, and rendered as a bare slash — an
            // unidentifiable row. Name the condition instead.
            let name = Path::new(&path)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "(no project)".to_string());
            ProjectRow {
                path,
                name,
                tokens: acc.tokens,
                requests: acc.requests,
                sessions,
                models,
                active_min: active_minutes(acc.ts),
            }
        })
        .collect();
    // Output tokens are the least misleading default order: cache reads are
    // ~97% of all tokens, so ranking by total tokens ranks conversation length.
    projects.sort_by(|a, b| b.tokens.output.cmp(&a.tokens.output));

    SessionsSummary {
        projects,
        totals,
        total_requests,
        active_min: active_minutes(all_ts),
        files_scanned: 0,
        window_start: window.0,
        window_end: window.1,
    }
}

// ---------------------------------------------------------------------------
// I/O — walk, filter, read
// ---------------------------------------------------------------------------

fn projects_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".claude/projects"))
}

/// Collect `*.jsonl` under `root` whose mtime could contain in-window records.
/// Symlinked directories are skipped: several project dirs are symlinks to
/// others, and following them would visit the same transcripts twice.
fn candidate_files(root: &Path, since: f64, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for e in entries.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let path = e.path();
        if ft.is_dir() {
            candidate_files(&path, since, out);
        } else if path.extension().map(|x| x == "jsonl").unwrap_or(false) {
            let recent = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64() >= since - MTIME_MARGIN_SECS)
                .unwrap_or(true);
            if recent {
                out.push(path);
            }
        }
    }
}

/// The transcript's own name for itself, if it declares one in its metadata
/// prefix (`custom-title` / `agent-name`). Cheap: only the first few lines.
fn transcript_title(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    for k in ["custom-title", "customTitle", "agent-name", "agentName"] {
        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            if !s.trim().is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// First line of the first user message, as a last-resort session label.
/// Without it an unnamed session shows only a UUID, and answering "which
/// session is this?" means grepping the archive by hand.
fn first_user_text(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("type")?.as_str()? != "user" {
        return None;
    }
    let content = v.get("message")?.get("content")?;
    let raw = match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .and_then(|b| b.get("text"))
            .and_then(|t| t.as_str())?
            .to_string(),
        _ => return None,
    };
    let t = raw.trim();
    // Skip injected system reminders and command wrappers — not what the user typed.
    if t.is_empty() || t.starts_with('<') {
        return None;
    }
    let line1 = t.lines().find(|l| !l.trim().is_empty())?.trim();
    Some(line1.chars().take(70).collect())
}

/// Walk up from `cwd` to the enclosing git repository root. Falls back to `cwd`
/// itself when there is no repo (or the directory is gone).
fn git_root(cwd: &str) -> String {
    let mut p = Path::new(cwd);
    loop {
        if p.join(".git").exists() {
            return p.to_string_lossy().into_owned();
        }
        match p.parent() {
            Some(parent) if parent != p => p = parent,
            _ => return cwd.to_string(),
        }
    }
}

/// Global dedup over every record in the scan — see the module header.
///
/// Claude Code writes one JSONL line per content block of a streamed assistant
/// message, all sharing `(message.id, requestId)`. The early lines carry the
/// usage known at `message_start` — an `output_tokens` of 1–3 — and only the
/// last re-emission carries the final accounting. Keeping the first line lost
/// 25.2% of all output tokens on this archive (18.7M of 74.4M; 16,474 of
/// 69,732 groups). So the MOST COMPLETE snapshot wins: the one with the most
/// output tokens, since output only grows while a message streams (measured:
/// the max is the last line in every group, and never ties with a line that
/// differs elsewhere). It is taken whole rather than as a per-field max
/// because the final accounting can also *lower* a field (measured:
/// `cache_creation` 4520 → 1809 on a subagent line) and a per-field max would
/// stitch a total no API response ever carried. Exact copies across files (the
/// 2.3x/4.1x case) tie, so the first occurrence keeps its identity.
#[derive(Default)]
struct Dedup {
    index: HashMap<String, usize>,
    records: Vec<Record>,
}

impl Dedup {
    fn push(&mut self, r: Record) {
        let Some(k) = r.dedup_key.clone() else {
            self.records.push(r);
            return;
        };
        match self.index.get(&k) {
            Some(&i) => {
                // A later re-emission with more output is the more complete
                // snapshot of the same message; take its accounting whole.
                if r.tokens.output > self.records[i].tokens.output {
                    self.records[i].tokens = r.tokens;
                }
            }
            None => {
                self.index.insert(k, self.records.len());
                self.records.push(r);
            }
        }
    }

    fn into_records(self) -> Vec<Record> {
        self.records
    }
}

fn scan(window_start: f64, window_end: f64) -> SessionsView {
    let Some(root) = projects_dir() else {
        return SessionsView::Error {
            message: "no HOME".into(),
        };
    };
    if !root.exists() {
        return SessionsView::Error {
            message: format!("{} not found", root.display()),
        };
    }

    let mut files = Vec::new();
    candidate_files(&root, window_start, &mut files);
    let files_scanned = files.len();

    let mut dedup = Dedup::default();
    let mut titles: HashMap<String, String> = HashMap::new();

    for path in &files {
        let Ok(f) = File::open(path) else { continue };
        for (i, line) in BufReader::new(f).lines().enumerate() {
            // A session being written to right now can have a torn last line.
            // A single unreadable line must not truncate the rest of the file:
            // `break` here would silently drop every session after it.
            let Ok(line) = line else { continue };
            if i < 60 {
                let named = transcript_title(&line);
                // The transcript's own name wins; the first prompt is the fallback.
                if let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) {
                    if let Some(t) = named {
                        titles.insert(stem, t);
                    } else if !titles.contains_key(&stem) {
                        if let Some(t) = first_user_text(&line) {
                            titles.insert(stem, t);
                        }
                    }
                }
            }
            let Some(r) = parse_record(&line) else { continue };
            if r.ts < window_start || r.ts > window_end {
                continue;
            }
            // GLOBAL dedup — see the module header and `Dedup`.
            dedup.push(r);
        }
    }

    let mut roots: HashMap<String, String> = HashMap::new();
    let mut summary = aggregate(dedup.into_records(), (window_start, window_end), |cwd| {
        if let Some(hit) = roots.get(cwd) {
            return hit.clone();
        }
        let r = git_root(cwd);
        roots.insert(cwd.to_string(), r.clone());
        r
    });
    summary.files_scanned = files_scanned;
    for p in &mut summary.projects {
        for s in &mut p.sessions {
            s.title = titles.get(&s.session_id).cloned();
        }
    }
    SessionsView::Active(summary)
}

// ---------------------------------------------------------------------------
// Groups — user-named folders over projects/sessions ("Acme", "Personal", …).
//
// Stored as a plain JSON blob next to `window.json` rather than in the webview's
// localStorage: this is the mapping that turns raw folders into who-owes-what,
// so it must survive a reinstall, be backup-able, and be editable by hand.
// The schema lives in the frontend; Rust only guarantees it is valid JSON and
// that a failed write never truncates the previous mapping.
// ---------------------------------------------------------------------------

fn groups_file(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    use tauri::Manager;
    Some(app.path().app_config_dir().ok()?.join("groups.json"))
}

/// Raw JSON blob, or an empty string when nothing has been saved yet.
#[tauri::command]
pub fn get_groups(app: tauri::AppHandle) -> String {
    groups_file(&app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}

#[tauri::command]
pub fn save_groups(app: tauri::AppHandle, json: String) -> Result<(), String> {
    // Reject anything unparseable before it can replace a good mapping.
    serde_json::from_str::<serde_json::Value>(&json).map_err(|e| format!("invalid groups: {e}"))?;
    let path = groups_file(&app).ok_or("no config dir")?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Write-then-rename so an interrupted write can't leave a half file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// Tauri command: per-project/session usage inside an explicit window.
///
/// The window is passed in (not guessed here) so it is always the SAME window
/// the bars show: the frontend derives it from `unified-{5h,7d}-reset`, which
/// is Anthropic's own boundary. Async + spawn_blocking — this touches hundreds
/// of megabytes and must never run on the UI thread.
#[tauri::command]
pub async fn get_sessions(window_start: f64, window_end: f64) -> SessionsView {
    tauri::async_runtime::spawn_blocking(move || scan(window_start, window_end))
        .await
        .unwrap_or_else(|e| SessionsView::Error {
            message: format!("worker failed: {e}"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iso_timestamps() {
        // 2026-08-08T17:11:27Z — cross-checked against `date -u -j -f ...`.
        let t = parse_iso_unix("2026-08-08T17:11:27.414Z").unwrap();
        assert!((t - 1_786_209_087.414).abs() < 0.01, "got {t}");
        assert_eq!(parse_iso_unix("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(parse_iso_unix("2026-08-08"), None);
        assert_eq!(parse_iso_unix("garbage"), None);
    }

    #[test]
    fn skips_lines_without_usage() {
        assert!(parse_record(r#"{"type":"user","timestamp":"2026-08-08T17:00:00Z"}"#).is_none());
        assert!(parse_record("not json").is_none());
    }

    fn line(mid: &str, rid: &str, session: &str, cwd: &str, out: u64) -> String {
        format!(
            r#"{{"timestamp":"2026-08-08T17:00:00Z","sessionId":"{session}","cwd":"{cwd}",
                "requestId":"{rid}","message":{{"id":"{mid}","model":"claude-opus-5",
                "usage":{{"input_tokens":10,"output_tokens":{out},
                "cache_creation_input_tokens":5,"cache_read_input_tokens":100}}}}}}"#
        )
        .replace('\n', "")
    }

    #[test]
    fn extracts_tokens_and_dedup_key() {
        let r = parse_record(&line("m1", "r1", "s1", "/p", 7)).unwrap();
        assert_eq!(r.tokens.output, 7);
        assert_eq!(r.tokens.cache_read, 100);
        assert_eq!(r.session_id, "s1");
        assert_eq!(r.dedup_key.as_deref(), Some("m1:r1"));
    }

    #[test]
    fn aggregates_by_project_and_session() {
        let recs: Vec<Record> = ["a", "b"]
            .iter()
            .map(|m| parse_record(&line(m, "r", "s1", "/repo", 5)).unwrap())
            .collect();
        let s = aggregate(recs, (0.0, f64::MAX), |cwd| cwd.to_string());
        assert_eq!(s.projects.len(), 1);
        assert_eq!(s.projects[0].requests, 2);
        assert_eq!(s.projects[0].tokens.output, 10);
        assert_eq!(s.projects[0].sessions.len(), 1);
        assert_eq!(s.total_requests, 2);
    }

    #[test]
    fn project_key_maps_many_cwds_to_one_row() {
        // The real failure this guards: one session touching subfolders would
        // otherwise become three phantom projects.
        let recs = vec![
            parse_record(&line("m1", "r1", "s", "/repo/src", 1)).unwrap(),
            parse_record(&line("m2", "r2", "s", "/repo/docs", 1)).unwrap(),
        ];
        let s = aggregate(recs, (0.0, f64::MAX), |_| "/repo".to_string());
        assert_eq!(s.projects.len(), 1);
        assert_eq!(s.projects[0].name, "repo");
    }

    #[test]
    fn active_minutes_drops_idle_gaps() {
        let t = 1_000_000.0;
        // 60s apart (counts), then a 2h gap (doesn't), then 30s (counts).
        let m = active_minutes(vec![t, t + 60.0, t + 7260.0, t + 7290.0]);
        assert!((m - 1.5).abs() < 0.001, "got {m}");
        assert_eq!(active_minutes(vec![]), 0.0);
    }

    /// Manual: dump what an ALL-TIME scan actually finds, to compare against the
    /// raw archive. Run with `-- --ignored --nocapture dump_all_time`.
    #[test]
    #[ignore]
    fn dump_all_time() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let SessionsView::Active(s) = scan(0.0, now) else {
            panic!("scan failed")
        };
        let sessions: usize = s.projects.iter().map(|p| p.sessions.len()).sum();
        println!(
            "RUST all-time: projects={} sessions={} requests={} files_scanned={}",
            s.projects.len(),
            sessions,
            s.total_requests,
            s.files_scanned
        );
        let mut empty_id = 0;
        for p in &s.projects {
            for sess in &p.sessions {
                if sess.session_id.is_empty() {
                    empty_id += 1;
                }
            }
        }
        println!("sessions with empty id: {empty_id}");
    }

    /// Manual reconciliation against ccusage on the REAL archive. Not part of
    /// the gate (it reads ~/.claude/projects and takes seconds); run it when the
    /// parsing/dedup logic changes:
    ///
    ///   cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture reconcile
    ///
    /// Then compare with:
    ///   npx -y ccusage@14 daily --json --breakdown --since <YYYYMMDD>
    ///
    /// Input, cache_write and cache_read must match per model to within a
    /// rounding error; a mismatch there means the dedup key or the window
    /// filter regressed. OUTPUT is expected to be HIGHER than ccusage@14's —
    /// it keeps the first (partial) line of a re-emitted message, we keep the
    /// most complete one (see `Dedup`). Measured 2026-09-09: in/cw/cr exact on
    /// all four models; output 1,434,683 vs 1,047,584 (+37%), two models
    /// identical and opus-5 +45%.
    #[test]
    #[ignore]
    fn reconcile_with_ccusage() {
        // Local midnight → now, so the window lines up with ccusage's `daily`.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let offset_secs = 3.0 * 3600.0; // this machine is UTC-3
        let local_midnight = ((now - offset_secs) / 86400.0).floor() * 86400.0 + offset_secs;

        let view = scan(local_midnight, now);
        let SessionsView::Active(s) = view else {
            panic!("scan failed");
        };

        let mut per_model: HashMap<String, Tokens> = HashMap::new();
        for p in &s.projects {
            for m in &p.models {
                per_model.entry(m.model.clone()).or_default().add(&m.tokens);
            }
        }
        let mut models: Vec<_> = per_model.into_iter().collect();
        models.sort_by(|a, b| b.1.output.cmp(&a.1.output));

        println!("\n=== window {} → {} (files scanned: {}) ===", local_midnight, now, s.files_scanned);
        let mut grand = 0u64;
        for (model, t) in &models {
            let total = t.input + t.output + t.cache_write + t.cache_read;
            grand += total;
            println!(
                "{model:<24} total={total:>14}  in={:>10} out={:>10} cw={:>12} cr={:>14}",
                t.input, t.output, t.cache_write, t.cache_read
            );
        }
        println!("{:<24} total={grand:>14}", "ALL MODELS");
        println!("projects={} requests={}", s.projects.len(), s.total_requests);
        for p in s.projects.iter().take(10) {
            println!("  {:<44} out={:>10} req={:>5}", p.name, p.tokens.output, p.requests);
        }
    }

    fn usage_line(mid: &str, rid: &str, session: &str, t: Tokens) -> String {
        format!(
            r#"{{"timestamp":"2026-08-08T17:00:00Z","sessionId":"{session}","cwd":"/p",
                "requestId":"{rid}","message":{{"id":"{mid}","model":"claude-opus-5",
                "usage":{{"input_tokens":{},"output_tokens":{},
                "cache_creation_input_tokens":{},"cache_read_input_tokens":{}}}}}}}"#,
            t.input, t.output, t.cache_write, t.cache_read
        )
        .replace('\n', "")
    }

    fn dedup(lines: &[String]) -> Vec<Record> {
        let mut d = Dedup::default();
        for l in lines {
            d.push(parse_record(l).unwrap());
        }
        d.into_records()
    }

    #[test]
    fn dedup_keeps_the_most_complete_snapshot_of_a_streamed_message() {
        // One line per content block, same (message.id, requestId); the early
        // lines carry the partial output count and only the last the real one.
        // First-wins kept the 3 — measured 25.2% of all output tokens lost.
        let lines: Vec<String> = [3, 3, 3, 2055]
            .iter()
            .map(|&out| line("m1", "r1", "s1", "/p", out))
            .collect();
        let recs = dedup(&lines);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].tokens.output, 2055);
    }

    #[test]
    fn dedup_takes_the_final_accounting_whole_not_a_per_field_max() {
        // The final snapshot can LOWER a field (seen on a subagent transcript:
        // cache_creation 4520 → 1809). A per-field max would report a total no
        // API response ever carried.
        let partial = Tokens { input: 2, output: 3, cache_write: 4520, cache_read: 78525 };
        let fin = Tokens { input: 514, output: 1797, cache_write: 1809, cache_read: 78525 };
        let recs = dedup(&[
            usage_line("m1", "r1", "s1", partial),
            usage_line("m1", "r1", "s1", partial),
            usage_line("m1", "r1", "s1", fin),
        ]);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].tokens, fin);
    }

    #[test]
    fn dedup_counts_exact_copies_once_and_keeps_the_first_identity() {
        // The same message shows up in several files (2.3x on a main session,
        // 4.1x on a subagent file) with identical usage.
        let recs = dedup(&[
            line("m1", "r1", "main", "/p", 40),
            line("m1", "r1", "subagent-copy", "/p", 40),
            line("m1", "r1", "subagent-copy", "/p", 40),
        ]);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].session_id, "main");
        assert_eq!(recs[0].tokens.output, 40);
    }

    #[test]
    fn dedup_always_counts_records_without_a_key() {
        let mut r = parse_record(&line("m1", "r1", "s1", "/p", 1)).unwrap();
        r.dedup_key = None;
        let mut d = Dedup::default();
        d.push(r.clone());
        d.push(r);
        assert_eq!(d.into_records().len(), 2);
    }

    #[test]
    fn reads_a_transcript_title() {
        assert_eq!(
            transcript_title(r#"{"custom-title":"ClaWidget"}"#).as_deref(),
            Some("ClaWidget")
        );
        assert_eq!(transcript_title(r#"{"type":"user"}"#), None);
    }
}

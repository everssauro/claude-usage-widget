# Implementation plan — Claude Usage Widget (macOS / Tauri v2)

Execute in a Claude Code session opened in `claude-usage-widget/`. Build incrementally; verify each phase before the next. Spec: `docs/spec.md`.

## Phase 0 — Prerequisites ✅ DONE (2026-06-10)
- [x] **Rust** — installed via rustup: `cargo 1.96.0` (aarch64-apple-darwin). PATH needs `source "$HOME/.cargo/env"` per non-login shell.
- [x] **Xcode CLT**: `/Library/Developer/CommandLineTools` ✓.
- [x] **Node** v22.22.0 ✓, npm 11.12.1 ✓.
- [x] **ccusage** works — returns active-block JSON. Live JSON also has `burnRate.tokensPerMinuteForIndicator` (smoothed), `actualEndTime`, `totalTokens`, `entries`, `isGap` beyond spec's list.

## Phase 1 — Scaffold Tauri v2 (vanilla) ✅
- [x] Scaffolded `create-tauri-app` 4.6.2 vanilla into a temp dir, integrated `src/`, `src-tauri/`, `.vscode/`, `package.json` into root (kept README/docs/REFERENCE/CLAUDE.md; merged `.gitignore`). Renamed `cuw-scaffold` → `claude-usage-widget` (lib `claude_usage_widget_lib`, productName "Claude Usage Widget").
- [x] `npm install` + `cargo build` green (52.8s, Tauri 2.11.2).
- [ ] Commit scaffold — **deferred** (batched git decision at end; on `main`).

## Phase 2 — Data: Rust command `get_usage` ✅
- [x] `serde`/`serde_json` (already in scaffold). Structs in `src-tauri/src/usage.rs` mirror real ccusage keys — note `costUSD` (uppercase) needs explicit `rename`, not camelCase.
- [x] `#[tauri::command] get_usage()` runs ccusage via `std::process::Command`, returns `UsageView::{Active,Idle,Error}` (internally tagged → `{state,...}`). `CCUSAGE_CMD` override supported. Parser kept **pure** (time math deferred to JS) for deterministic tests.
- [x] **4 unit tests** pass: active parse, empty→idle, malformed→error, inactive-block→idle. Fixtures `tests/fixtures/{active,idle}.json`.
- [x] Registered in `tauri::Builder` (removed demo `greet`).

## Phase 3 — Window config (floating widget) ✅
- [x] `tauri.conf.json`: `decorations:false`, `alwaysOnTop:true`, `transparent:true`, `resizable:false`, `skipTaskbar:true`, `shadow:false`, 280×150, `macOSPrivateApi:true` (needs cargo feature `macos-private-api` — added). Top-right positioning done in Rust `setup()` via monitor size (adapts to screen).
- [x] Rounded corners + shadow + glass blur via CSS. Whole card is `data-tauri-drag-region`.

## Phase 4 — UI (layout) ✅
- [x] `src/{index.html,styles.css,main.js}`. Big cost, burn ($/h · tok/m) with **heat color from the smoothed `tokensPerMinuteForIndicator`** (raw `tokensPerMinute` ~1M is cache-dominated/misleading), time bar, projection, compact models.
- [x] States via `[data-state]`: loading / idle / error overlays.
- [x] system-ui + SF Mono only; original meter glyph + original app icon. No mascot/proprietary fonts.

## Phase 5 — Refresh + polish ✅
- [x] `setInterval` 10s → `invoke('get_usage')`. Keep-last-good on degrade + stale dot (block clock keeps ticking).
- [x] Original app icon generated from `/tmp/cuw-icon.svg` (gauge mark) via `tauri icon`.
- [x] `npm run tauri build` → `.app` + `.dmg` (`src-tauri/target/release/bundle/`).
- [x] **PATH fix** (`usage.rs`): a Finder-launched `.app` only inherits `/usr/bin:/bin:...` — node is nvm-managed here, so `get_usage` now resolves `npx` to an absolute path (nvm/homebrew dirs) + passes augmented PATH to the child. Without this the bundled app errors on every poll.

## Phase 6 — Verify 🟡
- [x] **UI verified via Playwright** (real `styles.css`/`main.js`, mocked invoke) — all 4 states render correctly. Screenshots in `docs/screenshots/` (active, idle, stale, error).
- [x] Parser verified by 4 unit tests; release binary runs clean (no stderr errors).
- [~] **Live Tauri window**: position/process/logs all correct, but `screencapture` could not grab the wry window in this automation context (even opaque+decorated) — likely a Spaces/capture limitation. **Needs user to confirm the floating widget appears on launch.**
- [ ] Commit + push — **pending git decision** (on `main`).

## Known caveats / future
- Idle "last $X" not shown (needs a 2nd ccusage call without `--active`). YAGNI for MVP.
- Bundled `.app` is unsigned/un-notarized → Gatekeeper will warn on first open (right-click → Open, or `xattr -dr com.apple.quarantine` the `.app`).
- A `__cuwRender` test hook is exposed on `window` (enables headless UI screenshots); harmless, remove if undesired.

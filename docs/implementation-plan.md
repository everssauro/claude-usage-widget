# Implementation plan — Claude Usage Widget (macOS / Tauri v2)

Execute in a Claude Code session opened in `claude-usage-widget/`. Build incrementally; verify each phase before the next. Spec: `docs/spec.md`.

## Phase 0 — Prerequisites
- [ ] **Rust** (Tauri needs it; MISSING on this Mac): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`; `source ~/.cargo/env`; verify `cargo --version`.
- [ ] **Xcode CLT**: `xcode-select --install` (verify `xcode-select -p`).
- [ ] **Node** v22 ✓ (`node -v`).
- [ ] **ccusage** works: `npx -y ccusage@latest blocks --active --json` returns JSON. Save a sample to `src-tauri/tests/fixtures/active.json`.

## Phase 1 — Scaffold Tauri v2 (vanilla)
- [ ] `npm create tauri-app@latest . -- --template vanilla` into this folder (keep README/docs/REFERENCE/CLAUDE.md).
- [ ] `npm install`; `npm run tauri dev` opens default window.
- [ ] Commit scaffold.

## Phase 2 — Data: Rust command `get_usage`
- [ ] Add `serde`/`serde_json` in `src-tauri/`. Structs for ccusage `blocks[0]` (cost, tokenCounts, burnRate, projection, start/end, models, isActive).
- [ ] `#[tauri::command] fn get_usage()` — run `npx -y ccusage@latest blocks --active --json` via `std::process::Command`, parse, return typed view model (or `Idle`/`Error` variant). Allow `CCUSAGE_CMD` env override (for a global install = faster).
- [ ] **Unit test** the parser against `tests/fixtures/active.json` + an empty-blocks (idle) fixture.
- [ ] Register the command in `tauri::Builder`.

## Phase 3 — Window config (floating widget)
- [ ] `tauri.conf.json` window: `decorations:false`, `alwaysOnTop:true`, `transparent:true`, `resizable:false`, `skipTaskbar:true`, `width:280`, `height:150`, corner position. Enable `macOSPrivateApi` if needed for transparency.
- [ ] Rounded corners + subtle shadow via CSS. Drag: top bar with `data-tauri-drag-region`.

## Phase 4 — UI (layout)
- [ ] `src/index.html` + `style.css` + `main.js`. Big **cost**, **burn rate** (tok/min + $/h) with **heat color** from `tokensPerMinute` (calm→hot), **time-remaining bar**, **projection**, **model(s)**.
- [ ] States: loading, idle (no active block), error (ccusage/node missing).
- [ ] **Fonts/assets: system-ui / SF Mono only; original glyph. NEVER Clawd mascot or Anthropic proprietary fonts** (REFERENCE.md).

## Phase 5 — Refresh + polish
- [ ] Frontend `setInterval` 10s → `invoke('get_usage')` → render. Keep last good value on error; "stale" marker.
- [ ] Original app icon (`npm run tauri icon <png>`).
- [ ] `npm run tauri build` → `.app`.

## Phase 6 — Verify
- [ ] dev: live 10s updates, draggable, always-on-top, corner.
- [ ] Force idle + error states.
- [ ] Run the built `.app` standalone.
- [ ] Commit + push.

# claude-usage-widget

Native macOS desktop widget showing live Claude Code usage (current 5h block). Tauri v2 (Rust + web UI). Data from `ccusage`.

## ▶ Start here (first build session)
This project is **designed but not implemented**. To build it:
1. Read `docs/spec.md` (the approved design) and `docs/implementation-plan.md` (6 phases).
2. Do **Phase 0** first — install prereqs (**Rust/rustup is NOT installed yet**; Tauri needs it).
3. Then execute the plan **phase by phase, verifying each** before moving on.
4. Asset rule (legal): original assets only — **no Clawd mascot, no Anthropic proprietary fonts** (see `REFERENCE.md`).

Origin: independent project inspired by Clawdmeter (credit in `REFERENCE.md`). Repo: `everssauro/claude-usage-widget` (private).

## Build / run
- Prereqs: **Rust** (rustup — NOT yet installed on the dev Mac), **Xcode CLT**, **Node** (v22 ✓), **ccusage** (`npx -y ccusage@latest blocks --active --json` must return JSON).
- Dev: `npm run tauri dev` · Build: `npm run tauri build` (→ `.app` in `src-tauri/target/release/bundle/macos/`).

## Architecture
- `src-tauri/` (Rust): window config (floating, always-on-top, transparent, no chrome, draggable, ~280×150, skipTaskbar) + `#[tauri::command] get_usage()` that runs ccusage, parses JSON, returns a typed view model. Parser is unit-tested with fixture JSON.
- `src/` (web UI): vanilla HTML/CSS/JS. Renders cost, burn-rate (heat color), time bar, projection, models. States: loading / idle / error. Polls `get_usage` every 10s.
- Data brain = **ccusage** (we don't recompute usage).

## Design docs (read before coding)
- `docs/spec.md` — approved design.
- `docs/implementation-plan.md` — phased build steps. **Follow in order, verify each phase.**
- `REFERENCE.md` — origin credit + **asset rule: no Clawd mascot, no Anthropic proprietary fonts**.

## Conventions
- Reuse the hard part (usage calc) via ccusage; only build the thin UI shell.
- Keep it MVP: active-block widget only. No history/menu-bar/settings yet (YAGNI).
- Original assets only (legal — see REFERENCE.md).

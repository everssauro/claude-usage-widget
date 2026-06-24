# claude-usage-widget

Native macOS desktop widget — a **faithful Clawdmeter clone** showing live Claude Code **subscription** usage. Tauri v2 (Rust + vanilla web UI).

## ▶ Status (2026-06-10)
Working `.app`, verified on-screen with real data. Repo: `everssauro/claude-usage-widget` — **PUBLIC, source-only** (no installers/binaries distributed; `bundle.targets` excludes `.dmg`; CI = tests only). Bundles the copyrighted Clawd mascot + proprietary Anthropic fonts used without permission → **no license** (all rights reserved), mirroring upstream; README carries the "not affiliated / personal use / you have been warned" disclaimer. See `REFERENCE.md` DECISION 2026-06-15 (public, credited) — supersedes the earlier private-only rule. Clean exit if needed: swap the 2 commercial fonts for OFL lookalikes.

Features: Current (5h) + Weekly (7d) usage % with heat bars + reset timers (5h also shows clock time); **ETA-to-limit** ("limit in Xh Ym" from the utilization slope) + **throttle status** (amber/red card from the `status` header) + **80% macOS notification**; animated Clawd mascot (13 ClaudePix animations, busier as usage climbs); **⤢ expand** → detail panel (cost/burn/projection/models/tokens/**cache-hit %** via ccusage); **click mascot → creature mode** (big idle Clawd, click cycles figure, ↩ back); **📌 pin → PiP mode** (visible on all Spaces + floats over fullscreen apps + on top); **⚙ settings** (theme **dark/light** [Claude palette], notifications on/off, **plan** Pro/Max5×/Max20× auto-detected + override, **cost comparison**: subscription $/mo vs API-equivalent spent this block + this month via `ccusage monthly`); **✕** (hover) closes; whole card draggable, **remembers position**; Clawd app icon.

## Data — two sources
- **Primary: rate-limit % (`get_usage`)** — reads the Claude Code OAuth token from the macOS **Keychain** (`security find-generic-password -s "Claude Code-credentials"`; fallback `~/.claude/.credentials.json`), POSTs one minimal `/v1/messages` (`anthropic-beta: oauth-2025-04-20`, haiku, max_tokens 1), reads response headers `anthropic-ratelimit-unified-{5h,7d}-{utilization,reset}` + `-5h-status`. **Subscription auth, not API-billed.** Pure header parser is unit-tested. Polls every 30s.
- **Secondary: cost (`get_cost`)** — runs `ccusage@14 blocks --active --json` → `costUSD`/`burnRate.costPerHour`/`projection.totalCost`/`models`/`tokenCounts`. Only polled while the info panel is open. (`costUSD` needs explicit serde `rename` — uppercase acronym.) **Pinned to `@14`**: v15+ ships a native binary that crashes with a nix-libiconv `dyld` error on this Mac; v14 is the last pure-JS release.

## Build / run
- Prereqs: **Rust** (rustup, `cargo 1.96`; non-login shells: `source "$HOME/.cargo/env"`), Xcode CLT, Node v22, ccusage reachable via `npx`.
- Dev: `npm run tauri dev` · Build: `npm run tauri build` (→ `.app`/`.dmg` in `src-tauri/target/release/bundle/`).
- Test: `cargo test --manifest-path src-tauri/Cargo.toml` — 12 unit tests (rate-limit parser, token extraction, ccusage cost/month parsers) are the gate.
- **Install / distribution**: ships as **source only** (public repo, but no installers — bundles Clawd/Anthropic assets). Users build from source: `npm install && npm run tauri build` then run the `.app` (macOS) / AppImage (Linux). **No install script** — people (rightly) don't run unknown `.sh`; the README leads with the transparent `tauri build` + an "ask your Claude Code to build it" path. `bundle.targets` = `["app","appimage"]` (no `.dmg`). CI is `.github/workflows/ci.yml` — `cargo test` gate only. Linux: macOS-only deps (`objc`, `tauri-nspanel`) are target-gated; token falls back to `~/.claude/.credentials.json`; PiP degrades to always-on-top + all-workspaces.
- Perf notes: Tauri commands are async (`spawn_blocking`) — blocking I/O on the main thread froze the UI every poll; ccusage cmd/token/HTTP-agent are cached (`OnceLock`); position saves throttled (1/s + flush on close); animations pre-rendered offscreen + JSON cached; polling decays 30s→2min when utilization is flat.
- Icon: render a Clawd frame to a 1024 PNG (PIL, see git history) → `npm run tauri icon <png>`.
- `CCUSAGE_CMD` / `CLAUDE_CODE_TOKEN` env overrides (the latter handy for headless testing without the Keychain).

### ⚠️ This dev machine's gotchas
- **iCloud-synced `~/Desktop`** corrupts the shell cwd inode mid-session → `getcwd`/`uv_cwd` EPERM for spawned binaries (cargo/npm/node/python). **Workaround: prefix build/run commands with `cd /` and use absolute paths** (e.g. `cd / && npm --prefix <abs> run tauri -- build`). A fresh terminal also resets it. Long-term fix: move the repo out of `~/Desktop`.
- **macOS PATH**: a Finder-launched `.app` only inherits `/usr/bin:/bin:...` — `usage.rs::resolve_program()`/`extra_node_dirs()` resolve `npx` to an absolute path (nvm + homebrew) so the bundled app finds node.
- **set_position is LOGICAL points**, not physical pixels — mixing physical on Retina hides the window off-screen. `lib.rs::top_right_pos` converts via `scale_factor`. Verify window placement with `CGWindowListCopyWindowInfo` (JXA): expect `X≈2264, onscreen=true`.
- First `.app` launch may prompt to allow Keychain access → **Allow**.
- **Window commands need capability permissions**: JS `setSize`/`startDragging`/`close`/etc. silently fail unless granted in `src-tauri/capabilities/default.json` — `core:default` does NOT include the mutating ones. We grant `core:window:allow-{set-size,set-always-on-top,close,start-dragging}`. (Drag and resize were both broken purely from missing perms.)
- **PiP mode** (`apply_pip` in `lib.rs`, macOS): NSWindow `collectionBehavior = CanJoinAllSpaces|FullScreenAuxiliary` (raw `objc`) + re-assert `set_always_on_top` (collectionBehavior resets the level; toggle off→on to force re-apply). **`CGWindowListCopyWindowInfo` reports `layer=0` for all-Spaces windows even though the real NSWindow `level` is 5** — don't trust CGWindowLayer here; read `[nsWindow level]` directly to verify.

## Architecture
- `src-tauri/src/usage.rs` — `get_usage` (rate-limit %, pure `parse_rate_limit`) + `get_cost` (ccusage, pure `parse_cost`) + Keychain token read + PATH resolution. All parsers unit-tested against `tests/fixtures/{active,idle}.json`.
- `src-tauri/src/lib.rs` — window config + `top_right_pos` (logical, origin-monitor) + **position persistence** (`window.json` in `app_config_dir`, saved on `WindowEvent::Moved`, restored in `setup`).
- `src/` (vanilla HTML/CSS/JS) — view state machine `compact|info|creature` (`data-view`), `data-state` overlays, canvas **animation engine** (palette-indexed 20×20 frames from `src/assets/animations/*.json`), `setSize` on view change. Drag via `data-tauri-drag-region="deep"` (buttons block naturally; mascot canvas opts out with `="false"`). Test hooks `__cuwRender`/`__cuwRenderCost`.
- Assets: `src/assets/fonts/` (Tiempos, StyreneB), `src/assets/animations/` (13 Clawd JSON) — from upstream Clawdmeter (private use).

## Docs
- `REFERENCE.md` — origin credit + the DECISION 2026-06-10 (faithful clone, private-only).
- `docs/spec.md`, `docs/implementation-plan.md` — original (cost-based MVP) history; superseded by the rate-limit clone.
- `docs/screenshots/` — compact / info-expanded / creature.

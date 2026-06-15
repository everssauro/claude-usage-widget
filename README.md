# Claude Usage Widget

A tiny always-on-top **macOS desktop widget** for your live **Claude Code subscription usage** — Current (5h) + Weekly (7d) limits at a glance, with an animated Clawd mascot. A software take on the [Clawdmeter](https://github.com/HermannBjorgvin/Clawdmeter) desk dashboard.

![compact](docs/screenshots/compact.png) ![expanded](docs/screenshots/info-expanded.png)

## Install (one command)

```bash
git clone https://github.com/everssauro/claude-usage-widget.git
cd claude-usage-widget
./install.sh
```

That's it — the script checks prerequisites (installs Rust if missing), builds, and drops the app in `/Applications` (launches it too). Re-run `./install.sh` any time to update after `git pull`.

**Prerequisites it can't auto-install:** macOS with **Xcode Command Line Tools** (`xcode-select --install`) and **Node 20+** ([nodejs.org](https://nodejs.org) or `brew install node`). Linux is supported too (the script installs the GTK/WebKit deps via apt).

## Connect your account

- **Have Claude Code signed in on this machine?** It just works — detected automatically.
- **Don't?** Open ⚙ settings → **Account → Sign in with Claude**, approve in the browser, paste the code back. Uses your own Claude (Pro/Max) subscription — **no API key, no API billing**.

## Features

- **Current (5h) + Weekly (7d) usage %** with heat bars, reset timers (and reset clock time), and an **ETA-to-limit** ("limit in 1h 12m") + throttle warning.
- **⤢ expand** → cost / burn rate / projected cost / models / tokens / cache-hit % (via [`ccusage`](https://github.com/ryoppippi/ccusage)) and a **subscription-vs-API-equivalent** value comparison.
- **📌 PiP mode** — floats on top, on every Space, over fullscreen apps (like a video PiP). Toggle off for a normal window.
- **Click the Clawd mascot** → big idle creature; click again to cycle its 13 animations.
- **⚙ settings** — dark / light (Claude palette) theme, 80%-usage notification toggle, plan selector.
- Drag anywhere; remembers its position. **80% macOS notification** so you don't blow your block.

## How it works

- **Usage %** — reads your Claude Code OAuth token (macOS Keychain `Claude Code-credentials`, or the widget's own login) and makes one minimal `/v1/messages` call, reading the `anthropic-ratelimit-unified-*` response headers. Subscription auth, not API-billed.
- **Cost panel** — runs `ccusage@14` against your local `~/.claude` transcripts (offline, only while the panel is open). Needs `node`/`npx` available.

## Credit

The concept, the Clawd pixel-art animations, and the "Usage" screen are from **[HermannBjorgvin/Clawdmeter](https://github.com/HermannBjorgvin/Clawdmeter)** (an ESP32 desk dashboard) — Clawd animations by [@amaanbuilds](https://x.com/amaanbuilds) via [claudepix.vercel.app](https://claudepix.vercel.app). This project reimplements that idea as a native macOS app. See [`REFERENCE.md`](REFERENCE.md).

> ⚠️ **Private / personal use.** Like upstream, this bundles the copyrighted Clawd mascot and proprietary Anthropic fonts — so it is **not for public distribution** and ships no installers, only source. Build it yourself with `./install.sh`.

## Dev

```bash
npm run tauri dev                                  # run with hot reload
cargo test --manifest-path src-tauri/Cargo.toml    # parser/auth unit tests (the gate)
```

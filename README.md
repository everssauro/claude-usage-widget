# Claude Usage Widget (macOS)

A small, always-on-top **native macOS desktop widget** showing live Claude Code usage for the current 5h billing block — glanceable cost, burn rate, and time left.

Built with **Tauri v2** (Rust shell + web UI). Data comes from **[`ccusage`](https://github.com/ryoppippi/ccusage)** (reads local `~/.claude` transcripts — offline, free).

## Status
Design approved, not yet implemented. Start from `docs/implementation-plan.md`.

## Credit / origin
Inspired by **[HermannBjorgvin/Clawdmeter](https://github.com/HermannBjorgvin/Clawdmeter)** — an ESP32 hardware desk dashboard for Claude usage. This is an independent software reimagining (own code, own assets), not a copy. See `REFERENCE.md`.

## Quick start (for the build session)
1. Prereqs: Rust (rustup), Xcode CLT, Node, `ccusage` reachable.
2. Follow `docs/implementation-plan.md` phase by phase.

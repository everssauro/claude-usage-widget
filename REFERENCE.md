# Reference, credit & legal

## Credit
Inspired by **[HermannBjorgvin/Clawdmeter](https://github.com/HermannBjorgvin/Clawdmeter)** — ESP32 desk dashboard for Claude Code usage. This project reimagines that idea as a native macOS widget with **independent code and original assets**.

## What we take vs not
| Upstream | Reuse? | Why |
|---|---|---|
| `firmware/` (ESP32-S3/Arduino/LVGL, C) | ❌ | Hardware-coupled; not portable to macOS. |
| `daemon/` (Python → BLE) | ❌ (concept only) | We use `ccusage` for data. |
| UX idea (glanceable cost, burn rate, "busier as usage climbs") | ✅ concept | Recreated with original visuals. |
| **`ccusage`** (npm, ryoppippi) | ✅ data brain | Computes usage/cost from local transcripts. |

## Data source
`ccusage` — https://github.com/ryoppippi/ccusage
```
npx -y ccusage@latest blocks --active --json
```

## ⚠️ Legal background
Upstream's README says it bundles **proprietary Anthropic fonts** + the **copyrighted Clawd mascot**, used **without permission**, and is **intentionally not licensed**. The original design intent here was a clean-room homage with original assets only.

## 🔴 DECISION 2026-06-10 — faithful clone, PRIVATE ONLY (overrides the clean-room rule)
The owner chose a **faithful Clawdmeter clone**: use the **Clawd mascot pixel animations** (from `claudepix.vercel.app`, by [@amaanbuilds](https://x.com/amaanbuilds)) and the **proprietary Anthropic fonts** (TiemposText, StyreneB), copied from upstream's `assets/`.

Consequences / guardrails:
- **This repo MUST stay PRIVATE.** These assets are copyrighted/unlicensed; distribution (public repo, releasing the `.app`) is **not** authorized. Personal/local use only.
- If a public release is ever wanted: strip the proprietary fonts + Clawd mascot and substitute originals (the clean-homage path), **or** deliberately fork upstream and inherit its (un)licensing — a conscious decision at that point.
- Data source also changed: live **Claude Code OAuth rate-limit %** (subscription, not API-billed), not just `ccusage`. See `CLAUDE.md`.
- Earlier original assets (the meter glyph/icon) are retained in git history if we ever revert to clean-room.

## If we ever want a PUBLIC fork later
Per Ton's plan: if a public release is wanted, fork `HermannBjorgvin/Clawdmeter` on GitHub and place our code there as a fork of his — done deliberately at that point, not now. Keeping this repo independent/private keeps options open and clean.

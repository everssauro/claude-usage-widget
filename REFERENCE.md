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

## 🟢 DECISION 2026-06-15 — PUBLIC, source-only, credited (supersedes the PRIVATE-ONLY rule)
The owner chose to make the repo **public** as a credited reimplementation of Clawdmeter, **keeping the assets as-is** (no clean-up), accepting the same risk posture as upstream + its ~195 forks.

Rationale: going public removes the collaborator-access friction for testers; source-only public forks of Clawdmeter are the de-facto norm and low practical risk.

Guardrails (the mitigations chosen):
- **Source only — NO installers/binaries distributed.** `bundle.targets` excludes `.dmg`; CI runs tests only. Distributing built binaries (vs source) is the step that materially raises exposure, so we don't.
- **No license** on the repo (all rights reserved), mirroring upstream — because it bundles the copyrighted Clawd mascot + proprietary Anthropic fonts (Tiempos = Klim, StyreneB = Commercial Type) used **without permission**. README carries the explicit "not affiliated / personal use / you have been warned" disclaimer.
- **Prominent credit** to Clawdmeter (HermannBjorgvin) + @amaanbuilds/ClaudePix.
- **Acknowledged highest-risk element:** committing the commercial font files (.otf) to a *public* repo is the spiciest part (font foundries care most). The owner accepted this. The clean exit if ever needed: swap the 2 fonts for OFL/free lookalikes (kills that risk, keeps ~90% of the look) — see git history for the earlier original meter glyph too.

(Earlier decisions retained above for history: 2026-06-10 = faithful clone, private-only.)

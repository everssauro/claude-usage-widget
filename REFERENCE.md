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

## ⚠️ Legal — why this is a clean repo, not a fork
Upstream's README says it bundles **proprietary Anthropic fonts** + the **copyrighted Clawd mascot**, used **without permission**, and is **intentionally not licensed**. So:
- This repo carries **none of upstream's code or assets** — it's independent, started clean, credit-only.
- Our app must use **free/system fonts** (`system-ui`, SF Mono) and an **original glyph/mark** — never the Clawd mascot or Anthropic proprietary fonts.

## If we ever want a PUBLIC fork later
Per Ton's plan: if a public release is wanted, fork `HermannBjorgvin/Clawdmeter` on GitHub and place our code there as a fork of his — done deliberately at that point, not now. Keeping this repo independent/private keeps options open and clean.

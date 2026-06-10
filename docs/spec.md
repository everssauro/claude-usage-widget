# Spec — Claude Usage Widget (macOS)

Status: **approved** (brainstorm 2026-06-10). Org: Ton. Personal tool.

## Goal
A small, always-visible **native macOS desktop widget** showing live Claude Code usage for the current 5h billing block — glanceable cost + burn rate + time left. "Desk meter" spirit of Clawdmeter, but software, license-clean, and pretty.

## Form factor (decided)
- **Tauri v2**: Rust shell + web UI (HTML/CSS/JS). Ships as a real `.app`.
- **Floating widget, always-on-top, draggable, no window chrome**, ~280×150, lives in a screen corner. `skipTaskbar`.
- Data brain = **`ccusage`** (we do NOT recompute usage).

Why Tauri (vs SwiftUI / porting upstream C): designer iterates layout fast in CSS; real native `.app`; reuses the hard part (ccusage) instead of rewriting; avoids hardware-coupled, legally-encumbered upstream C.

## Data
```
npx -y ccusage@latest blocks --active --json
```
Map from `blocks[0]` (active block):
| Field | From |
|---|---|
| cost ($) | `costUSD` |
| burn rate | `burnRate.tokensPerMinute`, `burnRate.costPerHour` |
| time elapsed/remaining | now − `startTime`, `endTime` − now (5h) |
| projected cost | `projection.totalCost` |
| tokens | `tokenCounts.{input,output,cacheRead,cacheCreation}` |
| models | `models[]` |
| active? | `isActive` / empty `blocks` |

## Widget content (MVP)
1. **Cost** — big focal number (`$85.30`).
2. **Burn rate** — `tok/min` + `$/h`, color **heats up as burn climbs** (calm→hot). The "busier as usage climbs" echo, done with color — **not** the Clawd mascot.
3. **Time remaining** in the 5h block + progress bar.
4. **Projection** — projected end-of-block cost.
5. **Model(s)** active (compact).

## Aesthetic (license-clean)
- **No Clawd mascot. No Anthropic proprietary fonts.** Free/system fonts (`system-ui`, SF Mono). Original glyph/mark.
- Dark, compact, high-contrast, glanceable. Mockups can be done in-browser when locking the look.

## States / edge cases
- **No active block**: idle — "no active block · last $X".
- **ccusage/node missing**: friendly error + log; install hint.
- **ccusage slow/timeout**: keep last value, subtle "stale" marker.
- **First run**: `npx` may take ~1s; show loading.

## Refresh
Poll every **10s** (configurable). ccusage is offline + free, so polling costs nothing.

## Non-goals (YAGNI MVP)
- No historical charts/daily breakdown (ccusage CLI does that).
- No menu-bar mode, multi-window, settings UI, auto-update, notifications.

## Testing
- **Unit**: Rust parser ccusage JSON → view model, with saved sample JSON (incl. empty/no-active case).
- **Manual**: dev run — live updates, drag, always-on-top, idle + error states; build release `.app`.

## Location
Repo root (`claude-usage-widget/`). Origin credit + legal: `REFERENCE.md`.

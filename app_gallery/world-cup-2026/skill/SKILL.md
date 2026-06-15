---
name: world-cup-2026
description: "Use when building, refreshing, or answering questions with the world-cup-2026 Spur App — a live FIFA World Cup 2026 dashboard combining Polymarket prediction odds with RSSHub news."
---

# world-cup-2026 — Live World Cup Dashboard

A Spur App that fuses two live data sources into one perspective dashboard:

- **Polymarket** (`gamma-api.polymarket.com`) — prediction-market implied odds
  for World Cup 2026 questions (tournament winner, host nations, golden boot).
- **RSSHub** (`rsshub.app`, Google-News fallback) — the World Cup headline feed.

The MCP server (`server/main.py`, on the `spur_app` SDK) does all fetching and
shaping; the entry notebook's frontend cell renders a `<perspective-viewer>`
datagrid + chart over the combined snapshot.

<HARD-GATE>
Operate the app and notebook ONLY through MCP tools. Never paste code or open
files for the user. Before answering ANY World Cup question, pull live data
first with `wc_snapshot` (or `wc_markets` / `wc_news` for one source). Use
`wc_report` for a written briefing. Build and refresh the dashboard with
`notebook_run_cascade`, then read and edit cells through the notebook MCP
tools. Cite the Polymarket `implied_pct` and the headline source for every
claim.
</HARD-GATE>

## The loop

1. **Open** `app.ipynb` in App mode — the host reads `spur-app.json`, grants
   `active_output_scripts` (Perspective loads its WASM with scripts on), and
   spawns the Python MCP plugin.
2. **Verify** the surface is live: call `wc_snapshot` — it returns
   `{ markets, news, kpis }`. If `markets` is empty, Polymarket has no active
   World Cup market matching the keyword yet; widen with
   `wc_markets(keyword="world cup")`.
3. **Render**: `notebook_run_cascade` from the source cells. The dashboard
   frontend cell reads the `wc_markets` / `wc_news` / `wc_kpis` Arrow ports and
   draws the Perspective datagrid (implied odds, volume) + the news table.
4. **Refresh**: re-run the source cells (manually, or arm a cron schedule on
   them) — the cascade re-renders the bound frontend cell with fresh odds.
5. **Brief**: `wc_report` returns a Markdown summary (favorite, top markets,
   headlines) for chat answers.
6. **Pack**: run `notebook_app_doctor` until green, then
   `notebook_export_spur_app` — never hand-roll a `.spurapp`.

## Tools

| Tool | Returns |
|---|---|
| `wc_snapshot` | `{markets[], news[], kpis}` — the whole dashboard payload |
| `wc_markets` | `{markets[]}` — Polymarket rows: question, implied_pct, volume, end_date, url |
| `wc_news` | `{news[]}` — RSSHub/RSS rows: title, link, published, source |
| `wc_report` | `{markdown, kpis}` — a written briefing |

## Notes

- The frontend uses Perspective from CDN — `capabilities.active_output_scripts`
  must stay `true`, and the dashboard has no scripts-off baseline (accepted
  exception for a live interactive dashboard).
- All odds are *market-implied probabilities*, not forecasts. Always say so.

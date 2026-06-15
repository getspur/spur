"""world-cup-2026 — Spur App MCP server (Polymarket + RSSHub).

Live World Cup 2026 dashboard backend. Tools combine Polymarket prediction
odds with RSSHub news and are consumed by the entry notebook's frontend cell
through the vendored TypeScript SDK (`callTool`) and by agents over the
notebook MCP socket.

Built on the `spur_app` SDK — `App` wraps FastMCP; `@app.tool()` registers
each tool; `app.run()` serves stdio. No hand-written protocol code.
"""
from __future__ import annotations

from typing import Any

from spur_app import App

import worldcup

app = App("world-cup-2026")


@app.tool()
def wc_markets(keyword: str = "world cup", limit: int = 60) -> dict[str, Any]:
    """Polymarket markets matching *keyword*, volume-ranked with implied odds.

    Each row carries ``question``, ``implied_pct`` (Yes probability, 0–100),
    ``volume``, ``liquidity``, ``end_date`` and a market ``url``.
    """
    return {"markets": worldcup.fetch_markets(keyword, limit)}


@app.tool()
def wc_news(limit: int = 40) -> dict[str, Any]:
    """Latest World Cup 2026 headlines via the RSSHub datasource (with fallback).

    Returns rows of ``title``, ``link``, ``published`` and ``source``.
    """
    return {"news": worldcup.fetch_news(limit)}


@app.tool()
def wc_snapshot(keyword: str = "world cup", market_limit: int = 60, news_limit: int = 40) -> dict[str, Any]:
    """Full dashboard payload: markets + news + derived KPI cards in one call.

    This is the primary tool the frontend dashboard cell calls each refresh.
    """
    return worldcup.build_snapshot(keyword, market_limit, news_limit)


@app.tool()
def wc_report(keyword: str = "world cup") -> dict[str, Any]:
    """A Markdown briefing combining the favorite, top markets, and headlines."""
    snap = worldcup.build_snapshot(keyword)
    k = snap["kpis"]
    lines = ["# FIFA World Cup 2026 — Live Briefing", ""]
    if k.get("favorite_question"):
        lines.append(f"**Market favorite:** {k['favorite_question']} — **{k['favorite_pct']}%**")
    lines.append(
        f"**Markets tracked:** {k['market_count']} · "
        f"**Total volume:** ${k['total_volume']:,.0f} · "
        f"**Headlines:** {k['news_count']}"
    )
    lines += ["", "## Top markets by volume", ""]
    for m in snap["markets"][:10]:
        pct = f"{m['implied_pct']}%" if m["implied_pct"] is not None else "—"
        lines.append(f"- {m['question']} — {pct} (vol ${m['volume']:,.0f})")
    lines += ["", "## Latest headlines", ""]
    for n in snap["news"][:8]:
        lines.append(f"- [{n['title']}]({n['link']})")
    return {"markdown": "\n".join(lines), "kpis": k}


def main() -> None:
    app.run()


if __name__ == "__main__":
    main()

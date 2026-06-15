"""World Cup 2026 data layer — Polymarket + RSSHub, stdlib only.

This module is the single source of truth for fetching and shaping the two
live data sources the dashboard combines:

* **Polymarket** (`gamma-api.polymarket.com`) — prediction-market odds for
  FIFA World Cup 2026 questions (winner, host performance, golden boot, ...).
* **RSSHub** (`rsshub.app`) — World Cup news, with a Google-News RSS fallback
  so the dashboard always has a headline feed even when the public RSSHub
  instance rate-limits.

Network helpers degrade gracefully (return ``[]`` on failure); the pure
shaping helpers (`shape_market`, `parse_feed_items`, `compute_kpis`) are
unit-tested without touching the network.
"""
from __future__ import annotations

import json
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
from typing import Any

GAMMA_BASE = "https://gamma-api.polymarket.com"
RSSHUB_BASE = "https://rsshub.app"
USER_AGENT = "spur-world-cup-2026/0.1 (+https://spur.dev)"
DEFAULT_KEYWORD = "world cup"

# RSSHub route first (true RSSHub datasource), then a Google-News RSS fallback.
# Both are parsed by the same RSS/Atom reader below.
NEWS_FEEDS = [
    f"{RSSHUB_BASE}/google/news/" + urllib.parse.quote("FIFA World Cup 2026"),
    "https://news.google.com/rss/search?"
    + urllib.parse.urlencode(
        {"q": "FIFA World Cup 2026", "hl": "en-US", "gl": "US", "ceid": "US:en"}
    ),
    "https://feeds.bbci.co.uk/sport/football/rss.xml",
]


# ---------------------------------------------------------------------------
# Transport (stdlib urllib, defensive)
# ---------------------------------------------------------------------------
# Tight timeouts: a hung feed must fail fast rather than block a notebook cell
# run (and the App-mode cascade) waiting on the network.
def _get(url: str, *, timeout: float = 6.0) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=timeout) as resp:  # noqa: S310
        return resp.read()


def _get_json(url: str, *, timeout: float = 8.0) -> Any:
    return json.loads(_get(url, timeout=timeout).decode("utf-8"))


# ---------------------------------------------------------------------------
# Polymarket
# ---------------------------------------------------------------------------
def _as_float(value: Any) -> float | None:
    try:
        if value is None or value == "":
            return None
        return float(value)
    except (TypeError, ValueError):
        return None


def _parse_json_list(value: Any) -> list[Any]:
    """Gamma encodes ``outcomes``/``outcomePrices`` as JSON-in-a-string."""
    if isinstance(value, list):
        return value
    if isinstance(value, str) and value.strip():
        try:
            parsed = json.loads(value)
            return parsed if isinstance(parsed, list) else []
        except ValueError:
            return []
    return []


def shape_market(raw: dict[str, Any]) -> dict[str, Any]:
    """Shape one raw Gamma market dict into a tidy dashboard row.

    Pure function — no network. ``yes_prob`` is the first outcome price
    (Polymarket binary markets order ``["Yes", "No"]``); ``implied_pct`` is
    that probability as a 0–100 percentage for the datagrid.
    """
    outcomes = _parse_json_list(raw.get("outcomes"))
    prices = [_as_float(p) for p in _parse_json_list(raw.get("outcomePrices"))]
    yes_prob = prices[0] if prices else None
    slug = raw.get("slug") or ""
    return {
        "question": (raw.get("question") or raw.get("title") or "").strip(),
        "outcome": outcomes[0] if outcomes else "Yes",
        "yes_prob": yes_prob,
        "implied_pct": round(yes_prob * 100, 1) if yes_prob is not None else None,
        "volume": _as_float(raw.get("volume")) or 0.0,
        "volume_24hr": _as_float(raw.get("volume24hr")) or 0.0,
        "liquidity": _as_float(raw.get("liquidity")) or 0.0,
        "end_date": (raw.get("endDate") or "")[:10],
        "url": f"https://polymarket.com/market/{slug}" if slug else "",
    }


def fetch_markets(keyword: str = DEFAULT_KEYWORD, limit: int = 60) -> list[dict[str, Any]]:
    """Fetch active Polymarket markets whose question matches *keyword*.

    Pulls the volume-ranked active markets from Gamma and filters by keyword
    client-side. Returns ``[]`` on any network/parse failure.
    """
    needle = keyword.lower().strip()
    params = urllib.parse.urlencode(
        {"closed": "false", "active": "true", "limit": 500, "order": "volume", "ascending": "false"}
    )
    try:
        raw = _get_json(f"{GAMMA_BASE}/markets?{params}")
    except Exception:  # noqa: BLE001 — degrade to empty feed
        return []
    rows = [
        shape_market(m)
        for m in (raw if isinstance(raw, list) else [])
        if needle in (m.get("question") or m.get("title") or "").lower()
    ]
    rows.sort(key=lambda r: r["volume"], reverse=True)
    return rows[: max(1, limit)]


# ---------------------------------------------------------------------------
# RSSHub / RSS
# ---------------------------------------------------------------------------
def _tag(elem: ET.Element, *names: str) -> str | None:
    """Return the text of the first matching child tag, namespace-insensitive."""
    for child in elem.iter():
        local = child.tag.rsplit("}", 1)[-1].lower()
        if local in names and child.text and child.text.strip():
            return child.text.strip()
    return None


def _parse_feed_items_regex(xml_text: str, source: str) -> list[dict[str, Any]]:
    """Dependency-free fallback parser (used when expat/ElementTree is absent).

    Handles RSS ``<item>`` and Atom ``<entry>`` blocks well enough to populate
    the headline feed without any XML C-extension.
    """
    import re

    def _between(block: str, *tags: str) -> str | None:
        for tag in tags:
            m = re.search(rf"<{tag}[^>]*>(.*?)</{tag}>", block, re.DOTALL | re.IGNORECASE)
            if m:
                text = re.sub(r"<!\[CDATA\[(.*?)\]\]>", r"\1", m.group(1), flags=re.DOTALL).strip()
                if text:
                    return text
        return None

    items: list[dict[str, Any]] = []
    for m in re.finditer(r"<(item|entry)\b.*?</\1>", xml_text, re.DOTALL | re.IGNORECASE):
        block = m.group(0)
        title = _between(block, "title")
        link = _between(block, "link")
        if not link:
            href = re.search(r'<link[^>]*href="([^"]+)"', block, re.IGNORECASE)
            link = href.group(1) if href else None
        published = _between(block, "pubDate", "published", "updated", "date")
        if title:
            items.append({"title": title, "link": link or "", "published": published or "", "source": source})
    return items


def parse_feed_items(xml_bytes: bytes, *, source: str = "rss") -> list[dict[str, Any]]:
    """Parse RSS 2.0 or Atom bytes into tidy news rows. Pure, no network."""
    try:
        root = ET.fromstring(xml_bytes)
    except ImportError:
        # Environment without the expat C-extension — use the regex fallback.
        return _parse_feed_items_regex(xml_bytes.decode("utf-8", "replace"), source)
    except ET.ParseError:
        fallback = _parse_feed_items_regex(xml_bytes.decode("utf-8", "replace"), source)
        return fallback
    items: list[dict[str, Any]] = []
    for elem in root.iter():
        local = elem.tag.rsplit("}", 1)[-1].lower()
        if local not in ("item", "entry"):
            continue
        title = _tag(elem, "title")
        link = _tag(elem, "link")
        if not link:  # Atom puts the URL in <link href="...">
            for child in elem.iter():
                if child.tag.rsplit("}", 1)[-1].lower() == "link":
                    link = child.attrib.get("href") or link
        published = _tag(elem, "pubdate", "published", "updated", "date")
        if title:
            items.append(
                {
                    "title": title,
                    "link": link or "",
                    "published": published or "",
                    "source": source,
                }
            )
    return items


def fetch_news(limit: int = 40) -> list[dict[str, Any]]:
    """Fetch World Cup news via the RSSHub datasource, falling back across feeds.

    Tries the RSSHub Google-News route first, then plain RSS fallbacks, and
    returns the first feed that yields items. ``[]`` if all fail.
    """
    for url in NEWS_FEEDS:
        try:
            source = "rsshub" if RSSHUB_BASE in url else "rss"
            items = parse_feed_items(_get(url), source=source)
        except Exception:  # noqa: BLE001
            items = []
        if items:
            return items[: max(1, limit)]
    return []


# ---------------------------------------------------------------------------
# Combined snapshot
# ---------------------------------------------------------------------------
def compute_kpis(markets: list[dict[str, Any]], news: list[dict[str, Any]]) -> dict[str, Any]:
    """Derive the headline KPI cards from the two feeds. Pure, no network."""
    priced = [m for m in markets if m.get("implied_pct") is not None]
    favorite = max(priced, key=lambda m: m["implied_pct"], default=None)
    return {
        "market_count": len(markets),
        "news_count": len(news),
        "total_volume": round(sum(m.get("volume", 0.0) for m in markets), 2),
        "favorite_question": favorite["question"] if favorite else None,
        "favorite_pct": favorite["implied_pct"] if favorite else None,
    }


def build_snapshot(keyword: str = DEFAULT_KEYWORD, market_limit: int = 60, news_limit: int = 40) -> dict[str, Any]:
    """Combine Polymarket markets + RSSHub news into one dashboard payload."""
    markets = fetch_markets(keyword, market_limit)
    news = fetch_news(news_limit)
    return {
        "markets": markets,
        "news": news,
        "kpis": compute_kpis(markets, news),
        "keyword": keyword,
    }

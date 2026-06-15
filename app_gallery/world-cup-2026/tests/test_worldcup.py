"""Pure shaping tests for the world-cup-2026 data layer (no network)."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "server"))

import worldcup


def test_shape_market_extracts_implied_probability():
    raw = {
        "question": "Will Brazil win the 2026 World Cup?",
        "slug": "brazil-2026-world-cup",
        "outcomes": '["Yes", "No"]',
        "outcomePrices": '["0.18", "0.82"]',
        "volume": "1234567.5",
        "volume24hr": "9876",
        "liquidity": "5000",
        "endDate": "2026-07-19T00:00:00Z",
    }
    row = worldcup.shape_market(raw)
    assert row["question"] == "Will Brazil win the 2026 World Cup?"
    assert row["outcome"] == "Yes"
    assert row["yes_prob"] == 0.18
    assert row["implied_pct"] == 18.0
    assert row["volume"] == 1234567.5
    assert row["end_date"] == "2026-07-19"
    assert row["url"] == "https://polymarket.com/market/brazil-2026-world-cup"


def test_shape_market_handles_missing_prices():
    row = worldcup.shape_market({"question": "Q", "volume": None})
    assert row["implied_pct"] is None
    assert row["yes_prob"] is None
    assert row["volume"] == 0.0
    assert row["url"] == ""


def test_parse_feed_items_reads_rss_2_0():
    xml = b"""<?xml version="1.0"?>
<rss version="2.0"><channel>
  <title>World Cup News</title>
  <item>
    <title>Host cities announced</title>
    <link>https://example.test/a</link>
    <pubDate>Sat, 13 Jun 2026 09:00:00 GMT</pubDate>
  </item>
  <item>
    <title>Qualifiers wrap up</title>
    <link>https://example.test/b</link>
  </item>
</channel></rss>"""
    items = worldcup.parse_feed_items(xml, source="rsshub")
    assert len(items) == 2
    assert items[0]["title"] == "Host cities announced"
    assert items[0]["link"] == "https://example.test/a"
    assert items[0]["source"] == "rsshub"


def test_parse_feed_items_reads_atom_link_href():
    xml = b"""<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <title>Atom headline</title>
    <link href="https://example.test/atom" rel="alternate"/>
    <updated>2026-06-13T09:00:00Z</updated>
  </entry>
</feed>"""
    items = worldcup.parse_feed_items(xml)
    assert len(items) == 1
    assert items[0]["title"] == "Atom headline"
    assert items[0]["link"] == "https://example.test/atom"


def test_parse_feed_items_tolerates_garbage():
    assert worldcup.parse_feed_items(b"not xml") == []


def test_compute_kpis_picks_favorite_and_totals():
    markets = [
        {"question": "Spain wins?", "implied_pct": 22.0, "volume": 100.0},
        {"question": "France wins?", "implied_pct": 19.0, "volume": 200.0},
        {"question": "No price", "implied_pct": None, "volume": 50.0},
    ]
    news = [{"title": "x"}, {"title": "y"}]
    kpis = worldcup.compute_kpis(markets, news)
    assert kpis["market_count"] == 3
    assert kpis["news_count"] == 2
    assert kpis["total_volume"] == 350.0
    assert kpis["favorite_question"] == "Spain wins?"
    assert kpis["favorite_pct"] == 22.0

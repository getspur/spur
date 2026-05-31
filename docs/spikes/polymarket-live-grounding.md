# Polymarket Live Grounding

Verdict: the existing Polymarket adapter does **not** parse the real API fully correctly as-is before Plan 2; `markets.volume` comes back as a JSON string and is converted to Arrow `NULL`, and `orderbook(token_id, depth)` sends `depth` but the live CLOB response was not limited to that depth. Required changes: parse numeric strings in numeric JSON extraction or point `volume` at numeric `volumeNum`, and either remove/rename the `depth` argument or trim book levels client-side.

Captured on 2026-05-31 with:

```text
scripts/spur-cargo test -p spur-rest-table-gateway --test live_polymarket -- --ignored --nocapture
```

Result: `2 passed; 0 failed; 0 ignored`. A local `curl` from the workspace failed DNS resolution (`Could not resolve host: gamma-api.polymarket.com`), but the repository's remote `spur-cargo` runner had outbound network access and produced the raw output below.

## Raw `/markets` Sample

Request:

```text
GET https://gamma-api.polymarket.com/markets?limit=1&offset=0&active=true&closed=false
```

The live test prints the full market object. This report quotes the adapter-relevant raw fields with values and JSON types preserved:

```json
{
  "id": "540817",
  "question": "New Rihanna Album before GTA VI?",
  "active": true,
  "closed": false,
  "volume": "782375.5482699985",
  "volumeNum": 782375.5482699985,
  "liquidity": "18379.895",
  "liquidityNum": 18379.895,
  "clobTokenIds": "[\"98022490269692409998126496127597032490334070080325855126491859374983463996227\", \"53831553061883006530739877284105938919721408776239639687877978808906551086026\"]",
  "outcomes": "[\"Yes\", \"No\"]",
  "outcomePrices": "[\"0.52\", \"0.48\"]",
  "conditionId": "0x1fad72fae204143ff1c3035e99e7c0f65ea8d5cd9bd1070987bd1a3316f772be",
  "enableOrderBook": true,
  "acceptingOrders": true
}
```

Observed adapter output for the first `markets` row:

```text
schema:
  id: Utf8
  question: Utf8
  active: Boolean
  volume: Float64
first row:
  id = 540817
  question = New Rihanna Album before GTA VI?
  active = true
  volume = NULL
```

The `volume = NULL` value is the key adapter bug: `json_to_batch` uses `serde_json::Value::as_f64()`, and `as_f64()` returns `None` for the live string `"782375.5482699985"`.

Additional live probes:

```text
active=false first row active field: Some(true)
pagination id offset=0: Some("540817")
pagination id offset=1: Some("540818")
```

Interpretation: `limit` and `offset` changed the result as expected. `active=false` did **not** return an inactive first row in this sample, so the adapter should not assume the Gamma `active` parameter supports symmetric boolean filtering. For the adapter's current `active == true` pushdown, the sampled `active=true&closed=false` response did return `active: true` and `closed: false`.

## Raw `/book` Sample

Token id was extracted from `clobTokenIds` by parsing the JSON-encoded string array:

```text
98022490269692409998126496127597032490334070080325855126491859374983463996227
```

Request:

```text
GET https://clob.polymarket.com/book?token_id=98022490269692409998126496127597032490334070080325855126491859374983463996227
```

Adapter-relevant excerpt from the live raw response:

```json
{
  "market": "0x1fad72fae204143ff1c3035e99e7c0f65ea8d5cd9bd1070987bd1a3316f772be",
  "asset_id": "98022490269692409998126496127597032490334070080325855126491859374983463996227",
  "timestamp": "1780234424054",
  "hash": "e6886590d5b80868a2df093387d1a87459212601",
  "bids": [
    {
      "price": "0.01",
      "size": "31445.71"
    },
    {
      "price": "0.02",
      "size": "33035.33"
    },
    {
      "price": "0.03",
      "size": "9"
    }
  ],
  "asks": [
    {
      "price": "0.99",
      "size": "29157.08"
    },
    {
      "price": "0.98",
      "size": "4792.75"
    },
    {
      "price": "0.97",
      "size": "14.31"
    }
  ],
  "min_order_size": "5",
  "tick_size": "0.01",
  "neg_risk": false,
  "last_trade_price": "0.470"
}
```

Observed adapter output for `orderbook(token_id, 10)`:

```text
orderbook rows:
  row 0: price=0.01, size=31445.71
  row 1: price=0.02, size=33035.33
  ...
  row 37: price=0.52, size=18.4
```

Interpretation: the field layout matches the adapter's current `body.bids[].price` and `body.bids[].size` extraction, and prices/sizes are strings that the adapter successfully parses. However, `depth=10` did not produce 10 returned bid rows; the adapter returned 38 bid rows in this run.

## Correctness Table

| Manifest/adapter assumption | Live reality | Status | Required follow-up |
|---|---:|---|---|
| `$.id` exists for markets | Present as `"540817"` | Match | Keep `id: Utf8`. |
| `id` is a string | JSON string | Match | No change. |
| `$.question` exists | Present as `"New Rihanna Album before GTA VI?"` | Match | Keep `question: Utf8`. |
| `question` is a string | JSON string | Match | No change. |
| `$.active` exists | Present as `true` | Match | Keep `active: Boolean`. |
| `active` is a bool | JSON boolean | Match | No change for extraction. |
| `$.volume` is usable as `Float64` | Present as JSON string `"782375.5482699985"` | **Mismatch** | Change manifest to `$.volumeNum` or extend numeric extraction to parse numeric strings. |
| Utf8 columns tolerate non-strings | Live has JSON-encoded strings for fields such as `clobTokenIds`, `outcomes`, `outcomePrices` | Match for current Utf8 behavior | If exposed later, keep as Utf8 or add a structured parser deliberately. |
| `active=true` filter param works for current scan | `active=true&closed=false` returned `active: true`; adapter scan returned rows | Partial match | Current `active == true` pushdown is usable for sampled happy path. |
| `active` filter is a symmetric boolean API filter | `active=false` first row still had `active: true` | **Mismatch/risk** | Do not rely on `active=false` pushdown without more API-specific handling. |
| `limit` and `offset` paginate | `offset=0` first id was `540817`; `offset=1` first id was `540818` | Match | Offset pagination is empirically supported for sampled request. |
| `/book?token_id=<id>` returns top-level `bids` and `asks` arrays | Live response had top-level `bids` and `asks` | Match | Current bid extraction path is valid. |
| `bids[].price` and `bids[].size` are strings | Live response used strings | Match | Current `scan_orderbook` string-or-number parsing is correct. |
| `orderbook` should read bids only | Live response has both bids and asks; adapter reads only bids | Intentional limitation/risk | Plan 2 should decide whether TVF needs side column and asks. |
| `depth` argument limits returned rows | `orderbook(token_id, 10)` returned 38 bid rows | **Mismatch** | Trim client-side or remove/rename `depth` if the API ignores it. |
| `clobTokenIds` is a JSON array | Live field is a JSON-encoded string array | **Mismatch for naive JSON array access** | Parse the string as `Vec<String>` when discovering token ids. |

## Plan-2 Prep: Required Adapter/Manifest Changes

1. Fix numeric market columns before DuckDB VTab integration. Preferred change: extend `json_to_batch` for `Float64` and `Int64` to parse numeric strings, mirroring `scan_orderbook`; this fixes `volume` and future Gamma fields like `liquidity` without changing semantic column types. Narrow alternative: change the Polymarket manifest from `$.volume` to `$.volumeNum`.

2. Treat Gamma JSON-encoded arrays as strings unless a structured column type is deliberately introduced. `clobTokenIds`, `outcomes`, and `outcomePrices` are strings containing JSON arrays, not JSON arrays.

3. Do not expose `active=false` as reliable predicate pushdown based on this sample. The live endpoint returned an `active: true` first row for `active=false`.

4. Decide orderbook side/depth semantics before Plan 2. The live CLOB shape matches `bids[].price/.size` as strings, but the endpoint also returns `asks`, and the adapter's `depth` argument did not cap the returned bid rows.

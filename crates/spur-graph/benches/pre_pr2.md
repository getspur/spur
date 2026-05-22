# Pre-PR2 Parquet Exporter Benchmark Decisions

## Decision Summary

| Decision | Value | Reason |
|---|---:|---|
| `PARQUET_ROW_GROUP_SIZE` | `16384` | Smallest tested row group whose `nodes.parquet` size stayed within 10% of the 64K baseline. |
| `ENCLOSING_SCOPE_DICTIONARY` | `true` | Distinct non-null `enclosing_scope` ratio was 0.106, so the <= 0.5 rule selects DICTIONARY. |
| `EDGES_BY_DST_PRESENT` | `false` | Materialized `dst_id` lookup median was 17.74 ms vs lazy sort 19.00 ms (1.07x); materialize only at >= 2x. |

Concrete constants for Task 3:

```rust
const PARQUET_ROW_GROUP_SIZE: usize = 16384;
const ENCLOSING_SCOPE_DICTIONARY: bool = true;
const EDGES_BY_DST_PRESENT: bool = false;
```

## Fixture

- Path: `/Volumes/Projects/spur/.spur/graph-index.json`
- JSON bytes: 46612076 (44.45 MiB)
- Files: 1557
- Symbols: 27875
- Total edges in JSON: 116561
- Resolved edges used for edge benchmarks: 47771
- Unresolved edges in JSON: 68790
- Note: the legacy JSON fixture does not store extractor `NodeId` values for every endpoint, so the benchmark assigned dense surrogate IDs from stable endpoint IDs. The Parquet column types, sort keys, and query shapes match the spec.

## Row Group Size

| Row group | `nodes.parquet` bytes | MiB | Median write ms | Write samples ms |
|---:|---:|---:|---:|---|
| 16384 | 1723025 | 1.643 | 26.98 | 27.61, 26.64, 25.69, 27.44, 26.98 |
| 32768 | 1710706 | 1.631 | 27.28 | 27.28, 28.48, 25.97, 28.56, 25.75 |
| 65536 | 1710706 | 1.631 | 25.83 | 25.22, 25.91, 25.83, 26.10, 25.78 |

Decision: pick the smallest candidate within 10% of the 64K size baseline, which is the chosen row group above. Write-time differences at this fixture size are recorded but not used as a hard gate.

## `enclosing_scope` Encoding

- DuckDB cardinality rule input: `COUNT(DISTINCT enclosing_scope) / COUNT(*) = 2962 / 27875 = 0.106260`.
| Encoding | Bytes | MiB | Median write ms | Median Rust read ms | Write samples ms | Read samples ms |
|---|---:|---:|---:|---:|---|---|
| DICTIONARY | 1723025 | 1.643 | 26.46 | 8.25 | 25.87, 26.46, 25.75, 26.52, 26.76 | 8.33, 8.23, 8.08, 8.25, 8.28 |
| PLAIN | 1732243 | 1.652 | 27.40 | 7.94 | 28.25, 27.33, 26.60, 27.40, 27.44 | 7.97, 7.86, 8.04, 7.94, 7.78 |

Decision: the ratio is below 0.5, so `enclosing_scope` stays DICTIONARY regardless of the small timing variance.

## `edges_by_dst.parquet`

- Probe `dst_id`: 16037 (622 incoming resolved edges).
- `edges.parquet`: 836541 bytes (0.798 MiB), write 17.69 ms.
- `edges_by_dst.parquet`: 878575 bytes (0.838 MiB), write 17.69 ms.
| Query mode | Median fresh DuckDB process ms | Samples ms |
|---|---:|---|
| materialized | 17.74 | 81.84, 18.46, 17.74, 17.51, 17.59, 18.04, 17.54 |
| lazy | 19.00 | 21.72, 19.77, 19.00, 18.69, 18.46, 19.03, 18.74 |

Decision: materialized `edges_by_dst.parquet` is 1.07x vs lazy; the gate is >= 2x, so `EDGES_BY_DST_PRESENT` is `false`.

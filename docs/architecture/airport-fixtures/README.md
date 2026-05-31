# airport-fixtures

Fixtures backing `../airport-wire-format.md`. Used as golden inputs for Plan 2's `filter.rs`
and `ticket.rs` round-trip tests.

| File | What | Provenance |
|---|---|---|
| `filter_eq_bool.json` | `WHERE active = true` predicate JSON (`json_filters` body) | Reconstructed from DuckDB serialization rules; **shape corroborated** by airport-go's captured test fixtures (incl. `alias:""`, `type_info:null`). |
| `filter_gt_float.json` | `WHERE volume > 0.5` predicate JSON | Same. |
| `filter_in_list.json` | `WHERE id IN ('a','b')` predicate JSON — **`COMPARE_IN` with right side a `BOUND_FUNCTION` named `list_value`** | Shape transcribed from airport-go's captured IN fixture (`filter/parse_test.go`). **CONFIRMED** structure. |
| `filter_and_conjunction.json` | `WHERE status = 'active' AND age > 18` | **Verbatim** from airport-go `filter/parse_test.go` AND fixture. CONFIRMED capture. |
| `filter_is_null.json` | `WHERE deleted_at IS NULL` (`BOUND_OPERATOR`/`OPERATOR_IS_NULL`) | **Verbatim** from airport-go `filter/parse_test.go`. CONFIRMED capture. |
| `ticket_struct.md` | The Flight ticket layout (server-private) — airport-go Go struct + python-flight-server struct, verbatim with tags + file:line | **CONFIRMED** transcription. |
| `doaction_endpoints_body.md` | The `endpoints` DoAction request body — verbatim C++ `MSGPACK_DEFINE_MAP` structs + a real msgpack hex dump of a representative body | **CONFIRMED** struct transcription; hex dump is illustrative (build the real one via wire-format.md §8). |

## Important grammar notes (apply to all `filter_*.json`)

- **`alias` and `type_info` are present, not omitted.** Real DuckDB output puts `"alias": ""`
  on every expression node and `"type_info": null` inside every `return_type`/`value.type`
  `LogicalType` (or a type-info object for `DECIMAL`/`LIST`/`STRUCT`/`ARRAY`/`ENUM`). The fixtures
  include them. A parser should tolerate but may ignore `alias`.
- **`IN` is `COMPARE_IN`, not OR-of-EQ.** DuckDB serializes `col IN (v1,v2)` as a
  `BOUND_COMPARISON`/`COMPARE_IN` whose `right` is a `BOUND_FUNCTION` named `"list_value"` with
  the elements as `BOUND_CONSTANT` `children`. (`COMPARE_NOT_IN` for `NOT IN`.) Source:
  airport-go `filter/types.go` constants + `filter/parse_test.go` IN fixture. An optimizer *may*
  alternatively lower a small `IN` to a `CONJUNCTION_OR` of `COMPARE_EQUAL` — handle both.
- **Column resolution:** resolve a `BOUND_COLUMN_REF` to a name via
  `column_binding_names_by_index[binding.column_index]`. (`binding.table_index` is a per-query
  internal index — do not assert on it.)
- **Multiple top-level `filters[]`** are implicitly AND-ed.

To convert the reconstructed filters → byte-exact captures for the *installed* extension version,
run the capture procedure in `../airport-wire-format.md` §8 and diff (should match modulo JSON
whitespace).

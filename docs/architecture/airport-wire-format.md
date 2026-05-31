# Airport wire-format reference (Plan 2 spike)

> **Status:** research spike — no production code. This is the authoritative byte-level
> reference the Plan 2 implementation tasks (`crates/spur-notebook/flight-gateway` airport
> subset) are written against, so they can be specified with concrete field names/types
> instead of guesses.
>
> **Method:** reverse-engineered from three independent sources and cross-checked. Every
> nonobvious claim is cited with a repo `file:line` or URL. Each claim is tagged
> **CONFIRMED** (read directly from source) or **INFERRED / UNCONFIRMED**.
>
> **Date:** 2026-05-31

## Sources of truth (and how authoritative each is)

| # | Source | Role | What it is ground truth for |
|---|---|---|---|
| **C** | `Query-farm/airport` (the DuckDB C++ extension) — <https://github.com/Query-farm/airport> | The **client**. DuckDB loads it; it *generates* every byte on the wire. | The **wire contract we must match**: DoAction action names + request msgpack bodies, the predicate JSON grammar, FlightInfo `app_metadata`, auth headers, ATTACH/location parsing. |
| **Go** | `hugr-lab/airport-go` — <https://github.com/hugr-lab/airport-go> (`pkg.go.dev/github.com/hugr-lab/airport-go`) | A **server** impl in Go. | A working reference for how a server *answers* the client; verbatim msgpack struct/tag definitions; a real **parser** of the predicate JSON (`filter/` package). |
| **Py** | `Query-farm/python-flight-server` — <https://github.com/Query-farm/python-flight-server> | A **server** impl in Python (by Query.Farm, same author as the extension). | Corroborates action names, msgpack body field names, and response shapes. |
| **Docs** | <https://airport.query.farm/> · <https://airport.query.farm/features.html> · the `server_action_*.html` pages | Prose spec. | High-level feature surface + a few struct definitions quoted in `server_action_*.html`. |

> **arrow-flight (Rust)** crate: <https://docs.rs/arrow-flight> — see §7 *Rust mapping*.

---

## 0. The single most important finding — what is fixed vs. what we get to design

The airport client treats the **Flight ticket as opaque bytes**. The server mints the ticket
(inside the `endpoints` DoAction response, as `FlightEndpoint.ticket`) and receives it back
verbatim in `DoGet`. The client never inspects it. **Proof: the two reference servers use
*different, incompatible* ticket encodings** and both work:

- **airport-go** encodes the ticket as **JSON** (`flight/ticket.go`, struct `TicketData` with `json:` tags). CONFIRMED (Go).
- **python-flight-server** encodes the ticket as **MessagePack** (`flight_handling.py`, `FlightTicketData` packed via `msgpack.packb(model.model_dump())`). CONFIRMED (Py).

Therefore:

| Layer | Who fixes it | Plan 2 obligation |
|---|---|---|
| **DoAction action names** (`endpoints`, `list_schemas`, `catalog_version`, …) | Client (C) | **MUST match exactly** — string-equal. |
| **DoAction request msgpack bodies** (field names + types) | Client (C) | **MUST decode** the exact map keys the client packs. |
| **Predicate JSON grammar** in `json_filters` | Client (C, = DuckDB's own `Expression::Serialize`) | **MUST parse** what DuckDB emits. |
| **`list_schemas` / `catalog_version` / `endpoints` response shapes** | Client parses them (C) | **MUST emit** what the client can parse (zstd+msgpack catalog, msgpack version map, list of serialized `FlightEndpoint`). |
| **FlightInfo `app_metadata`** map | Client parses it (C) | **MUST emit** keys the client reads (`type`, `schema`, `name`, …). |
| **The Flight ticket body** | **Server (us)** — opaque to client | **FREE CHOICE.** We design it. Recommend msgpack-named for Rust ergonomics. |
| **Auth header** `authorization: Bearer <token>` | Client (C) | MUST read it from gRPC metadata. |
| **Location URI** `grpc://` / `grpc+tls://` | Operator (ATTACH) | We bind a gRPC server at that address. |

This is the load-bearing insight for the whole plan: **we only have to *match the client*, and
the client's outputs are the DoAction bodies + the predicate JSON; the ticket is ours.**

---

## 1. Which Flight RPCs airport calls, and when

All confirmed from the C++ extension (source C) and corroborated by both servers.

### 1a. `ATTACH ... (TYPE airport, LOCATION 'grpc://…')`

On attach the extension initializes the catalog:

1. `DoAction("catalog_version")` — get the current catalog version (to decide caching).
   Source C: `src/storage/airport_catalog.cpp`. CONFIRMED.
2. `DoAction("list_schemas")` — fetch the whole catalog (schemas → tables/functions), as a
   zstd-compressed msgpack array of serialized `FlightInfo`. Source C:
   `src/storage/airport_catalog_api.cpp`. CONFIRMED.

`GetSchema` may be called per object during catalog population
(`AirportAPIObjectBase::GetSchema`), but for a normal scan the schema comes from the cached
`FlightInfo`, **not** a separate `GetSchema` call. CONFIRMED (C).

### 1b. Catalog discovery

- `DoAction("list_schemas")` is the catalog (see above).
- `ListFlights` powers `airport_list_flights()` and carries optional filter headers
  `airport-list-flights-filter-catalog` / `airport-list-flights-filter-schema`. CONFIRMED (C).

### 1c. A table scan (`SELECT … FROM schema.table WHERE …`)

1. **Filter pushdown** is computed in `AirportTakeFlightComplexFilterPushdown`
   (`src/airport_take_flight.cpp`), producing the **predicate JSON string** (§4). CONFIRMED (C).
2. `DoAction("flight_info")` for a catalog table (or `GetFlightInfo` for an ad-hoc
   `airport_take_flight(...)` call) → returns a `FlightInfo`. CONFIRMED (C).
3. `DoAction("endpoints")` — the scan-init call. Its msgpack body carries the pushed-down
   filter JSON, the projected `column_ids`, and (for TVFs) the function args. Returns a list of
   serialized `FlightEndpoint`s, each with a (server-private) ticket and a location. CONFIRMED
   (C, Go, Py).
4. `DoGet(ticket)` — for each endpoint whose location is a real `grpc://` URI, stream Arrow
   `RecordBatch`es. (Endpoints with a `data:` URI inline a base64 msgpack payload and skip
   `DoGet`.) CONFIRMED (C, docs `server_action_endpoints.html`).

### 1d. A table-valued function call (`SELECT … FROM schema.fn(args)`)

1. `DoAction("table_function_flight_info")` — bind the TVF; body carries the descriptor, the
   **arguments as an Arrow IPC `RecordBatch`**, and (for in/out TVFs) an input-table schema →
   returns a `FlightInfo`. CONFIRMED (C, Go, Py).
2. `DoAction("endpoints")` — same as a scan, but `table_function_parameters` (Arrow IPC bytes)
   and `table_function_input_schema` are populated. CONFIRMED (C, Go, Py).
3. `DoGet(ticket)` — stream results.

In-out TVFs (table arg in, table out) instead use `DoExchange` with the gRPC header
`airport-operation: table_function_in_out`; the first chunk's `app_metadata` is a msgpack
`TableFunctionParameters`. CONFIRMED (Py `server.py`). **Out of v1 scope** (write/exchange).

### RPC summary table

| Trigger | RPC | Action string | v1 scope |
|---|---|---|---|
| ATTACH init | `DoAction` | `"catalog_version"` | yes |
| ATTACH init / catalog | `DoAction` | `"list_schemas"` | yes |
| `airport_list_flights()` | `ListFlights` | — | yes |
| table bind | `DoAction` | `"flight_info"` | yes |
| ad-hoc `airport_take_flight()` | `GetFlightInfo` | — | yes |
| TVF bind | `DoAction` | `"table_function_flight_info"` | yes |
| scan/TVF init (filters, projection, args) | `DoAction` | `"endpoints"` | yes |
| stream rows | `DoGet` | — | yes |
| per-object schema (catalog build) | `GetSchema` | — | optional |
| column stats | `DoAction` | `"column_statistics"` | skip |
| txn | `DoAction` | `"create_transaction"`, `"get_transaction_status"` | skip |
| DDL | `DoAction` | `"create_schema"`, `"create_table"`, `"add_column"`, … | skip |
| DML | `DoExchange` | header `airport-operation: insert\|update\|delete` | skip |

---

## 2. DoAction read actions — MessagePack body layout

All DoAction request/response bodies are **MessagePack maps** (the C++ extension uses
`MSGPACK_DEFINE_MAP`, i.e. string-keyed). The action name is the Arrow `Action.type` string;
the body is the raw msgpack bytes in `Action.body`. CONFIRMED (C `src/include/airport_macros.hpp`
`AIRPORT_MSGPACK_ACTION_SINGLE_PARAMETER`).

> Each struct below is transcribed verbatim from a reference server (Go/Py); the **field names
> are the wire keys** the C++ client packs. For the canonical C++ struct shapes, the docs pages
> `server_action_*.html` quote several `MSGPACK_DEFINE_MAP` definitions verbatim (cited inline).

### 2a. `catalog_version`

**Request** (msgpack map):
```
{ "catalog_name": <string> }
```
CONFIRMED — Py `parameter_types.py` `class CatalogVersion { catalog_name: str }`; Go anonymous
struct `{ CatalogName string `msgpack:"catalog_name"` }`.

**Response** (msgpack map):
```
{ "catalog_version": <uint64>, "is_fixed": <bool> }
```
CONFIRMED — docs `server_action_catalog_version.html` quotes the C++ struct verbatim:
```cpp
struct GetCatalogVersionResult {
  uint64_t catalog_version;
  bool is_fixed;
  MSGPACK_DEFINE_MAP(catalog_version, is_fixed)
};
```
Also Py `GetCatalogVersionResult` / Go `versionInfo`.

### 2b. `list_schemas`

**Request** (msgpack map):
```
{ "catalog_name": <string> }
```
CONFIRMED — docs `server_action_list_schemas.html`:
```cpp
struct AirportSerializedCatalogSchemaRequest {
  std::string catalog_name;
  MSGPACK_DEFINE_MAP(catalog_name)
};
```
Py `class ListSchemas { catalog_name: str }`; Go `{ CatalogName string `msgpack:"catalog_name"` }`.

**Response** — a **ZStandard-compressed MessagePack** payload. Two nesting layers:

1. Outer wrapper `AirportSerializedCompressedContent` — a msgpack **array** (note: array, not
   map — uses `MSGPACK_DEFINE`): `[ <uint32 uncompressed_len>, <string zstd_bytes> ]`.
   CONFIRMED (Go `internal/serialize/compress.go`; docs say "ZStandard compressed msgpack array").
2. Decompressed payload = `AirportSerializedCatalogRoot` (msgpack map):

```
AirportSerializedCatalogRoot:
  contents:     AirportSerializedContentsWithSHA256Hash   # catalog-level
  schemas:      [ AirportSerializedSchema, ... ]
  version_info: GetCatalogVersionResult                   # {catalog_version, is_fixed}

AirportSerializedSchema:
  name:        string
  description: string
  tags:        map<string,string>
  contents:    AirportSerializedContentsWithSHA256Hash
  is_default:  bool          # present in C++/Go; absent in the Py model — see UNCONFIRMED

AirportSerializedContentsWithSHA256Hash:
  sha256:     string         # hex sha256 of serialized contents
  url:        string | null  # optional external URL
  serialized: bytes  | null  # inline payload: itself a [uint32,string] zstd array whose
                             # decompressed body is a msgpack array of serialized FlightInfo
```
CONFIRMED — Py `server.py` (`AirportSerializedCatalogRoot`, `AirportSerializedSchema`,
`AirportSerializedContentsWithSHA256Hash`, `GetCatalogVersionResult`); Go `flight/doaction.go`
(`serializeSchemaContents`). The innermost `serialized` decompresses to
`[ flight_info.serialize(), ... ]` — a msgpack array of **protobuf-serialized `FlightInfo`
bytes**. CONFIRMED — Py `flight_inventory.py`:
```python
packed_flight_info = msgpack.packb([flight_info.serialize() for flight_info, _meta in schema_items])
```

Each `FlightInfo` in that array carries its table/function identity in `app_metadata` (§3).

### 2c. `endpoints` (the scan-init call — carries filter + projection + TVF args)

**Request** (msgpack map; nested `parameters` map):
```
{
  "descriptor": <bytes>,            # serialized Arrow FlightDescriptor
  "parameters": {
    "json_filters":                 <string>,        # the predicate JSON (§4), "" if none
    "column_ids":                   [<uint64>, ...],  # projected column indices; rowid = 2^64-1
    "table_function_parameters":    <bytes/string>,   # Arrow IPC RecordBatch of TVF args ("" if table)
    "table_function_input_schema":  <bytes/string>,   # Arrow IPC schema (in/out TVFs)
    "at_unit":                      <string>,         # time-travel unit ("" = none)
    "at_value":                     <string>          # time-travel value
  }
}
```
CONFIRMED — C `src/airport_take_flight.cpp`:
```cpp
struct AirportEndpointParameters {
  std::string json_filters;
  std::vector<idx_t> column_ids;
  std::string table_function_parameters;
  std::string table_function_input_schema;
  std::string at_unit;
  std::string at_value;
  MSGPACK_DEFINE_MAP(json_filters, column_ids, table_function_parameters,
                     table_function_input_schema, at_unit, at_value)
};
struct AirportGetFlightEndpointsRequest {
  std::string descriptor;
  AirportEndpointParameters parameters;
  MSGPACK_DEFINE_MAP(descriptor, parameters)
};
```
Mirrored by Go `flight/doaction_metadata.go` (`decodeEndpointsRequest`) and Py
`parameter_types.py` (`Endpoints` / `EndpointsParameters`). Notes:
- `column_ids` are **0-based**; the rowid pseudo-column is `0xFFFFFFFFFFFFFFFF` (`^uint64(0)`).
  CONFIRMED (Go, C).
- `json_filters` is a **JSON string** (not msgpack) — see §4.
- `table_function_parameters` is an **Arrow IPC RecordBatch stream** (not msgpack), cast to a
  byte string. CONFIRMED (C, Go, Py).

**Response** (msgpack array of serialized `FlightEndpoint`):
```
[ <string: protobuf-serialized FlightEndpoint>, ... ]
```
CONFIRMED — Go `flight/doaction_metadata.go` (`endpoints := []string{...}; msgpack.Encode(endpoints)`).
Each `FlightEndpoint` holds the (server-private) ticket and one or more locations.

### 2d. `flight_info` (catalog-table bind)

**Request** (msgpack map):
```
{ "descriptor": <bytes>, "at_unit": <string>, "at_value": <string> }
```
CONFIRMED — Go `flight/doaction_metadata.go` (`handleFlightInfo`); C
`AirportFlightInfoParameters { descriptor, at_unit, at_value; MSGPACK_DEFINE_MAP(...) }`.

**Response:** protobuf-serialized `FlightInfo` bytes directly in `Result.body`. CONFIRMED (Go, C).

### 2e. `table_function_flight_info` (TVF bind)

**Request** (msgpack map):
```
{
  "descriptor":         <bytes>,   # serialized FlightDescriptor
  "parameters":         <bytes>,   # Arrow IPC RecordBatch of the TVF arguments
  "table_input_schema": <bytes>,   # Arrow IPC schema (in/out TVFs); "" otherwise
  "at_unit":            <string>,
  "at_value":           <string>
}
```
CONFIRMED — Go `flight/doaction_functions.go`. The docs `server_action_table_function_flight_info.html`
quote a C++ struct that additionally splits identity into `catalog` / `schema_name` /
`action_name` fields:
```cpp
struct AirportTableFunctionFlightInfoParameters {
  std::string catalog;
  std::string schema_name;
  std::string action_name;
  std::string parameters;          // Arrow IPC RecordBatch bytes (the args)
  std::string table_input_schema;  // Arrow IPC Schema bytes
  std::string at_unit;
  std::string at_value;
  MSGPACK_DEFINE_MAP(catalog, schema_name, action_name, parameters, table_input_schema, at_unit, at_value)
};
```
> **UNCONFIRMED / version-skew:** the Go server decodes `{descriptor, parameters,
> table_input_schema, at_unit, at_value}` while the docs C++ struct uses
> `{catalog, schema_name, action_name, parameters, table_input_schema, at_unit, at_value}`.
> These are two encodings of "which function + its args". Plan 2 should **capture the real
> bytes from the installed extension version** before locking this struct (see §8). The
> *args-as-Arrow-IPC-RecordBatch* convention is firmly CONFIRMED across all three sources.

**Response:** protobuf-serialized `FlightInfo` bytes in `Result.body`. CONFIRMED (Go).

---

## 3. The Flight ticket — server-private; what the FlightInfo `app_metadata` must carry

### 3a. Ticket: our design choice (opaque to the client)

As established in §0, the ticket is server-private. For reference, the two existing servers'
ticket shapes (we can copy either, or design our own):

**airport-go — JSON ticket** (`flight/ticket.go`). CONFIRMED (Go):
```go
type TicketData struct {
    Catalog        string   `json:"catalog,omitempty"`
    Schema         string   `json:"schema"`
    Table          string   `json:"table,omitempty"`
    TableFunction  string   `json:"table_function,omitempty"`
    FunctionParams []byte   `json:"function_params,omitempty"` // Arrow IPC RecordBatch of TVF args
    TimePointUnit  string   `json:"time_point_unit,omitempty"`
    TimePointValue string   `json:"time_point_value,omitempty"`
    Columns        []string `json:"columns,omitempty"`         // resolved projection column names
    Filters        []byte   `json:"filters,omitempty"`         // the predicate JSON (§4)
}
```

**python-flight-server — MessagePack ticket** (`flight_handling.py`). CONFIRMED (Py):
```python
class FlightTicketData(BaseModel):
    flight_name: str               # table/function identity
    json_filters: str | None = None  # predicate JSON (§4)
    column_ids: list[int] | None = None  # projection column IDs
# packed via: msgpack.packb(model.model_dump())  → FlightEndpoint(packed, locations)
```

Both carry the same three load-bearing things: **(1) which table/function, (2) the pushed-down
filter JSON, (3) the column projection**, plus TVF args. Plan 2's `ticket.rs` should carry the
same set, keyed to the existing `ScanRequest` (§7): `table`, `predicates`-source filter JSON,
`projection`, `tvf_args`.

> **Recommendation:** encode our ticket with `rmp_serde::to_vec_named` (msgpack map) — matches
> the python server's convention and round-trips cleanly with `#[derive(Serialize,Deserialize)]`.
> Because it is opaque to the client, golden round-trip tests (encode→decode equality) fully
> cover it; no captured client bytes are needed for the ticket.

### 3b. `FlightInfo.app_metadata` — this IS client-visible and must match

Every `FlightInfo` the client receives (from `list_schemas` / `flight_info` /
`table_function_flight_info`) carries a msgpack-map `app_metadata` the client reads to learn the
object's identity and kind:
```
{
  "type":         <string>,   # "table" | "table_function" | "scalar_function"
  "catalog":      <string>,
  "schema":       <string>,
  "name":         <string>,
  "comment":      <string>,
  "action_name":  <string|null>,  # for TVFs: the DoAction to call for flight_info
                                  # ("table_function_flight_info"); null for plain tables
  "input_schema": <bytes|null>,   # Arrow IPC schema of function arguments (functions only)
  "extra_data":   <any|null>      # scalar fns: msgpack {"stability": "volatile"|...}
}
```
CONFIRMED — Py `flight_inventory.py` `FlightSchemaMetadata.serialize()`; Go `flight/doaction.go`
`serializeSchemaContents`. The Arrow **schema** of the table itself rides in the standard
`FlightInfo.schema` (Arrow IPC), not in `app_metadata`. CONFIRMED.

---

## 4. The predicate JSON grammar (`ScanOptions.Filter` / `json_filters`)

This is the only grammar **fixed by the client and not of our choosing**, so it gets the most
detail. It is produced by `AirportTakeFlightComplexFilterPushdown`
(`src/airport_take_flight.cpp`) which calls DuckDB's own `Expression::Serialize` through a
JSON serializer configured with `serialize_enum_as_string = true` — so every discriminator is a
human-readable string. CONFIRMED (C `src/include/airport_json_serializer.hpp`,
`src/airport_json_serializer.cpp`). airport-go's `filter/` package is a working **parser** of
exactly this JSON. CONFIRMED (Go).

### 4a. Top-level envelope

```json
{
  "filters": [ <expression node>, ... ],
  "column_binding_names_by_index": [ "<col>", ..., "rowid" ]
}
```
- `filters` — array of serialized DuckDB `Expression` trees (implicitly AND-ed together).
- `column_binding_names_by_index` — maps the scan's column indices to names; `"rowid"` for the
  rowid pseudo-column. This is how a server resolves the numeric `binding.column_index` inside
  an expression back to a column name.

CONFIRMED — C `AirportTakeFlightComplexFilterPushdown` writes exactly these two keys; Py
`FilterData { filters: list, column_binding_names_by_index: list[str] }`; doc page
`server_predicate_pushdown.html` (referenced by Go `catalog/types.go`).

### 4b. Expression node shapes

Every node carries DuckDB's base `Expression` fields then class-specific fields. The
discriminators are `expression_class` (the node category) and `type` (the `ExpressionType`
enum). CONFIRMED — generated serializers in DuckDB
`src/storage/serialization/serialize_expression.cpp`.

**Base fields on every node** (CONFIRMED present in airport-go's captured fixtures):
```
"expression_class": string   # BOUND_COMPARISON | BOUND_CONJUNCTION | BOUND_CONSTANT |
                             # BOUND_COLUMN_REF | BOUND_OPERATOR | BOUND_FUNCTION |
                             # BOUND_CAST | BOUND_BETWEEN | BOUND_CASE | ...
"type":             string   # ExpressionType, e.g. COMPARE_EQUAL, CONJUNCTION_AND, COMPARE_IN, ...
"alias":            string   # present on EVERY node, usually "" (decorative; safe to ignore)
```
> `alias` is **always emitted** (confirmed in every airport-go test fixture); it is not omitted.
> Likewise every `LogicalType` carries `"type_info"` — `null` for scalars, or a type-info object
> (`{"type":"DECIMAL_TYPE_INFO","width":10,"scale":2}`, `LIST_TYPE_INFO`, `STRUCT_TYPE_INFO`,
> `ARRAY_TYPE_INFO`, `ENUM_TYPE_INFO`) for parameterized types.

**Comparison** — `BOUND_COMPARISON` (left = column ref, right = constant):
```
"expression_class": "BOUND_COMPARISON"
"type":  one of COMPARE_EQUAL | COMPARE_NOTEQUAL | COMPARE_LESSTHAN |
         COMPARE_LESSTHANOREQUALTO | COMPARE_GREATERTHAN | COMPARE_GREATERTHANOREQUALTO |
         COMPARE_DISTINCT_FROM | COMPARE_NOT_DISTINCT_FROM
"left":  <BOUND_COLUMN_REF node>
"right": <BOUND_CONSTANT node>
```

**Column reference** — `BOUND_COLUMN_REF`:
```
"expression_class": "BOUND_COLUMN_REF"
"type":             "BOUND_COLUMN_REF"
"alias":            ""
"return_type":      { "id": <LogicalTypeID>, "type_info": null }
"binding":          { "table_index": <int>, "column_index": <int> }
"depth":            <int>   # 0 for a normal scan
```
Resolve the column **name** via `column_binding_names_by_index[binding.column_index]`.
CONFIRMED — airport-go `filter/parse.go` `rawColumnRef`; index rule confirmed across all
captured fixtures (e.g. `column_index:0` → first entry).

**Constant** — `BOUND_CONSTANT`:
```
"expression_class": "BOUND_CONSTANT"
"type":             "VALUE_CONSTANT"
"alias":            ""
"value":            <Value>
```

**Value:**
```
"type":    { "id": <LogicalTypeID>, "type_info": null }   # BOOLEAN|INTEGER|BIGINT|DOUBLE|VARCHAR|...
"is_null": <bool>
"value":   <primitive>             # absent when is_null=true; bool / number / string per type
```
Value-`id` → JSON primitive: `BOOLEAN`→bool, `TINYINT/SMALLINT/INTEGER/BIGINT`→number,
`FLOAT/DOUBLE`→number, `VARCHAR/CHAR/UUID`→string, `BLOB`→base64 `{"base64":"..."}`,
timestamps→epoch integer. CONFIRMED — airport-go `filter/parse.go` value dispatch.

**Conjunction** — `BOUND_CONJUNCTION`:
```
"expression_class": "BOUND_CONJUNCTION"
"type":             "CONJUNCTION_AND" | "CONJUNCTION_OR"
"children":         [ <expression node>, ... ]
```

**IN / NOT IN** — `BOUND_COMPARISON` with `type` = `COMPARE_IN` / `COMPARE_NOT_IN`. The `right`
side is **not** a constant but a `BOUND_FUNCTION` named `"list_value"` whose `children` are the
`BOUND_CONSTANT` list elements:
```
"expression_class": "BOUND_COMPARISON"
"type":             "COMPARE_IN"          # or COMPARE_NOT_IN
"left":             <BOUND_COLUMN_REF>
"right": { "expression_class": "BOUND_FUNCTION", "type": "BOUND_FUNCTION",
           "name": "list_value", "return_type": {"id":"LIST","type_info":null},
           "children": [ <BOUND_CONSTANT>, ... ] }
```
CONFIRMED — airport-go `filter/types.go` (`TypeCompareIn`/`TypeCompareNotIn`) + captured IN
fixture in `filter/parse_test.go`. **Correction to an earlier C++-only reconstruction** that
guessed OR-of-EQ: the *Expression* serialization keeps `IN` as `COMPARE_IN`+`list_value`. (An
optimizer may still lower a small `IN` to `CONJUNCTION_OR` of `COMPARE_EQUAL` in some plans —
handle both shapes.)

**IS NULL / IS NOT NULL** — `BOUND_OPERATOR`:
```
"expression_class": "BOUND_OPERATOR"
"type":             "OPERATOR_IS_NULL" | "OPERATOR_IS_NOT_NULL"
"return_type":      { "id": "BOOLEAN", "type_info": null }
"children":         [ <BOUND_COLUMN_REF node> ]
```

**Other confirmed node kinds** (airport-go parses them; **residualize for v1**):
- `BOUND_CAST` / `type:"CAST"` — `{child, return_type, try_cast}` (e.g. `CAST(value AS INTEGER) > 10`).
- `BOUND_FUNCTION` / `type:"BOUND_FUNCTION"` — `{name, children, arguments, ...}` (e.g.
  `lower(name) = 'john'`, `struct_extract`). Column-side function ⇒ not a plain column predicate.
- `BOUND_BETWEEN` / `type:"COMPARE_BETWEEN"` — `{input, lower, upper, lower_inclusive, upper_inclusive}`.
- `BOUND_CASE` / `type:"CASE_EXPR"` — `{case_checks:[{when_expr,then_expr}], else_expr}`.
CONFIRMED — airport-go `filter/types.go` + `filter/parse_test.go` fixtures.

CONFIRMED enum strings (from DuckDB's serializer; airport-go matches on the same constants in
`filter/duckdb.go`): `BOUND_COMPARISON`, `BOUND_CONJUNCTION`, `BOUND_CONSTANT`,
`BOUND_COLUMN_REF`, `BOUND_OPERATOR`, `BOUND_FUNCTION`, `COMPARE_EQUAL`, `COMPARE_NOTEQUAL`,
`COMPARE_LESSTHAN`, `COMPARE_LESSTHANOREQUALTO`, `COMPARE_GREATERTHAN`,
`COMPARE_GREATERTHANOREQUALTO`, `CONJUNCTION_AND`, `CONJUNCTION_OR`, `OPERATOR_IS_NULL`,
`OPERATOR_IS_NOT_NULL`, `VALUE_CONSTANT`.

### 4c. Worked examples

See `airport-fixtures/` for the standalone JSON files. The three required examples:

1. **`active = true`** (boolean eq) → `filter_eq_bool.json`
2. **`volume > 0.5`** (double gt) → `filter_gt_float.json`
3. **`id IN ('a','b')`** (varchar in-list = `COMPARE_IN` + `list_value` `BOUND_FUNCTION`) →
   `filter_in_list.json`

Bonus golden fixtures transcribed **verbatim** from airport-go's test suite:
`filter_and_conjunction.json` (`status='active' AND age>18`), `filter_is_null.json`
(`deleted_at IS NULL`).

> **Fixture provenance:** examples 1–2 are reconstructed from DuckDB's serialization rules, and
> their **shape is corroborated** by airport-go's captured test fixtures (matching `alias:""`,
> `type_info:null`, and the exact node layout). Example 3 and the two bonus fixtures are
> **transcribed verbatim from airport-go `filter/parse_test.go`** (a real parser's test corpus) —
> CONFIRMED structure. They are not guaranteed byte-identical to your *installed* extension
> version's JSON whitespace; run the §8 capture and diff before locking byte-level assertions.
> `binding.table_index` is a per-query index — assert via `column_binding_names_by_index` +
> `column_index`, never on `table_index`.

---

## 5. Auth and location/grpc URL

### 5a. Bearer auth — CONFIRMED (C)

The extension adds the token to **every** RPC as gRPC metadata
(`src/airport_request_headers.cpp`):
```cpp
options.headers.emplace_back("authorization", "Bearer " + auth_token);
```
- Header key: `"authorization"` (lowercase). Value: `"Bearer " + token` (single space).
- Token resolution (three-tier, `src/airport_secrets.cpp`): inline `auth_token='…'` on ATTACH →
  named `secret='…'` (a `KeyValueSecret` with key `auth_token`) → path-scoped secret
  (`SecretManager::LookupSecret(..., "airport")`). Secret `TYPE` is `airport`; scope must start
  with `grpc://` or `grpc+tls://`. CONFIRMED (C).
- Server side: read `authorization` from gRPC metadata, strip `"Bearer "`. CONFIRMED — Go
  `auth/auth.go` (`TokenFromAuthorizationHeader`), Py `middleware.py` (case-insensitive key
  match, `partition(" ")`, check scheme `== "Bearer"`).

Other standard request headers the client sends (`src/airport_request_headers.cpp`, CONFIRMED):
`airport-user-agent` (`airport/<date>`), `authority`, `airport-client-session-id` (UUID/process),
`airport-trace-id` (UUID/op), `airport-catalog` (catalog name), `airport-flight-path`
(`/`-joined PATH descriptor parts). Go mirrors these constants in `flight/context.go`
(`HeaderAuthorization="authorization"`, `HeaderCatalog="airport-catalog"`, …). For v1 the
gateway only needs to **read/validate `authorization`**; the rest are informational.

### 5b. Location / grpc URL — CONFIRMED (C)

```sql
ATTACH '' AS db   (TYPE airport, LOCATION 'grpc+tls://server.example.com/database_name');
ATTACH '' AS local(TYPE airport, LOCATION 'grpc://localhost:8815/mydb', auth_token='xxx');
```
- Scheme `grpc://` = plaintext, `grpc+tls://` = TLS. The path after host (`/database_name`) is
  the catalog/database name; `location` = scheme+host. Parsed in `src/airport_extension.cpp`.
  CONFIRMED (C).
- For Plan 2 (loopback, in-process): bind a tonic gRPC server on `127.0.0.1:PORT` and
  `ATTACH '' (TYPE airport, LOCATION 'grpc://127.0.0.1:PORT/<source>', auth_token='<startup>')`.
- Server-emitted endpoint/flight locations use `grpc://host:port` (or `grpc+tls://…`). An empty
  endpoint location means "redeem the ticket on this same service" — airport-go normalizes
  `""` → `flight.LocationReuseConnection`. CONFIRMED (Go `flight/server.go`). The Rust
  equivalent is the URI `arrow-flight-reuse-connection://?` (Py default in
  `flight_handling.endpoint()`). CONFIRMED (Py).

---

## 6. Confirmed-vs-inferred ledger

| Claim | Status | Primary cite |
|---|---|---|
| Ticket is opaque/server-private (JSON in Go, msgpack in Py) | CONFIRMED | Go `flight/ticket.go`; Py `flight_handling.py` |
| DoAction action name strings | CONFIRMED | C dispatch + Py `ActionType` enum |
| `list_schemas`/`catalog_version`/`endpoints` request map keys | CONFIRMED | C structs + Go/Py mirrors |
| `endpoints.parameters` carries filter/projection/TVF-args | CONFIRMED | C `AirportEndpointParameters` |
| TVF args = Arrow IPC RecordBatch | CONFIRMED | C, Go, Py |
| Predicate JSON envelope `{filters, column_binding_names_by_index}` | CONFIRMED | C serializer + Py `FilterData` |
| Expression class / comparison enum strings | CONFIRMED | DuckDB `serialize_expression.cpp`; Go `filter/types.go`, `filter/duckdb.go` |
| `alias:""` + `type_info` present on every node/type | CONFIRMED | airport-go `filter/parse_test.go` captures |
| `IN` = `COMPARE_IN` + `BOUND_FUNCTION` `"list_value"` (not OR-of-EQ) | CONFIRMED | airport-go `filter/types.go`, IN fixture in `filter/parse_test.go` |
| `app_metadata` map keys for FlightInfo | CONFIRMED | Py `flight_inventory.py`; Go `serializeSchemaContents` |
| `authorization: Bearer <token>` header | CONFIRMED | C `airport_request_headers.cpp` |
| `grpc://` / `grpc+tls://` location parsing | CONFIRMED | C `airport_extension.cpp` |
| Worked-example JSON byte exactness (whitespace) vs installed version | INFERRED | shape corroborated by Go captures; run §8 to byte-pin |
| `table_function_flight_info` exact map keys (Go vs docs C++ differ) | UNCONFIRMED (version skew) | Go vs docs `server_action_table_function_flight_info.html` |
| `AirportSerializedSchema.is_default` presence | UNCONFIRMED | C/Go have it; Py model omits it |
| `column_statistics` body | out of scope (skip in v1) | Go `flight/doaction_statistics.go` |

---

## 7. Rust mapping (for Plan 2 tasks)

### 7a. Crates (version-pinned at spike time)

```toml
[dependencies]
arrow-flight = "58"     # FlightService trait, FlightInfo, FlightDataEncoderBuilder; pulls arrow-* ^58
tonic        = "0.14"   # gRPC; interceptor for Bearer auth
rmp-serde    = "1.3"    # MessagePack (serde) — USE to_vec_named for map-encoded structs
serde        = { version = "1", features = ["derive"] }
serde_json   = "1"      # parse the predicate JSON
futures      = "0.3"
tokio        = { version = "1", features = ["full"] }
bytes        = "1"
zstd         = "0.13"   # list_schemas response compression
```
> Pin against the workspace's existing `arrow`/`tonic` versions to avoid a second arrow in the
> tree — check `cargo tree` before fixing the major. arrow-flight major must equal the arrow
> major already used by the duckdb/arrow path. (Marked: VERIFY at task time.)

### 7b. Server skeleton

Implement `arrow_flight::flight_service_server::FlightService` (the tonic-generated trait).
Required methods (signatures from docs.rs/arrow-flight): `handshake`, `list_flights`,
`get_flight_info`, `poll_flight_info`, `get_schema`, `do_get`, `do_put`, `do_exchange`,
`do_action`, `list_actions`. v1 implements `do_action` (dispatch on `Action.r#type`),
`get_flight_info`, `do_get`, `list_flights`, `handshake`; the write/exchange ones return
`Status::unimplemented`.

- **Schema → FlightInfo:** `FlightInfo::new().try_with_schema(&schema)?.with_endpoint(...)`.
- **Stream rows in `do_get`:** `FlightDataEncoderBuilder::new().with_schema(schema).build(stream)`.
- **Auth:** `FlightServiceServer::with_interceptor(svc, auth_interceptor)` reading
  `req.metadata().get("authorization")`, `strip_prefix("Bearer ")`.

### 7c. MessagePack ⇄ Rust

- **Decode client DoAction bodies** (map-encoded): define structs with
  `#[derive(Deserialize)]` and decode with `rmp_serde::from_slice`. Field names must equal the
  wire keys (§2): e.g.
  ```rust
  #[derive(Deserialize)]
  struct EndpointsParameters {
      json_filters: String,
      column_ids: Vec<u64>,
      table_function_parameters: serde_bytes::ByteBuf,
      table_function_input_schema: serde_bytes::ByteBuf,
      at_unit: String,
      at_value: String,
  }
  #[derive(Deserialize)]
  struct EndpointsRequest { descriptor: serde_bytes::ByteBuf, parameters: EndpointsParameters }
  ```
- **Encode our ticket** (server-private): `#[derive(Serialize,Deserialize)]` + `to_vec_named`.
  Round-trip golden test only (no client bytes needed).
- **Encode `list_schemas` / `app_metadata` responses** (client-parsed, map-keyed):
  `rmp_serde::to_vec_named`. The `list_schemas` outer wrapper is an **array** `[len, zstd_bytes]`
  → build with `to_vec` (tuple) over a `(u32, ByteBuf)` or pack manually; compress the inner
  payload with `zstd`.

> **Gotcha (CONFIRMED docs.rs/rmp-serde):** `rmp_serde::to_vec` encodes structs as positional
> arrays; `rmp_serde::to_vec_named` encodes them as string-keyed maps. The client uses
> `MSGPACK_DEFINE_MAP` (named) for bodies, so **use `to_vec_named` for anything the client
> decodes**; `from_slice` accepts both.

### 7d. Predicate JSON → existing `crate::adapter::Predicate`

Target types already exist in `crates/spur-notebook/flight-gateway/src/adapter/mod.rs:15-38`:
```rust
pub enum ScalarValue { Utf8(String), Int64(i64), Float64(f64), Bool(bool) }
pub enum PredicateOp { Eq, Ne, Lt, Le, Gt, Ge }
pub struct Predicate { pub column: String, pub op: PredicateOp, pub value: ScalarValue }
// consumed by ScanRequest { table, predicates, projection, tvf_args, auth } (mod.rs:53)
```

Approach: parse `json_filters` into `serde_json::Value` and **tree-walk** (the grammar is
client-defined and may evolve, so a dynamic walk with a residual fallback is safer than a
`#[serde(tag)]` enum). Return both pushed predicates and a residual list; **anything not
representable as a `Predicate` is dropped to residual** — DuckDB re-applies the full filter
after `DoGet`, so correctness never depends on pushdown completeness.

Mapping rules:
- `BOUND_COMPARISON` with `left=BOUND_COLUMN_REF`, `right=BOUND_CONSTANT` →
  `Predicate { column: column_binding_names_by_index[left.binding.column_index], op: map(type), value: from(right.value) }`.
  `map`: `COMPARE_EQUAL→Eq`, `COMPARE_NOTEQUAL→Ne`, `COMPARE_LESSTHAN→Lt`,
  `COMPARE_LESSTHANOREQUALTO→Le`, `COMPARE_GREATERTHAN→Gt`, `COMPARE_GREATERTHANOREQUALTO→Ge`.
- `value` → `ScalarValue` by `value.type.id`: `BOOLEAN→Bool`, `INTEGER`/`BIGINT`→`Int64`,
  `FLOAT`/`DOUBLE`→`Float64`, `VARCHAR`→`Utf8`. `is_null=true` → residual (no null op in
  `PredicateOp`).
- `BOUND_CONJUNCTION`/`CONJUNCTION_AND` → recurse into `children`; each child independently
  pushed or residualized (top-level `filters[]` are already implicitly AND).
- `BOUND_COMPARISON`/`COMPARE_IN` (the `IN` shape): `right` is a `BOUND_FUNCTION` named
  `"list_value"` with `BOUND_CONSTANT` children. **v1: residualize** (simplest, still correct),
  or later add an `InList { column, values: Vec<ScalarValue> }` predicate variant and push as one
  node. Do **not** flatten into multiple `Eq` `Predicate`s in the AND-combined `predicates` vec —
  that would change semantics (they must be OR-combined). Same for `CONJUNCTION_OR`.
- `BOUND_OPERATOR` (`OPERATOR_IS_NULL`/`IS_NOT_NULL`), `BOUND_CAST`, `BOUND_FUNCTION` (incl.
  column-side `lower(...)`/`struct_extract`), `BOUND_BETWEEN`, `BOUND_CASE`, anything else →
  residual.

This lives in Plan 2's `filter.rs` behind golden tests using the `airport-fixtures/*.json` here.

---

## 8. Capturing real bytes (recommended before locking structs)

The predicate-JSON fixtures here are reconstructed, and `table_function_flight_info` shows a
Go-vs-docs field skew. Before Plan 2 freezes its structs, capture ground truth from the actually
installed extension version with a tiny logging Flight server (or tcp capture):

1. Stand up a throwaway Flight server (Go airport-go `examples/filter` or Py
   python-flight-server) that **logs the raw `Action.body` bytes** for `endpoints` and the raw
   `json_filters` string.
2. From DuckDB: `INSTALL airport; LOAD airport; ATTACH '' (TYPE airport, LOCATION 'grpc://127.0.0.1:PORT/db');`
   then run `SELECT … WHERE active = true`, `… WHERE volume > 0.5`, `… WHERE id IN ('a','b')`.
3. Dump the logged `json_filters` → diff against `airport-fixtures/filter_*.json` (should match
   modulo `alias`/`query_location` and `binding.table_index`).
4. Dump the `endpoints` `Action.body` msgpack → confirm the map keys in §2c; hex-dump one and
   store alongside `airport-fixtures/doaction_endpoints_body.md`.

This converts the remaining INFERRED items to CONFIRMED with version-pinned evidence.

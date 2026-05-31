# Flight ticket structure (server-private)

> **Key fact:** the airport client treats the Flight ticket as **opaque bytes** — the server
> mints it (in the `endpoints` DoAction response, as `FlightEndpoint.ticket`) and receives it
> back verbatim in `DoGet`. The two reference servers use **different, mutually-incompatible**
> ticket encodings (JSON vs MessagePack), which is the proof that the ticket format is
> **our free design choice**, not part of the airport wire contract. See
> `../airport-wire-format.md` §0 / §3a.

Both transcriptions below are **CONFIRMED** (read verbatim from source). They are recorded here
as design references for Plan 2's `ticket.rs`.

## airport-go — JSON-encoded ticket

`flight/ticket.go` — <https://raw.githubusercontent.com/hugr-lab/airport-go/main/flight/ticket.go>

```go
// TicketData is the decoded content of a Flight ticket. Encoding: JSON (json.Marshal).
type TicketData struct {
    Catalog        string   `json:"catalog,omitempty"`
    Schema         string   `json:"schema"`
    Table          string   `json:"table,omitempty"`           // set XOR TableFunction
    TableFunction  string   `json:"table_function,omitempty"`
    FunctionParams []byte   `json:"function_params,omitempty"` // Arrow IPC RecordBatch of TVF args
    TimePointUnit  string   `json:"time_point_unit,omitempty"`
    TimePointValue string   `json:"time_point_value,omitempty"`
    Columns        []string `json:"columns,omitempty"`         // resolved projection column NAMES
    Filters        []byte   `json:"filters,omitempty"`         // the predicate JSON (filter_*.json)
}
```

Notes (CONFIRMED, airport-go):
- `Table` XOR `TableFunction` — exactly one is set.
- `FunctionParams` carries TVF arguments as an Arrow IPC record-batch stream.
- `Columns` is `[]string` — the server has **already resolved** the projection `column_ids`
  (from the `endpoints` request) to names before minting the ticket.
- `Filters` is the raw DuckDB predicate JSON bytes (§4 of the wire-format ref).

## python-flight-server — MessagePack-encoded ticket

`flight_handling.py` — <https://raw.githubusercontent.com/Query-farm/python-flight-server/master/src/query_farm_flight_server/flight_handling.py>

```python
class FlightTicketData(BaseModel):
    flight_name: str                      # table/function identity
    json_filters: str | None = None       # the predicate JSON (filter_*.json)
    column_ids: list[int] | None = None   # projection column IDs (NOT yet resolved to names)

# minted:    packed = msgpack.packb(ticket_data.model_dump()); FlightEndpoint(packed, locations)
# consumed:  FlightTicketData.unpack(ticket.ticket)   # msgpack.unpackb(raw=True, object_hook=...)
```

Notes (CONFIRMED, Py):
- Encoded with `msgpack.packb(model.model_dump())` → map-keyed msgpack.
- Carries the projection as raw `column_ids` (resolution deferred), unlike airport-go which
  resolves to names. Both are valid because the server reads its own ticket back.

## Recommendation for Plan 2 `ticket.rs`

Carry the same load-bearing set, aligned to the existing `ScanRequest`
(`crates/spur-notebook/flight-gateway/src/adapter/mod.rs:53`):

```rust
#[derive(serde::Serialize, serde::Deserialize)]
struct Ticket {
    schema: String,
    table: Option<String>,            // table XOR table_function
    table_function: Option<String>,
    filters: String,                  // predicate JSON, "" if none
    projection: Vec<String>,          // resolved column names (airport-go style)
    tvf_args: Vec<u8>,                // Arrow IPC RecordBatch of TVF args, empty if none
}
// encode: rmp_serde::to_vec_named(&t)?     decode: rmp_serde::from_slice(&ticket.ticket)?
```

Because it is opaque to the client, a **golden round-trip test** (encode→decode equality) fully
covers it — no captured client bytes are required for the ticket (unlike the DoAction bodies and
the predicate JSON, which the client generates).

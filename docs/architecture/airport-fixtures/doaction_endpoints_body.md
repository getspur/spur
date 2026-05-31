# DoAction `endpoints` request body — struct + hex dump

The `endpoints` DoAction is the scan-init call. Its `Action.type == "endpoints"` and
`Action.body` is a **MessagePack map** carrying the pushed-down filter JSON, the projected
column ids, and (for TVFs) the function args. See `../airport-wire-format.md` §2c.

## Authoritative C++ struct (CONFIRMED)

Verbatim from the DuckDB airport extension, `src/airport_take_flight.cpp`
(<https://github.com/Query-farm/airport>). The `MSGPACK_DEFINE_MAP` macro makes these
**string-keyed maps** on the wire.

```cpp
struct AirportEndpointParameters {
  std::string json_filters;                 // predicate JSON (see filter_*.json), "" if none
  std::vector<idx_t> column_ids;            // projected column indices; rowid = 0xFFFFFFFFFFFFFFFF
  std::string table_function_parameters;    // Arrow IPC RecordBatch of TVF args ("" for a table)
  std::string table_function_input_schema;  // Arrow IPC schema (in/out TVFs)
  std::string at_unit;                      // time-travel unit ("" = none)
  std::string at_value;                     // time-travel value
  MSGPACK_DEFINE_MAP(json_filters, column_ids, table_function_parameters,
                     table_function_input_schema, at_unit, at_value)
};

struct AirportGetFlightEndpointsRequest {
  std::string descriptor;                   // serialized Arrow FlightDescriptor
  AirportEndpointParameters parameters;
  MSGPACK_DEFINE_MAP(descriptor, parameters)
};
```

Corroboration: Go `flight/doaction_metadata.go` `decodeEndpointsRequest` (msgpack tags
`descriptor`, and nested `json_filters`/`column_ids`/`table_function_parameters`/
`table_function_input_schema`/`at_unit`/`at_value`); Py `parameter_types.py` `Endpoints` /
`EndpointsParameters`.

## Representative hex dump (ILLUSTRATIVE — see note)

> **Provenance:** produced by a minimal spec-correct MessagePack encoder over a representative
> body for `SELECT question, volume FROM polymarket.markets WHERE active = true`
> (`column_ids = [1,3]`, the exact `filter_eq_bool.json` predicate string, no TVF args). The
> `descriptor` is a short placeholder (`0x12 0x07 "markets"`) standing in for a real serialized
> `FlightDescriptor`. The **structure and key names are CONFIRMED**; exact bytes (esp. the
> descriptor and the JSON whitespace) are illustrative. Capture the real bytes via
> `../airport-wire-format.md` §8.

Total length: 645 bytes (the embedded `json_filters` string is 503 bytes, str16 = `da 01 f7`).

```
0000: 82 aa 64 65 73 63 72 69 70 74 6f 72 c4 09 12 07   .ªdescriptor.Ä..
0010: 6d 61 72 6b 65 74 73 aa 70 61 72 61 6d 65 74 65   marketsªparamete
0020: 72 73 86 ac 6a 73 6f 6e 5f 66 69 6c 74 65 72 73   rs..json_filters
0030: da 01 f7 7b 22 66 69 6c 74 65 72 73 22 3a 5b 7b   Ú..{"filters":[{
...   (json_filters string, 503 bytes — the COMPARE_EQUAL tree of filter_eq_bool.json,
       including the "alias":"" and "type_info":null fields present in real DuckDB output)
0228: ...                              aa 63 6f 6c 75 6d            ªcolum
0230: 6e 5f 69 64 73 92 01 03 b9 74 61 62 6c 65 5f 66   n_ids...¹table_f
0240: 75 6e 63 74 69 6f 6e 5f 70 61 72 61 6d 65 74 65   unction_paramete
0250: 72 73 c4 00 bb 74 61 62 6c 65 5f 66 75 6e 63 74   rsÄ.»table_funct
0260: 69 6f 6e 5f 69 6e 70 75 74 5f 73 63 68 65 6d 61   ion_input_schema
0270: c4 00 a7 61 74 5f 75 6e 69 74 a0 a8 61 74 5f 76   Ä.§at_unit¨at_v
0280: 61 6c 75 65 a0                                    alue
```

### Byte-level annotation of the map framing

| Bytes | MessagePack token | Meaning |
|---|---|---|
| `82` | fixmap(2) | outer `AirportGetFlightEndpointsRequest` — 2 keys |
| `aa` + `descriptor` | fixstr(10) | key `"descriptor"` |
| `c4 09` + `12 07 markets` | bin8(9) | serialized FlightDescriptor (placeholder) |
| `aa` + `parameters` | fixstr(10) | key `"parameters"` |
| `86` | fixmap(6) | nested `AirportEndpointParameters` — 6 keys |
| `ac` + `json_filters` | fixstr(12) | key `"json_filters"` |
| `da 01 b4` + … | str16(436) | the predicate JSON string |
| `aa` + `column_ids` | fixstr(10) | key `"column_ids"` |
| `92 01 03` | fixarray(2) [1,3] | projected column indices |
| `b9` + `table_function_parameters` | fixstr(25) | key |
| `c4 00` | bin8(0) | empty (plain table scan) |
| `bb` + `table_function_input_schema` | fixstr(27) | key |
| `c4 00` | bin8(0) | empty |
| `a7` + `at_unit` | fixstr(7) | key |
| `a0` | fixstr(0) | `""` (no time travel) |
| `a8` + `at_value` | fixstr(8) | key |
| `a0` | fixstr(0) | `""` |

MessagePack token reference: <https://github.com/msgpack/msgpack/blob/master/spec.md>
(`0x80|n` fixmap, `0x90|n` fixarray, `0xa0|n` fixstr, `0xc4` bin8, `0xc2/0xc3` false/true,
`0xda` str16).

## Rust decode (Plan 2)

```rust
#[derive(serde::Deserialize)]
struct EndpointsParameters {
    json_filters: String,
    column_ids: Vec<u64>,
    table_function_parameters: serde_bytes::ByteBuf,
    table_function_input_schema: serde_bytes::ByteBuf,
    at_unit: String,
    at_value: String,
}
#[derive(serde::Deserialize)]
struct EndpointsRequest {
    descriptor: serde_bytes::ByteBuf,
    parameters: EndpointsParameters,
}
// let req: EndpointsRequest = rmp_serde::from_slice(&action.body)?;
```
`rmp_serde::from_slice` decodes both map- and array-encoded msgpack, so this matches the
client's `MSGPACK_DEFINE_MAP` output directly.

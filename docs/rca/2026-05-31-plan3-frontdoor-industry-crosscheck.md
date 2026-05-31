# Plan 3 Frontdoor Industry Crosscheck

Date: 2026-05-31

## 1. Confirmed Finding: Process Boundary

SPUR notebook cells execute in a Jupyter kernel process, not in the Rust process that owns `duckdb::Connection`.

Evidence already established in the prior investigation:

- The local kernel path launches a kernelspec command with a `{connection_file}` replacement and then connects over ZeroMQ (`crates/spur-notebook/jute-notebook/src-tauri/src/backend/local.rs:49-107`).
- Cell execution sends a Jupyter `ExecuteRequest` through `KernelConnection::call_shell` (`crates/spur-notebook/jute-notebook/src-tauri/src/backend/commands.rs:72-90`).
- The ZeroMQ transport uses Jupyter's five-socket protocol (`crates/spur-notebook/jute-notebook/src-tauri/src/backend/wire_protocol/driver_zeromq.rs:1-5`, `:65-111`).
- The datasource setup cell is generated Python that imports Python `duckdb` and emits `duckdb.sql(...)` calls (`crates/spur-notebook/src/mcp/mod.rs:717-737`), with ATTACH statements generated in `datasource_setup_statements` (`crates/spur-notebook/src/mcp/mod.rs:756-770`).

Implication: a Rust-registered `ApiTableVTab` inside SPUR's daemon process is invisible to the kernel's Python `duckdb` connection.

## 2. Kernel DuckDB Version Finding

Finding: the Python `duckdb` package version used by the kernel is not pinned by SPUR. In the default provisioned kernel, SPUR does not install `duckdb` at all.

Repo evidence:

- `start_kernel` always calls `ensure_python3_kernelspec` before starting the selected local kernel (`crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1334-1358`).
- The provisioner creates `~/.spur/jupyter/kernels/python3/kernel.json`, creates `~/.spur/jupyter/venv`, installs only `ipykernel`, and registers it as `Python 3 (SPUR)` (`crates/spur-notebook/jute-notebook/src-tauri/src/kernel_provision.rs:129-190`).
- The managed Python version fallback is `3.12`, then `3.11`, via `uv venv --no-project --seed --python <version> --python-preference managed` (`crates/spur-notebook/jute-notebook/src-tauri/src/kernel_provision.rs:272-318`).
- Kernel discovery prepends `~/.spur/jupyter`, then honors `JUPYTER_PATH`, `JUPYTER_DATA_DIR`, OS Jupyter dirs, and system dirs (`crates/spur-notebook/jute-notebook/src-tauri/src/backend/local/environment.rs:56-89`), and `start_local_kernel` selects by `spec_name` from those paths (`crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:982-1014`).
- The UI-managed venv command installs `ipykernel`, `black`, and `basedpyright`, not `duckdb` (`crates/spur-notebook/jute-notebook/src-tauri/src/commands/venv.rs:64-115`).
- The notebook daemon docs say the datasource demo requires `ipykernel`, `pyzmq`, and `duckdb` for the selected `PYTHON_PATH`, but give no `duckdb` version pin (`crates/spur-notebook/docs/DAEMON.md:104-107`).

Operational consequence: loadable-extension feasibility is gated by the user's selected kernel environment. The loader version is whatever `import duckdb; duckdb.__version__` reports inside that kernel. Today SPUR cannot stamp and ship one Rust `.duckdb_extension` with confidence unless it also owns/pins the kernel `duckdb` package.

## 3. Industry Survey Table

| Tool / Pattern | Approach | Mechanism | URL |
|---|---|---|---|
| DuckDB C table function API | Native in-process SQL table function | C API defines table functions callable in `FROM`, including named parameters via `duckdb_table_function_add_named_parameter` and bind-time access via `duckdb_bind_get_named_parameter`. | https://duckdb.org/docs/current/clients/c/table_functions |
| DuckDB stable C extension API | Loadable extension ABI | Stable API extensions are built on the stable C extension API and are intended to be binary-compatible across multiple DuckDB versions; unstable extensions remain tied 1:1 to a DuckDB version. | https://duckdb.org/docs/current/dev/release_cycle |
| DuckDB extension packaging/signing | Versioned/platformed binary extension | Extensions are signed by default; unsigned local extensions require `allow_unsigned_extensions`; `LOAD './some/local/ext.duckdb_extension'` works after enabling unsigned loads; incompatible DuckDB version/platform is rejected. | https://duckdb.org/docs/current/extensions/extension_distribution |
| DuckDB Python extension loading | Python client API hook | Python exposes `connect(..., config: dict)`, `install_extension`, and `load_extension`; unsigned loading must be enabled as a DuckDB database config on the connection before load. | https://duckdb.org/docs/current/clients/python/reference/ and https://duckdb.org/docs/current/extensions/extension_distribution |
| duckdb-rs loadable extension | Rust wrapper over DuckDB loadable-extension path | `duckdb` crate feature `loadable-extension` pulls in loadable macros, `libduckdb-sys/loadable-extension`, and `vtab`; duckdb-rs says `hello-ext` registers a table function through `duckdb_entrypoint_c_api`, but a bare shared library is not loadable without a `.duckdb_extension` metadata footer matching the target DuckDB version. | https://docs.rs/crate/duckdb/latest/features and https://github.com/duckdb/duckdb-rs |
| DuckDB replacement scans | Python/Arrow objects as tables by name | Python can query visible Pandas/Polars/NumPy/Arrow objects by variable name using replacement scans; registered objects behave like virtual views. This is table/view syntax, not parameterized table-function syntax. | https://duckdb.org/docs/current/clients/python/data_ingestion |
| DuckDB Python relation API | Arrow object relation bridge | `from_arrow` creates a DuckDB relation from a PyArrow table/record batch; `table_function` creates a relation from an already-existing DuckDB table function with positional parameters. | https://duckdb.org/docs/current/clients/python/relational_api |
| DuckDB Python UDF API | Scalar Python functions only | The public Python API documents `create_function` for callable-to-DuckDB functions, but not a Python API for defining parameterized table functions with named arguments. | https://duckdb.org/docs/current/clients/python/reference/ |
| DuckDB `httpfs` | Remote file/object storage, not arbitrary REST table source | `httpfs` is an autoloadable filesystem extension for HTTP(S) file reads and S3-compatible storage access. | https://duckdb.org/docs/current/core_extensions/httpfs/overview |
| DuckDB `http_client` community extension | SQL HTTP request helpers | Community extension exposes scalar functions such as `http_get`/`http_post`, including named `headers` and `params`; callers still parse JSON into rows themselves. | https://duckdb.org/community_extensions/extensions/http_client |
| DuckDB ADBC | Arrow-native database connectivity | ADBC is a C-style database connectivity API using Arrow transfer; DuckDB provides an ADBC driver, but this is client/database connectivity rather than a REST table function surface. | https://duckdb.org/docs/current/clients/adbc |
| MotherDuck | DuckDB cloud extension / ATTACH | The `motherduck` extension autoinstalls/autoloads and connects via `ATTACH 'md:'`; it is a DuckDB cloud warehouse path, not a generic REST adapter. | https://duckdb.org/docs/current/core_extensions/motherduck |
| Rill | Ingest to managed OLAP | Rill defaults to embedded DuckDB/ClickHouse as OLAP and ingests external sources; it can also bring-your-own OLAP with live connectors. | https://docs.rilldata.com/developers/build/connectors |
| Evidence.dev | Extract/cache or direct warehouse query | Evidence database connectors extract data and cache it in Evidence's query engine; application connectors sync SaaS/API data through Fivetran; object storage reads Parquet in place. | https://docs.evidence.studio/data-sources/index |
| Steampipe | API-to-SQL via FDW/virtual-table plugins | Steampipe exposes APIs/services as relational tables through its own Postgres-backed CLI, Postgres FDWs, and SQLite virtual tables; this is a separate SQL engine/plugin ecosystem. | https://steampipe.io/docs and https://steampipe.io/ |
| ROAPI | SQL/GraphQL/REST frontends over Arrow/DataFusion | ROAPI translates SQL/GraphQL/REST queries into DataFusion plans and serializes Arrow record batches; it is server-side query over configured datasets. | https://roapi.github.io/docs/ |
| dlt | Ingest/materialize REST APIs into DuckDB | dlt's REST API source declaratively configures endpoints/pagination/auth and `pipeline(..., destination="duckdb")` creates DuckDB tables for resources. | https://dlthub.com/docs/dlt-ecosystem/verified-sources/rest_api/basic |
| Airbyte | Connector-based replication into destinations | Airbyte replicates data from hundreds of sources into warehouses/lakes/databases; DuckDB is a destination pattern, not a live DuckDB table function. | https://docs.airbyte.com/ |
| Singer | Pipe-based ETL | Singer taps extract from APIs/databases/files to JSON streams; targets load into files/APIs/databases. This is materialization/replication, not a DuckDB extension. | https://www.singer.io/ |

## 4. Ranked Options A-D

| Rank | Option | Preserves `polymarket_markets(active := true)` UX? | Reuses Plan 1 / Plan 2? | Load/version/signing constraints | Build/packaging cost | Fit for SPUR |
|---:|---|---|---|---|---|---|
| 1 | A. Repackage Plan 2 VTab as loadable C-API `.duckdb_extension` and have setup cell load it | Yes. C table functions support named parameters in `FROM`, matching the target UX. | High reuse of Plan 1 adapter and Plan 2 table-function/VTab logic, but callback glue must be extension-safe. | High risk. The Python kernel's `duckdb` version is unpinned; extension must carry a valid `.duckdb_extension` footer for the loading DuckDB version/platform; unsigned local loads require `allow_unsigned_extensions` set before `LOAD`. | Medium-high. Need official Rust extension-template packaging, per-platform artifacts, setup-cell load path, and a version policy. | Best UX and code reuse, but only if SPUR pins or discovers/builds against the kernel DuckDB version. |
| 2 | C. Ingest/materialize REST to Arrow/Parquet/DuckDB tables in the kernel DB | No. User queries real tables/views, not parameterized table functions. | Medium-high reuse of Plan 1 REST-to-Arrow engine; little use of Plan 2 VTab. | Low. Avoids DuckDB extension loading/signing; only needs Python `duckdb` plus file/table creation. | Medium. Need refresh semantics, snapshot naming, invalidation, schema drift handling, and storage lifecycle. | Most reliable near-term fallback and matches dlt/Airbyte/Evidence/Rill industry pattern. |
| 3 | B. Python Arrow shim: sidecar serves ScanRequest -> Arrow IPC; generated Python helper registers Arrow relation/view | No for named table-function syntax. Could approximate `polymarket_markets = spur_api("polymarket_markets", active=True)` then query a relation/view. | High reuse of Plan 1; Plan 2 mostly bypassed. | Low-medium. Needs `pyarrow` in kernel and IPC/socket compatibility; avoids extension ABI/signing. | Medium. Need Python helper, socket protocol hardening, relation lifetime, pagination/backpressure. | Good diagnostic bridge, but the UX regression is significant. |
| 4 | D. ADBC / Arrow Flight / sidecar DuckDB file ATTACH | Mostly no. ADBC/Flight are transport/connectivity layers; a sidecar DuckDB file can be `ATTACH`ed but exposes materialized tables/views. | Varies: sidecar DuckDB can reuse Plan 1 and maybe Plan 2 internally, but the kernel only sees persisted output. | Medium. A sidecar file avoids extension loading but introduces writer/reader consistency. ADBC/Flight needs a client/driver story inside the kernel, not a DuckDB table-function registration story. | Medium-high. More moving parts than C without preserving the target syntax. | Useful later for remote/query-service architecture, not the best front door for notebook UX. |

## 5. Recommendation And Spike-Critical Unknown

Recommendation: pursue Option A, but only as a spike until the loader-version problem is proven. It is the only option that preserves the desired notebook UX (`SELECT * FROM polymarket_markets(active := true)`) and meaningfully reuses the Plan 2 table-function work. The industry direction supports this shape: DuckDB's C table-function API supports named parameters (https://duckdb.org/docs/current/clients/c/table_functions), DuckDB extensions are loadable in all clients including Python/R (https://duckdb.org/docs/lts/extensions/overview.html), and duckdb-rs now has a loadable-extension path (https://github.com/duckdb/duckdb-rs). The risk is not architectural; it is packaging/runtime compatibility.

Single spike-critical unknown:

> Can the actual SPUR-launched Python kernel load an unsigned local-path Rust-built `.duckdb_extension`, stamped for that kernel's exact `duckdb.__version__`, after the setup cell creates/uses a DuckDB connection configured with `allow_unsigned_extensions`?

If yes, Option A is the product path. If no, fall back to Option C for a reliable materialized-table experience while keeping Option A blocked on pinned kernel packaging.

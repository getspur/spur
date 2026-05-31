# DuckDB VTab REST Probe Verdict

Overall verdict: **FEASIBLE-WITH-CAVEATS**.

REST-backed DuckDB table functions are feasible in-process with `duckdb 1.10502.0` and the `bundled` + `vtab` features. The caveat is material: a VTab callback that creates a Tokio runtime and calls `Runtime::block_on` **aborts** when the DuckDB query is executed directly on a thread already inside an outer Tokio runtime. Running the whole synchronous DuckDB query on a blocking thread avoids that panic. A shared dedicated I/O runtime thread also worked in the direct nested-runtime probe because the VTab callback uses blocking channels instead of calling `block_on` on the Tokio worker thread.

## Q1-Q5

- Q1: **PASS** - `duckdb 1.10502.0` exposes `duckdb::vtab::VTab` and `Connection::register_table_function*`; the crate compiles and links with `bundled` + `vtab`.
- Q2: **PASS** - `SELECT * FROM polymarket_markets('true')` read back one returned row and asserted `id = "m1"`.
- Q3: **PASS** - the table function accepted `active VARCHAR`; the local server observed `GET /markets?active=true HTTP/1.1`.
- Q4: **PASS with negative finding** - sync context works; direct execution inside an outer Tokio runtime aborts with `Cannot start a runtime from within a runtime`; `spawn_blocking`, `std::thread`, and a shared I/O runtime thread avoided it.
- Q5: **PASS** - recommended pattern: run DuckDB `SELECT`s from `tokio::task::spawn_blocking` or another blocking thread, and prefer a shared dedicated runtime/client for REST I/O over creating a Tokio runtime per VTab callback.

## API Used

From `duckdb 1.10502.0`:

```rust
pub trait VTab: Sized {
    type InitData: Sized + Send + Sync;
    type BindData: Sized + Send + Sync;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn std::error::Error>>;
    fn init(init: &InitInfo) -> Result<Self::InitData, Box<dyn std::error::Error>>;
    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn std::error::Error>>;

    fn parameters() -> Option<Vec<LogicalTypeHandle>>;
}

impl Connection {
    pub fn register_table_function<T: VTab>(&self, name: &str) -> Result<()>;

    pub fn register_table_function_with_extra_info<T: VTab, E>(
        &self,
        name: &str,
        extra_info: &E,
    ) -> Result<()>
    where
        E: Clone + Send + Sync + 'static;
}
```

The probe used `BindInfo::add_result_column`, `BindInfo::get_parameter(0)`, `BindInfo::get_extra_info`, `TableFunctionInfo::get_bind_data`, `TableFunctionInfo::get_init_data`, `DataChunkHandle::flat_vector`, string `Inserter::insert`, primitive `FlatVector::as_mut_slice`, and `DataChunkHandle::set_len`.

## Actual Output

`cargo check`:

```text
Checking duckdb-vtab-probe v0.1.0 (/Volumes/Projects/spur/.spur/worktrees/0da3be96-b5d7-4c11-b124-6a1e94531f9b/docs/spikes/duckdb-vtab-probe)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.51s
```

`cargo run`:

```text
Q1 api compile/link: PASS - registered VTab via Connection::register_table_function_with_extra_info
Q2 rows returned + Q3 argument used: PASS - row=MarketRow { id: "m1", question: "Q?", active: true, volume: 12.5 }; observed_request=GET /markets?active=true HTTP/1.1
Q4a sync per-call Runtime::block_on: PASS - sync query returned MarketRow { id: "m1", question: "Q?", active: true, volume: 12.5 }
Q4b direct query inside outer tokio runtime: PASS - failed as expected with status signal: 6 (SIGABRT); stderr:

thread 'main' (29314452) panicked at /Users/kevintruong/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.52.3/src/runtime/scheduler/multi_thread/mod.rs:91:9:
Cannot start a runtime from within a runtime. This happens because a function (like `block_on`) attempted to block the current thread while the thread is being used to drive asynchronous tasks.
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

thread 'main' (29314452) panicked at /rustc/e408947bfd200af42db322daf0fadfe7e26d3bd1/library/core/src/panicking.rs:225:5:
panic in a function that cannot unwind
stack backtrace:
Q4b spawn_blocking workaround: PASS - spawn_blocking query returned MarketRow { id: "m1", question: "Q?", active: true, volume: 12.5 }
Q4b std::thread workaround: PASS - std::thread query returned MarketRow { id: "m1", question: "Q?", active: true, volume: 12.5 }
Q4b shared I/O runtime thread direct query: PASS - shared I/O thread direct query returned MarketRow { id: "m1", question: "Q?", active: true, volume: 12.5 }
Q5 recommendation: run DuckDB SELECTs from blocking threads (tokio::task::spawn_blocking or std::thread) and avoid per-call Runtime creation on Tokio worker threads; for production REST I/O prefer a shared dedicated runtime/client rather than one runtime per VTab callback.
```

`cargo test`:

```text
running 4 tests
test tests::sync_per_call_runtime_fetches_local_http ... ok
test tests::table_function_returns_rows_and_uses_argument ... ok
test tests::outer_tokio_spawn_blocking_avoids_nested_runtime ... ok
test tests::outer_tokio_shared_io_thread_works_directly ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
```

## Recommendation

For notebook integration, do **not** run a synchronous DuckDB query containing REST-backed VTabs directly on an async Tokio worker thread if the VTab calls `Runtime::block_on`. That path aborts.

Use this concrete pattern:

1. Execute DuckDB queries from `tokio::task::spawn_blocking` or an equivalent dedicated blocking query thread.
2. Do not create a fresh Tokio runtime per row or per VTab callback in production.
3. Put REST I/O behind a shared client/runtime owned by the notebook backend or a dedicated I/O thread, and make the VTab callback synchronously wait for that result only while it is already off the Tokio worker thread.

This keeps DuckDB's synchronous VTab API contained in the blocking part of the system and avoids nested runtime panics.

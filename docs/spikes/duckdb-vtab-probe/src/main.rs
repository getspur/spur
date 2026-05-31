use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId},
    vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab},
    Connection,
};
use serde_json::Value;
use std::{
    env,
    error::Error,
    io::{Read, Write},
    net::TcpListener,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
};

type AnyError = Box<dyn Error + Send + Sync>;

const RESPONSE_BODY: &str = r#"[{"id":"m1","question":"Q?","active":true,"volume":12.5}]"#;

#[derive(Clone)]
enum Bridge {
    PerCallRuntime,
    SharedIoThread(BlockingIoClient),
}

#[derive(Clone)]
struct ProbeConfig {
    base_url: String,
    bridge: Bridge,
}

struct MarketBindData {
    active: String,
    config: ProbeConfig,
}

struct MarketInitData {
    done: AtomicBool,
}

struct Market {
    id: String,
    question: String,
    active: bool,
    volume: f64,
}

struct PolymarketMarketsVTab;

impl VTab for PolymarketMarketsVTab {
    type InitData = MarketInitData;
    type BindData = MarketBindData;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        bind.add_result_column("id", LogicalTypeHandle::from(LogicalTypeId::Varchar));
        bind.add_result_column("question", LogicalTypeHandle::from(LogicalTypeId::Varchar));
        bind.add_result_column("active", LogicalTypeHandle::from(LogicalTypeId::Boolean));
        bind.add_result_column("volume", LogicalTypeHandle::from(LogicalTypeId::Double));

        let config = unsafe { &*bind.get_extra_info::<ProbeConfig>() };
        Ok(MarketBindData {
            active: bind.get_parameter(0).to_string(),
            config: config.clone(),
        })
    }

    fn init(_: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        Ok(MarketInitData {
            done: AtomicBool::new(false),
        })
    }

    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn Error>> {
        let init = func.get_init_data();
        if init.done.swap(true, Ordering::Relaxed) {
            output.set_len(0);
            return Ok(());
        }

        let bind = func.get_bind_data();
        let url = format!("{}/markets?active={}", bind.config.base_url, bind.active);
        let rows = fetch_markets(&bind.config.bridge, url)
            .map_err(|err| -> Box<dyn Error> { err.to_string().into() })?;
        let len = rows.len();

        let id_vec = output.flat_vector(0);
        let question_vec = output.flat_vector(1);
        let mut active_vec = output.flat_vector(2);
        let mut volume_vec = output.flat_vector(3);
        let active_slice = active_vec.as_mut_slice::<bool>();
        let volume_slice = volume_vec.as_mut_slice::<f64>();

        for (idx, row) in rows.iter().enumerate() {
            id_vec.insert(idx, row.id.as_str());
            question_vec.insert(idx, row.question.as_str());
            active_slice[idx] = row.active;
            volume_slice[idx] = row.volume;
        }

        output.set_len(len);
        Ok(())
    }

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![LogicalTypeHandle::from(LogicalTypeId::Varchar)])
    }
}

#[derive(Clone)]
struct BlockingIoClient {
    tx: mpsc::Sender<IoRequest>,
}

struct IoRequest {
    url: String,
    reply: mpsc::Sender<Result<String, String>>,
}

impl BlockingIoClient {
    fn start() -> Self {
        let (tx, rx) = mpsc::channel::<IoRequest>();
        thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("create dedicated I/O runtime");
            for request in rx {
                let result = rt
                    .block_on(fetch_body(request.url))
                    .map_err(|err| err.to_string());
                let _ = request.reply.send(result);
            }
        });
        Self { tx }
    }

    fn fetch_body(&self, url: String) -> Result<String, AnyError> {
        let (reply, rx) = mpsc::channel();
        self.tx.send(IoRequest { url, reply })?;
        match rx.recv()? {
            Ok(body) => Ok(body),
            Err(err) => Err(err.into()),
        }
    }
}

fn fetch_markets(bridge: &Bridge, url: String) -> Result<Vec<Market>, AnyError> {
    let body = match bridge {
        Bridge::PerCallRuntime => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(fetch_body(url))?
        }
        Bridge::SharedIoThread(client) => client.fetch_body(url)?,
    };
    parse_markets(&body)
}

async fn fetch_body(url: String) -> Result<String, AnyError> {
    Ok(reqwest::get(url).await?.text().await?)
}

fn parse_markets(body: &str) -> Result<Vec<Market>, AnyError> {
    let value: Value = serde_json::from_str(body)?;
    let array = value.as_array().ok_or("expected JSON array")?;
    let mut markets = Vec::with_capacity(array.len());
    for item in array {
        markets.push(Market {
            id: item["id"].as_str().ok_or("id missing")?.to_string(),
            question: item["question"]
                .as_str()
                .ok_or("question missing")?
                .to_string(),
            active: item["active"].as_bool().ok_or("active missing")?,
            volume: item["volume"].as_f64().ok_or("volume missing")?,
        });
    }
    Ok(markets)
}

#[derive(Debug, PartialEq)]
struct MarketRow {
    id: String,
    question: String,
    active: bool,
    volume: f64,
}

fn run_query(base_url: String, bridge: Bridge, active: &str) -> Result<MarketRow, AnyError> {
    let conn = Connection::open_in_memory()?;
    let config = ProbeConfig { base_url, bridge };
    conn.register_table_function_with_extra_info::<PolymarketMarketsVTab, _>(
        "polymarket_markets",
        &config,
    )?;

    let sql = format!("select id, question, active, volume from polymarket_markets('{active}')");
    let row = conn.query_row(&sql, [], |row| {
        Ok(MarketRow {
            id: row.get(0)?,
            question: row.get(1)?,
            active: row.get(2)?,
            volume: row.get(3)?,
        })
    })?;
    Ok(row)
}

struct LocalServer {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

fn start_local_server() -> Result<LocalServer, AnyError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let thread_requests = Arc::clone(&requests);

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                break;
            };
            let mut buf = [0_u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            if let Some(line) = request.lines().next() {
                thread_requests.lock().unwrap().push(line.to_string());
            }

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                RESPONSE_BODY.len(),
                RESPONSE_BODY
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });

    Ok(LocalServer {
        base_url: format!("http://{addr}"),
        requests,
    })
}

fn q1_compile_link_api() -> Result<String, AnyError> {
    let conn = Connection::open_in_memory()?;
    let server = start_local_server()?;
    let config = ProbeConfig {
        base_url: server.base_url,
        bridge: Bridge::PerCallRuntime,
    };
    conn.register_table_function_with_extra_info::<PolymarketMarketsVTab, _>(
        "polymarket_markets",
        &config,
    )?;
    Ok("registered VTab via Connection::register_table_function_with_extra_info".to_string())
}

fn q2_q3_rows_and_argument() -> Result<String, AnyError> {
    let server = start_local_server()?;
    let row = run_query(server.base_url.clone(), Bridge::PerCallRuntime, "true")?;
    assert_eq!(
        row,
        MarketRow {
            id: "m1".to_string(),
            question: "Q?".to_string(),
            active: true,
            volume: 12.5,
        }
    );
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("/markets?active=true"));
    Ok(format!("row={row:?}; observed_request={}", requests[0]))
}

fn q4a_sync_per_call_runtime() -> Result<String, AnyError> {
    let server = start_local_server()?;
    let row = run_query(server.base_url, Bridge::PerCallRuntime, "true")?;
    Ok(format!("sync query returned {row:?}"))
}

fn q4b_direct_nested_runtime_child() -> Result<(), AnyError> {
    let outer = tokio::runtime::Runtime::new()?;
    outer.block_on(async {
        let server = start_local_server().expect("local server");
        let row = run_query(server.base_url, Bridge::PerCallRuntime, "true").expect("query row");
        println!("direct nested runtime returned {row:?}");
    });
    Ok(())
}

fn q4b_direct_nested_runtime_subprocess() -> Result<String, AnyError> {
    let exe = env::current_exe()?;
    let output = Command::new(exe)
        .arg("--case=direct-nested-runtime")
        .output()?;
    if output.status.success() {
        return Ok(format!(
            "unexpectedly succeeded: {}",
            String::from_utf8_lossy(&output.stdout).trim()
        ));
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let first_lines = stderr.lines().take(8).collect::<Vec<_>>().join("\n");
    Ok(format!(
        "failed as expected with status {}; stderr:\n{}",
        output.status, first_lines
    ))
}

fn q4b_spawn_blocking_avoids_nested_runtime() -> Result<String, AnyError> {
    let server = start_local_server()?;
    let base_url = server.base_url;
    let outer = tokio::runtime::Runtime::new()?;
    let row = outer.block_on(async move {
        tokio::task::spawn_blocking(move || run_query(base_url, Bridge::PerCallRuntime, "true"))
            .await
            .expect("spawn_blocking join")
    })?;
    Ok(format!("spawn_blocking query returned {row:?}"))
}

fn q4b_std_thread_avoids_nested_runtime() -> Result<String, AnyError> {
    let server = start_local_server()?;
    let base_url = server.base_url;
    let outer = tokio::runtime::Runtime::new()?;
    let row = outer.block_on(async move {
        thread::spawn(move || run_query(base_url, Bridge::PerCallRuntime, "true"))
            .join()
            .expect("thread join")
    })?;
    Ok(format!("std::thread query returned {row:?}"))
}

fn q4b_shared_io_thread_direct_query() -> Result<String, AnyError> {
    let server = start_local_server()?;
    let base_url = server.base_url;
    let io = BlockingIoClient::start();
    let outer = tokio::runtime::Runtime::new()?;
    let row =
        outer.block_on(async move { run_query(base_url, Bridge::SharedIoThread(io), "true") })?;
    Ok(format!("shared I/O thread direct query returned {row:?}"))
}

fn print_check(label: &str, result: Result<String, AnyError>) {
    match result {
        Ok(detail) => println!("{label}: PASS - {detail}"),
        Err(err) => println!("{label}: FAIL - {err}"),
    }
}

fn run_all() {
    print_check("Q1 api compile/link", q1_compile_link_api());
    print_check(
        "Q2 rows returned + Q3 argument used",
        q2_q3_rows_and_argument(),
    );
    print_check(
        "Q4a sync per-call Runtime::block_on",
        q4a_sync_per_call_runtime(),
    );
    print_check(
        "Q4b direct query inside outer tokio runtime",
        q4b_direct_nested_runtime_subprocess(),
    );
    print_check(
        "Q4b spawn_blocking workaround",
        q4b_spawn_blocking_avoids_nested_runtime(),
    );
    print_check(
        "Q4b std::thread workaround",
        q4b_std_thread_avoids_nested_runtime(),
    );
    print_check(
        "Q4b shared I/O runtime thread direct query",
        q4b_shared_io_thread_direct_query(),
    );
    println!(
        "Q5 recommendation: run DuckDB SELECTs from blocking threads (tokio::task::spawn_blocking or std::thread) and avoid per-call Runtime creation on Tokio worker threads; for production REST I/O prefer a shared dedicated runtime/client rather than one runtime per VTab callback."
    );
}

fn main() -> Result<(), AnyError> {
    if env::args().any(|arg| arg == "--case=direct-nested-runtime") {
        return q4b_direct_nested_runtime_child();
    }
    run_all();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_function_returns_rows_and_uses_argument() -> Result<(), AnyError> {
        let detail = q2_q3_rows_and_argument()?;
        assert!(detail.contains("active=true"));
        Ok(())
    }

    #[test]
    fn sync_per_call_runtime_fetches_local_http() -> Result<(), AnyError> {
        let detail = q4a_sync_per_call_runtime()?;
        assert!(detail.contains("m1"));
        Ok(())
    }

    #[test]
    fn outer_tokio_spawn_blocking_avoids_nested_runtime() -> Result<(), AnyError> {
        let detail = q4b_spawn_blocking_avoids_nested_runtime()?;
        assert!(detail.contains("m1"));
        Ok(())
    }

    #[test]
    fn outer_tokio_shared_io_thread_works_directly() -> Result<(), AnyError> {
        let detail = q4b_shared_io_thread_direct_query()?;
        assert!(detail.contains("m1"));
        Ok(())
    }
}

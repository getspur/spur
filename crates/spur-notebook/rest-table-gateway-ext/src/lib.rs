use std::{
    env,
    error::Error,
    fs,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};

use arrow_array::{Array, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, SchemaRef};
use directories::BaseDirs;
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId},
    duckdb_entrypoint_c_api,
    vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab, Value as DuckValue},
    Connection, Result,
};
use spur_rest_table_gateway::{
    adapter::{
        manifest::Manifest, manifest_adapter::ManifestAdapter, ActionRequest, Adapter, ArgLocation,
        ArgSpec, ResolvedAuth, ScalarValue, ScanRequest, TableKind,
    },
    adapters::polymarket::PolymarketAdapter,
    vtab::{
        bridge::IoBridge,
        table_fn::{ApiTableExtra, ApiTableVTab},
    },
};

const DEFAULT_GAMMA_BASE: &str = "https://gamma-api.polymarket.com";
const DEFAULT_CLOB_BASE: &str = "https://clob.polymarket.com";
const CHUNK_SIZE: usize = 2048;
static ACTION_NAMED_PARAMETERS: OnceLock<Mutex<Vec<(String, DataType)>>> = OnceLock::new();

#[duckdb_entrypoint_c_api(ext_name = "spur_rest", min_duckdb_version = "v1.2.0")]
pub fn extension_entrypoint(con: Connection) -> Result<(), Box<dyn Error>> {
    let gamma_base =
        env::var("SPUR_POLYMARKET_GAMMA_BASE").unwrap_or_else(|_| DEFAULT_GAMMA_BASE.to_string());
    let clob_base =
        env::var("SPUR_POLYMARKET_CLOB_BASE").unwrap_or_else(|_| DEFAULT_CLOB_BASE.to_string());

    let adapter: Arc<dyn Adapter> = Arc::new(PolymarketAdapter::new(&gamma_base, &clob_base)?);
    let bridge = Arc::new(IoBridge::new());
    register_adapter(&con, adapter, Arc::clone(&bridge))?;

    if let Ok(manifest_path) = env::var("SPUR_REST_MANIFEST") {
        let manifest_toml = fs::read_to_string(manifest_path)?;
        let manifest = manifest_with_write_override(Manifest::from_toml(&manifest_toml)?);
        let manifest_adapter: Arc<dyn Adapter> = Arc::new(ManifestAdapter::new(manifest));
        register_adapter(&con, manifest_adapter, Arc::clone(&bridge))?;
    }

    register_saved_connections(&con, &bridge);

    Ok(())
}

fn register_saved_connections(con: &Connection, bridge: &Arc<IoBridge>) {
    for manifest_toml in saved_manifest_tomls() {
        match Manifest::from_toml(&manifest_toml) {
            Ok(manifest) => {
                let manifest = manifest_with_write_override(manifest);
                let manifest_adapter: Arc<dyn Adapter> = Arc::new(ManifestAdapter::new(manifest));
                if let Err(error) = register_adapter(con, manifest_adapter, Arc::clone(bridge)) {
                    eprintln!("spur_rest: saved manifest skipped: {error}");
                }
            }
            Err(error) => eprintln!("spur_rest: malformed saved manifest skipped: {error}"),
        }
    }
}

fn manifest_with_write_override(mut manifest: Manifest) -> Manifest {
    if env::var_os("SPUR_REST_ALLOW_WRITES").is_some() {
        manifest.source.allow_writes = true;
    }
    manifest
}

fn saved_manifest_tomls() -> Vec<String> {
    if let Some(dir) = env::var_os("SPUR_REST_MANIFEST_DIR") {
        return saved_manifest_tomls_from_dir(Path::new(&dir));
    }

    let Some(base_dirs) = BaseDirs::new() else {
        eprintln!("spur_rest: saved connections skipped: could not resolve home directory");
        return Vec::new();
    };
    let path = base_dirs.home_dir().join(".spur").join("connections.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            eprintln!(
                "spur_rest: saved connections skipped: failed to read {}: {error}",
                path.display()
            );
            return Vec::new();
        }
    };
    let value = match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("spur_rest: saved connections skipped: malformed JSON: {error}");
            return Vec::new();
        }
    };

    value
        .get("templates")
        .and_then(|templates| templates.as_array())
        .map(|templates| {
            templates
                .iter()
                .filter_map(|template| {
                    template
                        .get("manifest_toml")
                        .or_else(|| template.get("manifestToml"))
                        .and_then(|manifest_toml| manifest_toml.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn saved_manifest_tomls_from_dir(dir: &Path) -> Vec<String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!(
                "spur_rest: saved manifests skipped: failed to read {}: {error}",
                dir.display()
            );
            return Vec::new();
        }
    };

    let mut manifest_paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) == Some("toml") {
                    manifest_paths.push(path);
                }
            }
            Err(error) => eprintln!(
                "spur_rest: saved manifest entry skipped in {}: {error}",
                dir.display()
            ),
        }
    }
    manifest_paths.sort();

    manifest_paths
        .into_iter()
        .filter_map(|path| match fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(error) => {
                eprintln!(
                    "spur_rest: saved manifest skipped: failed to read {}: {error}",
                    path.display()
                );
                None
            }
        })
        .collect()
}

fn register_adapter(
    con: &Connection,
    adapter: Arc<dyn Adapter>,
    bridge: Arc<IoBridge>,
) -> Result<usize, Box<dyn Error>> {
    let mut registered = 0;
    for table in adapter.catalog() {
        let fn_name = format!("{}_{}", adapter.name(), table.name);
        match table.kind {
            TableKind::Table => {
                let extra = ApiTableExtra {
                    bridge: Arc::clone(&bridge),
                    adapter: Arc::clone(&adapter),
                    table: table.name,
                    schema: table.schema,
                };
                con.register_table_function_with_extra_info::<ApiTableVTab, _>(&fn_name, &extra)?;
                registered += 1;
            }
            TableKind::TableFunction { arg_names } => {
                let extra = ApiFunctionExtra {
                    bridge: Arc::clone(&bridge),
                    adapter: Arc::clone(&adapter),
                    table: table.name,
                    schema: table.schema,
                    arg_names,
                };
                con.register_table_function_with_extra_info::<ApiFunctionVTab, _>(
                    &fn_name, &extra,
                )?;
                registered += 1;
            }
            TableKind::Action {
                method,
                path,
                arg_specs,
                dry_run_arg,
                idempotency_header,
            } => {
                let extra = ApiActionExtra {
                    bridge: Arc::clone(&bridge),
                    adapter: Arc::clone(&adapter),
                    action: table.name,
                    method,
                    action_path: path,
                    schema: table.schema,
                    arg_specs,
                    dry_run_arg,
                    idempotency_header,
                };
                register_action_table_function(con, &fn_name, &extra)?;
                registered += 1;
            }
        }
    }
    Ok(registered)
}

fn register_action_table_function(
    con: &Connection,
    fn_name: &str,
    extra: &ApiActionExtra,
) -> Result<(), Box<dyn Error>> {
    let named_parameters = action_named_parameter_types(extra)?;
    {
        let mut current = action_named_parameters()
            .lock()
            .map_err(|err| format!("action named parameter lock poisoned: {err}"))?;
        *current = named_parameters;
    }
    let result = con.register_table_function_with_extra_info::<ApiActionVTab, _>(fn_name, extra);
    if let Ok(mut current) = action_named_parameters().lock() {
        current.clear();
    }
    result?;
    Ok(())
}

fn action_named_parameters() -> &'static Mutex<Vec<(String, DataType)>> {
    ACTION_NAMED_PARAMETERS.get_or_init(|| Mutex::new(Vec::new()))
}

fn action_named_parameter_types(
    extra: &ApiActionExtra,
) -> Result<Vec<(String, DataType)>, Box<dyn Error>> {
    let mut params = Vec::new();
    for spec in &extra.arg_specs {
        arrow_to_duckdb_type(&spec.ty)?;
        push_action_named_parameter(&mut params, &spec.name, spec.ty.clone());
    }
    if let Some(arg) = &extra.dry_run_arg {
        push_action_named_parameter(&mut params, arg, DataType::Boolean);
    }
    if extra.idempotency_header.is_some() {
        push_action_named_parameter(&mut params, "idempotency_key", DataType::Utf8);
    }
    Ok(params)
}

fn push_action_named_parameter(params: &mut Vec<(String, DataType)>, name: &str, ty: DataType) {
    if !params.iter().any(|(existing, _)| existing == name) {
        params.push((name.to_string(), ty));
    }
}

#[derive(Clone)]
struct ApiFunctionExtra {
    bridge: Arc<IoBridge>,
    adapter: Arc<dyn Adapter>,
    table: String,
    schema: SchemaRef,
    arg_names: Vec<String>,
}

struct ApiFunctionBindData {
    bridge: Arc<IoBridge>,
    adapter: Arc<dyn Adapter>,
    table: String,
    schema: SchemaRef,
    tvf_args: Vec<ScalarValue>,
}

struct ApiFunctionInitData {
    rows: Vec<RecordBatch>,
    cursor: Mutex<ApiCursor>,
}

#[derive(Default)]
struct ApiCursor {
    batch_idx: usize,
    row_idx: usize,
}

struct ApiFunctionVTab;

impl VTab for ApiFunctionVTab {
    type InitData = ApiFunctionInitData;
    type BindData = ApiFunctionBindData;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        let extra = unsafe { &*bind.get_extra_info::<ApiFunctionExtra>() };
        for field in extra.schema.fields() {
            let logical_type = LogicalTypeHandle::from(arrow_to_duckdb_type(field.data_type())?);
            bind.add_result_column(field.name(), logical_type);
        }

        Ok(ApiFunctionBindData {
            bridge: Arc::clone(&extra.bridge),
            adapter: Arc::clone(&extra.adapter),
            table: extra.table.clone(),
            schema: Arc::clone(&extra.schema),
            tvf_args: bind_named_args(bind, &extra.arg_names)?,
        })
    }

    fn init(init: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        let bind_data = unsafe { &*init.get_bind_data::<ApiFunctionBindData>() };
        let rows = bind_data.bridge.call(
            Arc::clone(&bind_data.adapter),
            ScanRequest {
                table: bind_data.table.clone(),
                predicates: vec![],
                projection: None,
                tvf_args: bind_data.tvf_args.clone(),
                auth: ResolvedAuth::None,
            },
        )?;

        Ok(ApiFunctionInitData {
            rows,
            cursor: Mutex::new(ApiCursor::default()),
        })
    }

    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn Error>> {
        let init_data = func.get_init_data();
        let bind_data = func.get_bind_data();
        let mut cursor = init_data
            .cursor
            .lock()
            .map_err(|err| format!("table cursor lock poisoned: {err}"))?;

        let mut emitted = 0;
        while emitted < CHUNK_SIZE && cursor.batch_idx < init_data.rows.len() {
            let batch = &init_data.rows[cursor.batch_idx];
            if cursor.row_idx >= batch.num_rows() {
                cursor.batch_idx += 1;
                cursor.row_idx = 0;
                continue;
            }

            let available = batch.num_rows() - cursor.row_idx;
            let take = available.min(CHUNK_SIZE - emitted);
            write_batch_rows(
                batch,
                &bind_data.schema,
                cursor.row_idx,
                take,
                emitted,
                output,
            )?;
            emitted += take;
            cursor.row_idx += take;
        }

        output.set_len(emitted);
        Ok(())
    }

    fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
        Some(vec![
            (
                "token_id".to_string(),
                LogicalTypeHandle::from(LogicalTypeId::Varchar),
            ),
            (
                "depth".to_string(),
                LogicalTypeHandle::from(LogicalTypeId::Bigint),
            ),
        ])
    }
}

#[derive(Clone)]
struct ApiActionExtra {
    bridge: Arc<IoBridge>,
    adapter: Arc<dyn Adapter>,
    action: String,
    method: String,
    action_path: String,
    schema: SchemaRef,
    arg_specs: Vec<ArgSpec>,
    dry_run_arg: Option<String>,
    idempotency_header: Option<String>,
}

struct ApiActionBindData {
    bridge: Arc<IoBridge>,
    adapter: Arc<dyn Adapter>,
    schema: SchemaRef,
    request: ActionRequest,
}

struct ApiActionInitData {
    rows: Vec<RecordBatch>,
    cursor: Mutex<ApiCursor>,
}

struct ApiActionVTab;

impl VTab for ApiActionVTab {
    type InitData = ApiActionInitData;
    type BindData = ApiActionBindData;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        let extra = unsafe { &*bind.get_extra_info::<ApiActionExtra>() };
        for field in extra.schema.fields() {
            let logical_type = LogicalTypeHandle::from(arrow_to_duckdb_type(field.data_type())?);
            bind.add_result_column(field.name(), logical_type);
        }

        Ok(ApiActionBindData {
            bridge: Arc::clone(&extra.bridge),
            adapter: Arc::clone(&extra.adapter),
            schema: Arc::clone(&extra.schema),
            request: compose_action_request(bind, extra)?,
        })
    }

    fn init(init: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        let bind_data = unsafe { &*init.get_bind_data::<ApiActionBindData>() };
        let rows = bind_data
            .bridge
            .call_act(Arc::clone(&bind_data.adapter), bind_data.request.clone())?;

        Ok(ApiActionInitData {
            rows,
            cursor: Mutex::new(ApiCursor::default()),
        })
    }

    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn Error>> {
        let init_data = func.get_init_data();
        let bind_data = func.get_bind_data();
        let mut cursor = init_data
            .cursor
            .lock()
            .map_err(|err| format!("action cursor lock poisoned: {err}"))?;

        let mut emitted = 0;
        while emitted < CHUNK_SIZE && cursor.batch_idx < init_data.rows.len() {
            let batch = &init_data.rows[cursor.batch_idx];
            if cursor.row_idx >= batch.num_rows() {
                cursor.batch_idx += 1;
                cursor.row_idx = 0;
                continue;
            }

            let available = batch.num_rows() - cursor.row_idx;
            let take = available.min(CHUNK_SIZE - emitted);
            write_batch_rows(
                batch,
                &bind_data.schema,
                cursor.row_idx,
                take,
                emitted,
                output,
            )?;
            emitted += take;
            cursor.row_idx += take;
        }

        output.set_len(emitted);
        Ok(())
    }

    fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
        let current = action_named_parameters().lock().ok()?;
        Some(
            current
                .iter()
                .map(|(name, data_type)| {
                    (
                        name.clone(),
                        LogicalTypeHandle::from(action_arg_duckdb_type(data_type)),
                    )
                })
                .collect(),
        )
    }
}

fn compose_action_request(
    bind: &BindInfo,
    extra: &ApiActionExtra,
) -> Result<ActionRequest, Box<dyn Error>> {
    let mut path = extra.action_path.clone();
    let mut body = serde_json::Map::new();
    let mut query = Vec::new();
    let mut dry_run = false;
    let mut idempotency_key = None;

    for spec in &extra.arg_specs {
        let Some(value) = bind.get_named_parameter(&spec.name) else {
            if spec.required {
                return Err(format!("action {} requires {}", extra.action, spec.name).into());
            }
            continue;
        };

        match spec.location {
            ArgLocation::Path => {
                path = path.replace(
                    &format!("{{{}}}", spec.name),
                    &duckdb_value_to_string(&value),
                );
            }
            ArgLocation::Body => {
                body.insert(
                    spec.json_key.clone(),
                    duckdb_value_to_json(&value, &spec.ty)?,
                );
            }
            ArgLocation::Query => {
                query.push((spec.query_param.clone(), duckdb_value_to_string(&value)));
            }
        }
    }

    if let Some(arg) = &extra.dry_run_arg {
        if let Some(value) = bind.get_named_parameter(arg) {
            dry_run = duckdb_value_to_bool(&value)?;
        }
    }
    if extra.idempotency_header.is_some() {
        if let Some(value) = bind.get_named_parameter("idempotency_key") {
            idempotency_key = Some(duckdb_value_to_string(&value));
        }
    }

    Ok(ActionRequest {
        name: extra.action.clone(),
        method: extra.method.clone(),
        path,
        query,
        body: if body.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(body))
        },
        auth: ResolvedAuth::None,
        idempotency_key,
        dry_run,
    })
}

fn bind_named_args(
    bind: &BindInfo,
    arg_names: &[String],
) -> Result<Vec<ScalarValue>, Box<dyn Error>> {
    let mut args = Vec::new();
    for arg_name in arg_names {
        match arg_name.as_str() {
            "token_id" => {
                let token_id = bind
                    .get_named_parameter("token_id")
                    .ok_or("polymarket_orderbook requires token_id")?
                    .to_string();
                args.push(ScalarValue::Utf8(token_id));
            }
            "depth" => {
                if let Some(depth) = bind.get_named_parameter("depth") {
                    args.push(ScalarValue::Int64(depth.to_int64()));
                }
            }
            other => return Err(format!("unsupported table function argument: {other}").into()),
        }
    }
    Ok(args)
}

fn duckdb_value_to_string(value: &DuckValue) -> String {
    value.to_string()
}

fn duckdb_value_to_json(
    value: &DuckValue,
    data_type: &DataType,
) -> Result<serde_json::Value, Box<dyn Error>> {
    Ok(match data_type {
        DataType::Utf8 => serde_json::Value::String(duckdb_value_to_string(value)),
        DataType::Int64 => serde_json::Value::Number(serde_json::Number::from(value.to_int64())),
        DataType::Float64 => {
            let parsed = duckdb_value_to_string(value)
                .parse::<f64>()
                .map_err(|err| format!("invalid Float64 action argument: {err}"))?;
            serde_json::Number::from_f64(parsed)
                .map(serde_json::Value::Number)
                .ok_or_else(|| format!("non-finite Float64 action argument: {parsed}"))?
        }
        DataType::Boolean => serde_json::Value::Bool(duckdb_value_to_bool(value)?),
        other => {
            return Err(format!("unsupported action argument type: {other:?}").into());
        }
    })
}

fn duckdb_value_to_bool(value: &DuckValue) -> Result<bool, Box<dyn Error>> {
    duckdb_value_to_string(value)
        .parse::<bool>()
        .map_err(|err| format!("invalid Boolean action argument: {err}").into())
}

fn action_arg_duckdb_type(data_type: &DataType) -> LogicalTypeId {
    match data_type {
        DataType::Utf8 => LogicalTypeId::Varchar,
        DataType::Int64 => LogicalTypeId::Bigint,
        DataType::Float64 => LogicalTypeId::Double,
        DataType::Boolean => LogicalTypeId::Boolean,
        other => unreachable!("unsupported action argument type was prevalidated: {other:?}"),
    }
}

fn arrow_to_duckdb_type(data_type: &DataType) -> Result<LogicalTypeId, Box<dyn Error>> {
    Ok(match data_type {
        DataType::Utf8 => LogicalTypeId::Varchar,
        DataType::Int64 => LogicalTypeId::Bigint,
        DataType::Float64 => LogicalTypeId::Double,
        DataType::Boolean => LogicalTypeId::Boolean,
        other => {
            return Err(
                format!("unsupported Arrow type for DuckDB table function: {other:?}").into(),
            );
        }
    })
}

fn write_batch_rows(
    batch: &RecordBatch,
    schema: &SchemaRef,
    source_start: usize,
    len: usize,
    output_start: usize,
    output: &mut DataChunkHandle,
) -> Result<(), Box<dyn Error>> {
    for (column_idx, field) in schema.fields().iter().enumerate() {
        let column = batch.column(column_idx);
        match field.data_type() {
            DataType::Utf8 => {
                let array = column
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .ok_or_else(|| format!("column {} expected Utf8 array", field.name()))?;
                let mut vector = output.flat_vector(column_idx);
                for row_offset in 0..len {
                    let source_row = source_start + row_offset;
                    let output_row = output_start + row_offset;
                    if array.is_null(source_row) {
                        vector.set_null(output_row);
                    } else {
                        vector.insert(output_row, array.value(source_row));
                    }
                }
            }
            DataType::Int64 => {
                let array = column
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .ok_or_else(|| format!("column {} expected Int64 array", field.name()))?;
                let mut vector = output.flat_vector(column_idx);
                for row_offset in 0..len {
                    let source_row = source_start + row_offset;
                    let output_row = output_start + row_offset;
                    if array.is_null(source_row) {
                        vector.set_null(output_row);
                    } else {
                        vector.as_mut_slice::<i64>()[output_row] = array.value(source_row);
                    }
                }
            }
            DataType::Float64 => {
                let array = column
                    .as_any()
                    .downcast_ref::<Float64Array>()
                    .ok_or_else(|| format!("column {} expected Float64 array", field.name()))?;
                let mut vector = output.flat_vector(column_idx);
                for row_offset in 0..len {
                    let source_row = source_start + row_offset;
                    let output_row = output_start + row_offset;
                    if array.is_null(source_row) {
                        vector.set_null(output_row);
                    } else {
                        vector.as_mut_slice::<f64>()[output_row] = array.value(source_row);
                    }
                }
            }
            DataType::Boolean => {
                let array = column
                    .as_any()
                    .downcast_ref::<BooleanArray>()
                    .ok_or_else(|| format!("column {} expected Boolean array", field.name()))?;
                let mut vector = output.flat_vector(column_idx);
                for row_offset in 0..len {
                    let source_row = source_start + row_offset;
                    let output_row = output_start + row_offset;
                    if array.is_null(source_row) {
                        vector.set_null(output_row);
                    } else {
                        vector.as_mut_slice::<bool>()[output_row] = array.value(source_row);
                    }
                }
            }
            other => {
                return Err(
                    format!("unsupported Arrow type for DuckDB table function: {other:?}").into(),
                );
            }
        }
    }
    Ok(())
}

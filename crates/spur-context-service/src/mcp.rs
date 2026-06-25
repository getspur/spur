//! MCP tool definitions and handlers for the external code context service.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_sdk_sfn::types::ExecutionStatus as AwsExecutionStatus;
use duckdb::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use crate::abuse::{self, RateLimiter, SourceKind, ValidateOptions};
use crate::catalog::{CatalogResolver, ResolvedRevision};
use crate::jobs::{CreateJobOutcome, CreateJobRequest, JobRecord, JobStatus, JobStore, JobsError};
use crate::knowledge::{self, KnowledgeContextOptions, KnowledgeScope};
use crate::query::{self, SearchMode, SearchOptions};

const DEFAULT_SOURCE: &str = "registry:crates-io";
const DEFAULT_INDEX_SOURCE: &str = "git:custom";
const DEFAULT_REF: &str = "latest";
const KNOWLEDGE_QUERY_VECTOR_DIMENSIONS: usize = 768;
const RATE_LIMIT_RETRY_AFTER_SECONDS: u64 = 60;
const DESCRIBE_EXECUTION_TIMEOUT: Duration = Duration::from_secs(2);
const STALE_JOB_REPAIR_AFTER: Duration = Duration::from_secs(60);

static INDEX_RATE_LIMITER: OnceLock<RateLimiter> = OnceLock::new();

/// Metadata for a single context-service MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexExecutionRequest {
    pub name: String,
    pub input: Value,
}

pub trait IndexExecutionStarter {
    fn start_execution<'a>(
        &'a self,
        request: IndexExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<String, McpHandlerError>> + Send + 'a>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionOutcomeStatus {
    Succeeded,
    Failed,
    Running,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionOutcome {
    pub status: ExecutionOutcomeStatus,
    pub output: Option<Value>,
    pub error: Option<String>,
}

pub trait ExecutionStatusChecker {
    fn describe_execution(&self, arn: &str) -> Result<Option<ExecutionOutcome>, McpHandlerError>;
}

#[derive(Debug, Clone)]
pub struct SfnExecutionStatusChecker {
    client: aws_sdk_sfn::Client,
    timeout: Duration,
}

impl SfnExecutionStatusChecker {
    pub fn new(client: aws_sdk_sfn::Client) -> Self {
        Self {
            client,
            timeout: DESCRIBE_EXECUTION_TIMEOUT,
        }
    }

    pub fn with_timeout(client: aws_sdk_sfn::Client, timeout: Duration) -> Self {
        Self { client, timeout }
    }
}

impl ExecutionStatusChecker for SfnExecutionStatusChecker {
    fn describe_execution(&self, arn: &str) -> Result<Option<ExecutionOutcome>, McpHandlerError> {
        let client = self.client.clone();
        let arn = arn.to_owned();
        let timeout = self.timeout;
        let output = std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().map_err(|error| {
                McpHandlerError::Internal(format!(
                    "DescribeExecution runtime creation failed: {error}"
                ))
            })?;
            runtime.block_on(async move {
                tokio::time::timeout(
                    timeout,
                    client.describe_execution().execution_arn(arn).send(),
                )
                .await
                .map_err(|error| {
                    McpHandlerError::Internal(format!("DescribeExecution timed out: {error}"))
                })?
                .map_err(|error| McpHandlerError::Internal(format!("DescribeExecution: {error}")))
            })
        })
        .join()
        .map_err(|_panic| {
            McpHandlerError::Internal("DescribeExecution thread panicked".to_owned())
        })??;

        sfn_execution_outcome(output).map(Some)
    }
}

#[derive(Debug, Error)]
pub enum McpHandlerError {
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl McpHandlerError {
    pub fn json_rpc_code(&self) -> i32 {
        match self {
            Self::InvalidParams(_) => -32602,
            Self::NotFound(_) => -32004,
            Self::Internal(_) => -32603,
        }
    }
}

pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        external_code_search_def(),
        external_code_read_def(),
        external_code_callers_def(),
        external_code_callees_def(),
        external_knowledge_context_def(),
        external_index_def(),
        external_index_status_def(),
    ]
}

#[expect(
    clippy::future_not_send,
    clippy::unused_async,
    reason = "public MCP entry point is required to be async while the DuckDB-backed implementation is synchronous"
)]
pub async fn handle_tool(
    name: &str,
    args: &Value,
    db: &Connection,
    catalog: &CatalogResolver,
) -> Result<Value, McpHandlerError> {
    handle_tool_sync(name, args, db, catalog)
}

pub fn handle_tool_sync(
    name: &str,
    args: &Value,
    db: &Connection,
    catalog: &CatalogResolver,
) -> Result<Value, McpHandlerError> {
    match name {
        "external_code_search" => handle_code_search(args, db, catalog),
        "external_code_read" => handle_code_read(args, db, catalog),
        "external_code_callers" => handle_code_callers(args, db, catalog),
        "external_code_callees" => handle_code_callees(args, db, catalog),
        "external_knowledge_context" => handle_knowledge_context(args, db, catalog),
        "external_index" => handle_index_requires_lambda(args),
        "external_index_status" => handle_index_status_requires_lambda(args),
        other => Err(McpHandlerError::InvalidParams(format!(
            "unknown context-service MCP tool: {other}"
        ))),
    }
}

fn handle_code_search(
    args: &Value,
    db: &Connection,
    catalog: &CatalogResolver,
) -> Result<Value, McpHandlerError> {
    let args: CodeSearchArgs = parse_args(args)?;
    args.validate()?;
    let source = args.source();
    let resolved = resolve_revision(catalog, source, &args.package, args.revision_ref())?;
    let result = query::search_symbols(
        db,
        &SearchOptions {
            source: resolved.source,
            package: resolved.package,
            revision: resolved.revision,
            query: args.query,
            mode: SearchMode::Substring,
            symbol_kind: args.symbol_kind,
            file_glob: None,
            limit: args.limit.unwrap_or(20),
        },
    )
    .map_err(internal_error("external_code_search failed"))?;
    json_value(result)
}

fn handle_code_read(
    args: &Value,
    db: &Connection,
    catalog: &CatalogResolver,
) -> Result<Value, McpHandlerError> {
    let args: CodeReadArgs = parse_args(args)?;
    let selector = normalize_selector(&args.selector, catalog)?;
    let source = query::read_symbol(db, &selector, args.context_lines.unwrap_or(0))
        .map_err(internal_error("external_code_read failed"))?
        .ok_or_else(|| McpHandlerError::NotFound(format!("symbol not found: {}", args.selector)))?;
    json_value(source)
}

fn handle_code_callers(
    args: &Value,
    db: &Connection,
    catalog: &CatalogResolver,
) -> Result<Value, McpHandlerError> {
    let args: CodeCallersArgs = parse_args(args)?;
    let selector = normalize_selector(&args.selector, catalog)?;
    let result = query::find_callers(db, &selector, args.include_unresolved.unwrap_or(false))
        .map_err(internal_error("external_code_callers failed"))?;
    json_value(result)
}

fn handle_code_callees(
    args: &Value,
    db: &Connection,
    catalog: &CatalogResolver,
) -> Result<Value, McpHandlerError> {
    let args: CodeCalleesArgs = parse_args(args)?;
    let selector = normalize_selector(&args.selector, catalog)?;
    let result = query::find_callees(db, &selector, args.include_unresolved.unwrap_or(false))
        .map_err(internal_error("external_code_callees failed"))?;
    json_value(result)
}

fn handle_knowledge_context(
    args: &Value,
    db: &Connection,
    catalog: &CatalogResolver,
) -> Result<Value, McpHandlerError> {
    let args: KnowledgeContextArgs = parse_args(args)?;
    args.validate()?;
    let source = args.source();
    let resolved = resolve_revision(catalog, source, &args.package, args.revision_ref())?;
    let result = knowledge::query_knowledge_context(
        db,
        &KnowledgeContextOptions {
            query: args.query,
            source: resolved.source,
            package: resolved.package,
            revision: resolved.revision,
            scope: args.scope.unwrap_or(KnowledgeScope::All),
            limit: args.limit.unwrap_or(8),
            query_vec: args.query_vec,
        },
    )
    .map_err(internal_error("external_knowledge_context failed"))?;
    json_value(result)
}

fn handle_index_requires_lambda(args: &Value) -> Result<Value, McpHandlerError> {
    let args: ExternalIndexArgs = parse_args(args)?;
    args.validate()?;
    Err(McpHandlerError::Internal(
        "external_index requires Lambda routing with a Step Functions client".to_owned(),
    ))
}

fn handle_index_status_requires_lambda(args: &Value) -> Result<Value, McpHandlerError> {
    let args: ExternalIndexStatusArgs = parse_args(args)?;
    args.validate()?;
    Err(McpHandlerError::Internal(
        "external_index_status requires Lambda routing with a job store".to_owned(),
    ))
}

#[expect(
    clippy::future_not_send,
    reason = "handler borrows a DuckDB connection, which is intentionally not Sync"
)]
pub async fn route_index(
    args: &Value,
    _db: &Connection,
    catalog: &CatalogResolver,
    jobs: &dyn JobStore,
    sfn_client: &impl IndexExecutionStarter,
    caller_id: &str,
) -> Result<Value, McpHandlerError> {
    let args: ExternalIndexArgs = parse_args(args)?;
    args.validate()?;

    let parsed_url =
        match abuse::validate(&args.source_url, &ValidateOptions::default()).and_then(|parsed| {
            abuse::resolve_and_check_dns(&parsed)?;
            Ok(parsed)
        }) {
            Ok(parsed) => parsed,
            Err(error) => {
                return Ok(json!({
                    "status": "rejected",
                    "reason": format!("source_url: {error}")
                }));
            }
        };

    if INDEX_RATE_LIMITER
        .get_or_init(RateLimiter::default)
        .check(caller_id)
        .is_err()
    {
        return Ok(json!({
            "status": "rejected",
            "reason": "rate_limit",
            "retry_after_seconds": RATE_LIMIT_RETRY_AFTER_SECONDS
        }));
    }

    let source = args.source();
    let revision = args.revision.trim();
    let source_url_hash = source_url_hash(&args.source_url);
    let source_kind = args.source_kind(parsed_url.source_kind);

    if !args.force.unwrap_or(false) {
        if let Some(resolved) =
            lookup_complete_catalog_revision(catalog, source, &args.package, revision)?
        {
            return Ok(json!({
                "status": "complete",
                "snapshot_id": resolved.snapshot_id,
                "revision": resolved.revision
            }));
        }
    }

    let outcome = jobs
        .create_or_get_active_job(CreateJobRequest {
            source: source.to_owned(),
            package: args.package.clone(),
            revision: revision.to_owned(),
            source_url: args.source_url.clone(),
            source_url_hash: source_url_hash.clone(),
            source_kind: source_kind_label(source_kind).to_owned(),
            caller_id: caller_id.to_owned(),
        })
        .await
        .map_err(jobs_error("external_index create_or_get_active_job failed"))?;

    let job = match outcome {
        CreateJobOutcome::Created(record) => record,
        CreateJobOutcome::Existing(record) => {
            return Ok(active_job_response(&record));
        }
    };

    let payload = json!({
        "job_id": job.job_id,
        "source": source,
        "package": args.package,
        "revision": revision,
        "source_url": args.source_url,
        "source_kind": source_kind_label(source_kind),
        "caller_id": caller_id
    });
    let execution_arn = match sfn_client
        .start_execution(IndexExecutionRequest {
            name: job.job_id.clone(),
            input: payload,
        })
        .await
    {
        Ok(execution_arn) => execution_arn,
        Err(error) => {
            let _ = jobs
                .mark_failed(&job.job_id, "start_execution", &error.to_string())
                .await;
            return Err(error);
        }
    };

    let job = match jobs
        .record_execution_started(&job.job_id, &execution_arn)
        .await
    {
        Ok(record) => record,
        Err(error) => {
            let detail = error.to_string();
            let _ = jobs
                .mark_failed(&job.job_id, "record_execution_started", &detail)
                .await;
            return Err(jobs_error(
                "external_index record_execution_started failed",
            )(error));
        }
    };

    Ok(active_job_response(&job))
}

pub async fn route_index_status(
    args: &Value,
    jobs: &dyn JobStore,
    checker: Option<&dyn ExecutionStatusChecker>,
) -> Result<Value, McpHandlerError> {
    let args: ExternalIndexStatusArgs = parse_args(args)?;
    args.validate()?;
    let Some(record) = jobs
        .lookup_job(&args.job_id)
        .await
        .map_err(jobs_error("external_index_status lookup failed"))?
    else {
        return Ok(json!({ "status": "not_found" }));
    };

    let record = update_stale_job(record, jobs, checker).await?;
    Ok(index_status_response(&record))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeSearchArgs {
    query: String,
    package: String,
    source: Option<String>,
    revision: Option<String>,
    #[serde(rename = "ref")]
    ref_name: Option<String>,
    symbol_kind: Option<String>,
    limit: Option<usize>,
}

impl CodeSearchArgs {
    fn validate(&self) -> Result<(), McpHandlerError> {
        validate_non_empty("query", &self.query)?;
        validate_non_empty("package", &self.package)?;
        validate_revision_choice(self.revision.as_deref(), self.ref_name.as_deref())
    }

    fn source(&self) -> &str {
        self.source.as_deref().unwrap_or(DEFAULT_SOURCE)
    }

    fn revision_ref(&self) -> Option<&str> {
        self.revision.as_deref().or(self.ref_name.as_deref())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeReadArgs {
    selector: String,
    context_lines: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeCallersArgs {
    selector: String,
    include_unresolved: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeCalleesArgs {
    selector: String,
    include_unresolved: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KnowledgeContextArgs {
    query: String,
    package: String,
    source: Option<String>,
    revision: Option<String>,
    #[serde(rename = "ref")]
    ref_name: Option<String>,
    scope: Option<KnowledgeScope>,
    limit: Option<usize>,
    query_vec: Option<Vec<f32>>,
}

impl KnowledgeContextArgs {
    fn validate(&self) -> Result<(), McpHandlerError> {
        validate_non_empty("query", &self.query)?;
        validate_non_empty("package", &self.package)?;
        validate_revision_choice(self.revision.as_deref(), self.ref_name.as_deref())?;
        validate_query_vec(self.query_vec.as_deref())
    }

    fn source(&self) -> &str {
        self.source.as_deref().unwrap_or(DEFAULT_SOURCE)
    }

    fn revision_ref(&self) -> Option<&str> {
        self.revision.as_deref().or(self.ref_name.as_deref())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalIndexArgs {
    package: String,
    revision: String,
    source_url: String,
    source_kind: Option<ExternalIndexSourceKind>,
    source: Option<String>,
    force: Option<bool>,
}

impl ExternalIndexArgs {
    fn validate(&self) -> Result<(), McpHandlerError> {
        validate_non_empty("package", &self.package)?;
        validate_non_empty("revision", &self.revision)?;
        validate_non_empty("source_url", &self.source_url)?;
        if let Some(source) = self.source.as_deref() {
            validate_non_empty("source", source)?;
        }
        Ok(())
    }

    fn source(&self) -> &str {
        self.source.as_deref().unwrap_or(DEFAULT_INDEX_SOURCE)
    }

    fn source_kind(&self, inferred: SourceKind) -> SourceKind {
        match self.source_kind {
            Some(ExternalIndexSourceKind::Git) => SourceKind::Git,
            Some(ExternalIndexSourceKind::Tarball) => SourceKind::Tarball,
            None => inferred,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ExternalIndexSourceKind {
    Git,
    Tarball,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalIndexStatusArgs {
    job_id: String,
}

impl ExternalIndexStatusArgs {
    fn validate(&self) -> Result<(), McpHandlerError> {
        validate_non_empty("job_id", &self.job_id)
    }
}

#[derive(Debug)]
struct ParsedExternalSelector {
    package: String,
    revision_or_ref: Option<String>,
    qualified_name: String,
}

fn parse_args<T>(args: &Value) -> Result<T, McpHandlerError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(args.clone()).map_err(|error| {
        McpHandlerError::InvalidParams(format!("failed to parse tool arguments: {error}"))
    })
}

fn validate_non_empty(field: &str, value: &str) -> Result<(), McpHandlerError> {
    if value.trim().is_empty() {
        return Err(McpHandlerError::InvalidParams(format!(
            "field '{field}' must be non-empty"
        )));
    }
    Ok(())
}

fn validate_revision_choice(
    revision: Option<&str>,
    ref_name: Option<&str>,
) -> Result<(), McpHandlerError> {
    if revision.is_some() && ref_name.is_some() {
        return Err(McpHandlerError::InvalidParams(
            "use either 'revision' or 'ref', not both".to_owned(),
        ));
    }
    Ok(())
}

fn validate_query_vec(query_vec: Option<&[f32]>) -> Result<(), McpHandlerError> {
    let Some(query_vec) = query_vec else {
        return Ok(());
    };

    if query_vec.len() != KNOWLEDGE_QUERY_VECTOR_DIMENSIONS {
        return Err(McpHandlerError::InvalidParams(format!(
            "field 'query_vec' must contain {KNOWLEDGE_QUERY_VECTOR_DIMENSIONS} floats"
        )));
    }
    if query_vec.iter().any(|value| !value.is_finite()) {
        return Err(McpHandlerError::InvalidParams(
            "field 'query_vec' must contain only finite floats".to_owned(),
        ));
    }
    Ok(())
}

fn resolve_revision(
    catalog: &CatalogResolver,
    source: &str,
    package: &str,
    revision_or_ref: Option<&str>,
) -> Result<ResolvedRevision, McpHandlerError> {
    let revision_or_ref = revision_or_ref.unwrap_or(DEFAULT_REF);
    catalog
        .resolve(source, package, revision_or_ref)
        .map_err(catalog_error(format!(
            "{source}/{package}@{revision_or_ref}"
        )))
}

fn lookup_complete_catalog_revision(
    catalog: &CatalogResolver,
    source: &str,
    package: &str,
    revision: &str,
) -> Result<Option<ResolvedRevision>, McpHandlerError> {
    let resolved = match catalog.resolve(source, package, revision) {
        Ok(resolved) => resolved,
        Err(error) => {
            let message = format!("{error:#}");
            if message.contains("not found") {
                return Ok(None);
            }
            return Err(McpHandlerError::Internal(format!(
                "{source}/{package}@{revision}: {message}"
            )));
        }
    };

    let status: Option<String> = optional_no_rows(
        catalog.connection().query_row(
            r"
            SELECT index_status
            FROM package_catalog
            WHERE source = ? AND package = ? AND revision = ?
            LIMIT 1
            ",
            params![source, package, resolved.revision.as_str()],
            |row| row.get(0),
        ),
        "external_index catalog status lookup failed",
    )?;

    if status.as_deref() == Some("complete") {
        Ok(Some(resolved))
    } else {
        Ok(None)
    }
}

fn normalize_selector(
    selector: &str,
    catalog: &CatalogResolver,
) -> Result<String, McpHandlerError> {
    let parsed = parse_external_selector(selector)?;
    let resolved = resolve_revision(
        catalog,
        DEFAULT_SOURCE,
        &parsed.package,
        parsed.revision_or_ref.as_deref(),
    )?;
    Ok(format!(
        "pkg:{}@{}::{}",
        resolved.package, resolved.revision, parsed.qualified_name
    ))
}

fn parse_external_selector(selector: &str) -> Result<ParsedExternalSelector, McpHandlerError> {
    let trimmed = selector.trim();
    let Some(selector_body) = trimmed.strip_prefix("pkg:") else {
        return Err(McpHandlerError::InvalidParams(format!(
            "external selector must start with 'pkg:': {selector}"
        )));
    };
    let Some((package_revision, qualified_name)) = selector_body.split_once("::") else {
        return Err(McpHandlerError::InvalidParams(format!(
            "external selector must include a package and symbol path: {selector}"
        )));
    };
    if qualified_name.is_empty() {
        return Err(McpHandlerError::InvalidParams(format!(
            "external selector must include a symbol path: {selector}"
        )));
    }

    let (package, revision_or_ref) = match package_revision.split_once('@') {
        Some((package, revision_or_ref)) if !package.is_empty() && !revision_or_ref.is_empty() => {
            (package.to_owned(), Some(revision_or_ref.to_owned()))
        }
        Some(_) => {
            return Err(McpHandlerError::InvalidParams(format!(
                "external selector has an invalid package revision: {selector}"
            )))
        }
        None if !package_revision.is_empty() => (package_revision.to_owned(), None),
        None => {
            return Err(McpHandlerError::InvalidParams(format!(
                "external selector must include a package: {selector}"
            )))
        }
    };

    Ok(ParsedExternalSelector {
        package,
        revision_or_ref,
        qualified_name: qualified_name.to_owned(),
    })
}

fn catalog_error(target: String) -> impl FnOnce(anyhow::Error) -> McpHandlerError {
    move |error| {
        let message = format!("{error:#}");
        if message.contains("not found") {
            McpHandlerError::NotFound(format!("{target}: {message}"))
        } else {
            McpHandlerError::Internal(format!("{target}: {message}"))
        }
    }
}

fn internal_error(context: &'static str) -> impl FnOnce(anyhow::Error) -> McpHandlerError {
    move |error| McpHandlerError::Internal(format!("{context}: {error:#}"))
}

fn jobs_error(context: &'static str) -> impl FnOnce(JobsError) -> McpHandlerError {
    move |error| match error {
        JobsError::NotFound => McpHandlerError::NotFound(format!("{context}: {error}")),
        JobsError::Conflict => McpHandlerError::Internal(format!("{context}: {error}")),
        JobsError::Db(error) => McpHandlerError::Internal(format!("{context}: {error}")),
    }
}

fn optional_no_rows<T>(
    result: duckdb::Result<T>,
    context: &'static str,
) -> Result<Option<T>, McpHandlerError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(McpHandlerError::Internal(format!("{context}: {error}"))),
    }
}

fn json_value<T>(value: T) -> Result<Value, McpHandlerError>
where
    T: Serialize,
{
    serde_json::to_value(value).map_err(|error| {
        McpHandlerError::Internal(format!("failed to serialize response: {error}"))
    })
}

async fn update_stale_job(
    record: JobRecord,
    jobs: &dyn JobStore,
    checker: Option<&dyn ExecutionStatusChecker>,
) -> Result<JobRecord, McpHandlerError> {
    if !matches!(record.status, JobStatus::Queued | JobStatus::Running) {
        return Ok(record);
    }
    if !is_stale_job(&record) {
        return Ok(record);
    }

    let Some(checker) = checker else {
        return Ok(record);
    };
    let Some(execution_arn) = record.execution_arn.as_deref() else {
        return Ok(record);
    };

    let Ok(Some(outcome)) = checker.describe_execution(execution_arn) else {
        return Ok(record);
    };

    match outcome.status {
        ExecutionOutcomeStatus::Running => Ok(record),
        ExecutionOutcomeStatus::Succeeded => {
            let snapshot_id = outcome
                .output
                .as_ref()
                .and_then(|output| output.get("snapshot_id"))
                .and_then(Value::as_i64)
                .ok_or_else(|| {
                    McpHandlerError::Internal(
                        "DescribeExecution succeeded output missing snapshot_id".to_owned(),
                    )
                })?;
            let row_counts = outcome
                .output
                .as_ref()
                .and_then(|output| {
                    output
                        .get("rows_inserted")
                        .or_else(|| output.get("row_counts"))
                })
                .cloned()
                .unwrap_or_else(|| json!({}));

            jobs.mark_complete(&record.job_id, snapshot_id, row_counts)
                .await
                .map_err(jobs_error("external_index_status update complete failed"))
        }
        ExecutionOutcomeStatus::Failed => {
            let error = outcome
                .error
                .as_deref()
                .filter(|error| !error.trim().is_empty())
                .unwrap_or("execution: failed");
            let (code, detail) = split_job_error(error);
            jobs.mark_failed(&record.job_id, code, detail)
                .await
                .map_err(jobs_error(
                    "external_index_status update failed status failed",
                ))
        }
    }
}

fn is_stale_job(record: &JobRecord) -> bool {
    updated_at_age(&record.updated_at).is_some_and(|age| age >= STALE_JOB_REPAIR_AFTER)
}

fn updated_at_age(updated_at: &str) -> Option<Duration> {
    let updated_millis = updated_at.parse::<u128>().ok()?;
    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    let elapsed = now_millis.saturating_sub(updated_millis);
    Some(Duration::from_millis(
        elapsed.min(u128::from(u64::MAX)) as u64,
    ))
}

fn sfn_execution_outcome(
    output: aws_sdk_sfn::operation::describe_execution::DescribeExecutionOutput,
) -> Result<ExecutionOutcome, McpHandlerError> {
    let status = match output.status() {
        AwsExecutionStatus::Succeeded => ExecutionOutcomeStatus::Succeeded,
        AwsExecutionStatus::Running | AwsExecutionStatus::PendingRedrive => {
            ExecutionOutcomeStatus::Running
        }
        AwsExecutionStatus::Failed | AwsExecutionStatus::Aborted | AwsExecutionStatus::TimedOut => {
            ExecutionOutcomeStatus::Failed
        }
        other => match other.as_str() {
            "SUCCEEDED" => ExecutionOutcomeStatus::Succeeded,
            "RUNNING" | "PENDING_REDRIVE" => ExecutionOutcomeStatus::Running,
            _ => ExecutionOutcomeStatus::Failed,
        },
    };
    let output_value = output
        .output()
        .map(|raw| {
            serde_json::from_str(raw).map_err(|error| {
                McpHandlerError::Internal(format!("DescribeExecution output JSON: {error}"))
            })
        })
        .transpose()?;
    let error = execution_error_message(&status, &output);

    Ok(ExecutionOutcome {
        status,
        output: output_value,
        error,
    })
}

fn execution_error_message(
    status: &ExecutionOutcomeStatus,
    output: &aws_sdk_sfn::operation::describe_execution::DescribeExecutionOutput,
) -> Option<String> {
    match (output.error(), output.cause()) {
        (Some(error), Some(cause)) if !cause.trim().is_empty() => Some(format!("{error}: {cause}")),
        (Some(error), _) => Some(error.to_owned()),
        (None, Some(cause)) if !cause.trim().is_empty() => Some(format!("execution: {cause}")),
        (None, _) if *status == ExecutionOutcomeStatus::Failed => {
            Some(format!("execution: {}", output.status().as_str()))
        }
        _ => None,
    }
}

fn source_url_hash(source_url: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in source_url.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv1a64:{hash:016x}")
}

fn source_kind_label(source_kind: SourceKind) -> &'static str {
    match source_kind {
        SourceKind::Git => "git",
        SourceKind::Tarball => "tarball",
    }
}

fn active_job_response(record: &JobRecord) -> Value {
    json!({
        "job_id": record.job_id,
        "status": record.status.to_string(),
        "execution_arn": record.execution_arn,
        "revision": record.revision
    })
}

fn index_status_response(record: &JobRecord) -> Value {
    let mut response = json!({
        "job_id": record.job_id,
        "status": record.status.to_string(),
        "revision": record.revision,
        "created_at": record.created_at,
        "updated_at": record.updated_at,
        "attempt": record.attempt,
        "execution_arn": record.execution_arn
    });

    if let Some(stage) = record.stage.as_deref() {
        response["stage"] = json!(stage);
    }
    if let Some(snapshot_id) = record.snapshot_id {
        response["snapshot_id"] = json!(snapshot_id);
    }
    if let Some(row_counts) = record.row_counts.as_ref() {
        response["row_counts"] = row_counts.clone();
    }
    if record.status == JobStatus::Failed {
        if record.error_code.is_some() || record.error_detail.is_some() {
            response["error"] = job_error_response(
                record.error_code.as_deref().unwrap_or("execution"),
                record.error_detail.as_deref().unwrap_or("failed"),
            );
        }
    }

    response
}

fn split_job_error(error: &str) -> (&str, &str) {
    error.split_once(':').map_or_else(
        || (error.trim(), ""),
        |(code, detail)| (code.trim(), detail.trim()),
    )
}

fn job_error_response(code: &str, detail: &str) -> Value {
    json!({
        "code": code,
        "detail": detail,
        "retriable": matches!(code, "fetch" | "commit" | "spot_interrupted" | "timeout")
    })
}

fn external_code_search_def() -> ToolDefinition {
    ToolDefinition {
        name: "external_code_search".to_owned(),
        description: "Search symbols in an indexed external package revision.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["query", "package"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Symbol name, pattern, or qualified name."
                },
                "package": {
                    "type": "string",
                    "description": "Package name, for example serde or tokio."
                },
                "source": {
                    "type": "string",
                    "default": DEFAULT_SOURCE,
                    "description": "Package source, for example registry:crates-io or git:github.com/..."
                },
                "revision": {
                    "type": "string",
                    "description": "Exact version or SHA. Omit for latest."
                },
                "ref": {
                    "type": "string",
                    "description": "Branch or tag name. Alternative to revision."
                },
                "symbol_kind": {
                    "type": "string",
                    "description": "Optional symbol kind filter such as function, struct, or trait."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 200,
                    "default": 20
                }
            },
            "additionalProperties": false
        }),
    }
}

fn external_code_read_def() -> ToolDefinition {
    ToolDefinition {
        name: "external_code_read".to_owned(),
        description: "Read source for one external package symbol selector.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["selector"],
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "External selector such as pkg:serde@1.0.152::serde::de::Deserialize."
                },
                "context_lines": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Surrounding context lines."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn external_code_callers_def() -> ToolDefinition {
    ToolDefinition {
        name: "external_code_callers".to_owned(),
        description: "List symbols that call the requested external package symbol.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["selector"],
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "External selector such as pkg:serde@1.0.152::serde::de::Deserialize."
                },
                "include_unresolved": {
                    "type": "boolean",
                    "default": false,
                    "description": "Include cross-package labeled edges."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn external_code_callees_def() -> ToolDefinition {
    ToolDefinition {
        name: "external_code_callees".to_owned(),
        description: "List symbols called by the requested external package symbol.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["selector"],
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "External selector such as pkg:serde@1.0.152::serde::de::Deserialize."
                },
                "include_unresolved": {
                    "type": "boolean",
                    "default": false,
                    "description": "Include cross-package labeled edges."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn external_knowledge_context_def() -> ToolDefinition {
    ToolDefinition {
        name: "external_knowledge_context".to_owned(),
        description: "Retrieve a structured evidence pack for a natural-language question about an indexed external package.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["query", "package"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural-language query, for example how to deserialize JSON with serde."
                },
                "package": {
                    "type": "string",
                    "description": "Package name, for example serde or tokio."
                },
                "source": {
                    "type": "string",
                    "default": DEFAULT_SOURCE,
                    "description": "Package source, for example registry:crates-io or git:github.com/..."
                },
                "revision": {
                    "type": "string",
                    "description": "Exact version or SHA. Omit for latest."
                },
                "ref": {
                    "type": "string",
                    "description": "Branch or tag name. Alternative to revision."
                },
                "scope": {
                    "type": "string",
                    "enum": ["code", "docs", "all"],
                    "default": "all"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 20,
                    "default": 8
                },
                "query_vec": {
                    "type": "array",
                    "items": { "type": "number" },
                    "minItems": KNOWLEDGE_QUERY_VECTOR_DIMENSIONS,
                    "maxItems": KNOWLEDGE_QUERY_VECTOR_DIMENSIONS,
                    "description": "Optional precomputed query embedding. When omitted, retrieval gracefully degrades to BM25-only."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn external_index_def() -> ToolDefinition {
    ToolDefinition {
        name: "external_index".to_owned(),
        description: "Queue on-demand indexing for a fetchable external package source.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["package", "revision", "source_url"],
            "properties": {
                "package": {
                    "type": "string",
                    "description": "Package name, for example serde or tokio."
                },
                "revision": {
                    "type": "string",
                    "description": "Version, branch, tag, or SHA to index."
                },
                "source_url": {
                    "type": "string",
                    "description": "Fetchable git or tarball URL for the source."
                },
                "source_kind": {
                    "type": "string",
                    "enum": ["git", "tarball"],
                    "description": "Optional source fetch strategy. When omitted it is inferred from source_url."
                },
                "source": {
                    "type": "string",
                    "default": DEFAULT_INDEX_SOURCE,
                    "description": "Catalog source namespace for this package revision."
                },
                "force": {
                    "type": "boolean",
                    "default": false,
                    "description": "Bypass the warm-path catalog hit check."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn external_index_status_def() -> ToolDefinition {
    ToolDefinition {
        name: "external_index_status".to_owned(),
        description: "Return the queued indexing job status for a job_id.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["job_id"],
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "Job identifier returned by external_index."
                }
            },
            "additionalProperties": false
        }),
    }
}

//! Index job control-plane records and status transitions.

use std::{
    collections::HashMap,
    env,
    error::Error as StdError,
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use aws_sdk_dynamodb::{
    error::SdkError,
    operation::transact_write_items::TransactWriteItemsError,
    types::{AttributeValue, Delete, Put, ReturnValue, TransactWriteItem},
    Client as DynamoDbClient,
};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, JobsError>;

const DEFAULT_INDEX_JOBS_TABLE: &str = "spur-context-index-jobs";
const JOB_PK_PREFIX: &str = "JOB#";
const DEDUPE_PK_PREFIX: &str = "DEDUP#";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Complete,
    Failed,
    Partial,
}

impl JobStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Partial => "partial",
        }
    }

    fn from_str(value: &str) -> std::result::Result<Self, InvalidJobStatus> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "complete" => Ok(Self::Complete),
            "failed" => Ok(Self::Failed),
            "partial" => Ok(Self::Partial),
            _ => Err(InvalidJobStatus(value.to_string())),
        }
    }

}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct JobKey {
    pub source: String,
    pub package: String,
    pub revision: String,
    pub source_url_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CreateJobRequest {
    pub source: String,
    pub package: String,
    pub revision: String,
    pub source_url: String,
    pub source_url_hash: String,
    pub source_kind: String,
    pub caller_id: String,
}

impl CreateJobRequest {
    pub fn key(&self) -> JobKey {
        JobKey {
            source: self.source.clone(),
            package: self.package.clone(),
            revision: self.revision.clone(),
            source_url_hash: self.source_url_hash.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JobRecord {
    pub job_id: String,
    pub status: JobStatus,
    pub source: String,
    pub package: String,
    pub revision: String,
    pub source_url: String,
    pub source_url_hash: String,
    pub source_kind: String,
    pub caller_id: String,
    pub execution_arn: Option<String>,
    pub attempt: u32,
    pub stage: Option<String>,
    pub snapshot_id: Option<i64>,
    pub row_counts: Option<serde_json::Value>,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl JobRecord {
    pub fn key(&self) -> JobKey {
        JobKey {
            source: self.source.clone(),
            package: self.package.clone(),
            revision: self.revision.clone(),
            source_url_hash: self.source_url_hash.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CreateJobOutcome {
    Created(JobRecord),
    Existing(JobRecord),
}

#[async_trait]
pub trait JobStore: Send + Sync {
    async fn create_or_get_active_job(
        &self,
        request: CreateJobRequest,
    ) -> Result<CreateJobOutcome>;

    async fn record_execution_started(&self, job_id: &str, execution_arn: &str)
        -> Result<JobRecord>;

    async fn update_stage(
        &self,
        job_id: &str,
        status: JobStatus,
        stage: &str,
    ) -> Result<JobRecord>;

    async fn mark_complete(
        &self,
        job_id: &str,
        snapshot_id: i64,
        row_counts: serde_json::Value,
    ) -> Result<JobRecord>;

    async fn mark_failed(&self, job_id: &str, code: &str, detail: &str) -> Result<JobRecord>;

    async fn lookup_job(&self, job_id: &str) -> Result<Option<JobRecord>>;

    async fn release_dedupe_if_owner(&self, record: &JobRecord) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct DynamoDbJobStore {
    client: DynamoDbClient,
    table_name: String,
}

impl DynamoDbJobStore {
    pub fn new(client: DynamoDbClient) -> Self {
        let table_name = env::var("SPUR_INDEX_JOBS_TABLE")
            .unwrap_or_else(|_| DEFAULT_INDEX_JOBS_TABLE.to_string());
        Self { client, table_name }
    }

    pub fn with_table_name(client: DynamoDbClient, table_name: impl Into<String>) -> Self {
        Self {
            client,
            table_name: table_name.into(),
        }
    }

    pub fn table_name(&self) -> &str {
        &self.table_name
    }

    async fn lookup_dedupe_job(&self, key: &JobKey) -> Result<Option<JobRecord>> {
        let Some(item) = self.get_item(&dedupe_pk(key)).await? else {
            return Ok(None);
        };
        let job_id = string_attr(&item, "job_id")?;
        self.lookup_job(&job_id).await
    }

    async fn get_item(&self, pk: &str) -> Result<Option<HashMap<String, AttributeValue>>> {
        let output = self
            .client
            .get_item()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(pk.to_string()))
            .send()
            .await
            .map_err(dynamodb_error)?;
        Ok(output.item)
    }

    async fn update_job(
        &self,
        job_id: &str,
        update_expression: &str,
        names: HashMap<String, String>,
        values: HashMap<String, AttributeValue>,
    ) -> Result<JobRecord> {
        let output = self
            .client
            .update_item()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(job_pk(job_id)))
            .update_expression(update_expression)
            .set_expression_attribute_names(Some(names))
            .set_expression_attribute_values(Some(values))
            .condition_expression("attribute_exists(pk)")
            .return_values(ReturnValue::AllNew)
            .send()
            .await
            .map_err(dynamodb_error)?;
        let item = output.attributes.ok_or_else(|| {
            malformed_item(format!("update for job {job_id} returned no attributes"))
        })?;
        job_record_from_item(&item)
    }
}

#[async_trait]
impl JobStore for DynamoDbJobStore {
    async fn create_or_get_active_job(
        &self,
        request: CreateJobRequest,
    ) -> Result<CreateJobOutcome> {
        let now = now_string();
        let record = JobRecord {
            job_id: Uuid::new_v4().to_string(),
            status: JobStatus::Queued,
            source: request.source.clone(),
            package: request.package.clone(),
            revision: request.revision.clone(),
            source_url: request.source_url.clone(),
            source_url_hash: request.source_url_hash.clone(),
            source_kind: request.source_kind.clone(),
            caller_id: request.caller_id.clone(),
            execution_arn: None,
            attempt: 1,
            stage: None,
            snapshot_id: None,
            row_counts: None,
            error_code: None,
            error_detail: None,
            created_at: now.clone(),
            updated_at: now,
        };
        let key = request.key();
        let job_put = Put::builder()
            .table_name(&self.table_name)
            .set_item(Some(job_item(&record)))
            .condition_expression("attribute_not_exists(pk)")
            .build()
            .map_err(dynamodb_error)?;
        let dedupe_put = Put::builder()
            .table_name(&self.table_name)
            .set_item(Some(dedupe_item(&key, &record)))
            .condition_expression("attribute_not_exists(pk)")
            .build()
            .map_err(dynamodb_error)?;

        let result = self
            .client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().put(job_put).build())
            .transact_items(TransactWriteItem::builder().put(dedupe_put).build())
            .send()
            .await;

        match result {
            Ok(_) => Ok(CreateJobOutcome::Created(record)),
            Err(error) if is_transaction_conflict(&error) => {
                let existing = self.lookup_dedupe_job(&key).await?.ok_or(JobsError::Conflict)?;
                Ok(CreateJobOutcome::Existing(existing))
            }
            Err(error) => Err(dynamodb_error(error)),
        }
    }

    async fn record_execution_started(
        &self,
        job_id: &str,
        execution_arn: &str,
    ) -> Result<JobRecord> {
        let mut names = HashMap::new();
        names.insert("#execution_arn".to_string(), "execution_arn".to_string());
        names.insert("#updated_at".to_string(), "updated_at".to_string());
        let mut values = HashMap::new();
        values.insert(
            ":execution_arn".to_string(),
            AttributeValue::S(execution_arn.to_string()),
        );
        values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
        self.update_job(
            job_id,
            "SET #execution_arn = :execution_arn, #updated_at = :updated_at",
            names,
            values,
        )
        .await
    }

    async fn update_stage(
        &self,
        job_id: &str,
        status: JobStatus,
        stage: &str,
    ) -> Result<JobRecord> {
        let mut names = HashMap::new();
        names.insert("#status".to_string(), "status".to_string());
        names.insert("#stage".to_string(), "stage".to_string());
        names.insert("#updated_at".to_string(), "updated_at".to_string());
        let mut values = HashMap::new();
        values.insert(
            ":status".to_string(),
            AttributeValue::S(status.as_str().to_string()),
        );
        values.insert(":stage".to_string(), AttributeValue::S(stage.to_string()));
        values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
        self.update_job(
            job_id,
            "SET #status = :status, #stage = :stage, #updated_at = :updated_at",
            names,
            values,
        )
        .await
    }

    async fn mark_complete(
        &self,
        job_id: &str,
        snapshot_id: i64,
        row_counts: serde_json::Value,
    ) -> Result<JobRecord> {
        let mut names = HashMap::new();
        names.insert("#status".to_string(), "status".to_string());
        names.insert("#snapshot_id".to_string(), "snapshot_id".to_string());
        names.insert("#row_counts".to_string(), "row_counts".to_string());
        names.insert("#error_code".to_string(), "error_code".to_string());
        names.insert("#error_detail".to_string(), "error_detail".to_string());
        names.insert("#updated_at".to_string(), "updated_at".to_string());
        let mut values = HashMap::new();
        values.insert(
            ":status".to_string(),
            AttributeValue::S(JobStatus::Complete.as_str().to_string()),
        );
        values.insert(
            ":snapshot_id".to_string(),
            AttributeValue::N(snapshot_id.to_string()),
        );
        values.insert(
            ":row_counts".to_string(),
            AttributeValue::S(row_counts.to_string()),
        );
        values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
        let record = self
            .update_job(
                job_id,
                "SET #status = :status, #snapshot_id = :snapshot_id, #row_counts = :row_counts, #updated_at = :updated_at REMOVE #error_code, #error_detail",
                names,
                values,
            )
            .await?;
        self.release_dedupe_if_owner(&record).await?;
        Ok(record)
    }

    async fn mark_failed(&self, job_id: &str, code: &str, detail: &str) -> Result<JobRecord> {
        let mut names = HashMap::new();
        names.insert("#status".to_string(), "status".to_string());
        names.insert("#error_code".to_string(), "error_code".to_string());
        names.insert("#error_detail".to_string(), "error_detail".to_string());
        names.insert("#updated_at".to_string(), "updated_at".to_string());
        let mut values = HashMap::new();
        values.insert(
            ":status".to_string(),
            AttributeValue::S(JobStatus::Failed.as_str().to_string()),
        );
        values.insert(":error_code".to_string(), AttributeValue::S(code.to_string()));
        values.insert(
            ":error_detail".to_string(),
            AttributeValue::S(detail.to_string()),
        );
        values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
        let record = self
            .update_job(
                job_id,
                "SET #status = :status, #error_code = :error_code, #error_detail = :error_detail, #updated_at = :updated_at",
                names,
                values,
            )
            .await?;
        self.release_dedupe_if_owner(&record).await?;
        Ok(record)
    }

    async fn lookup_job(&self, job_id: &str) -> Result<Option<JobRecord>> {
        let Some(item) = self.get_item(&job_pk(job_id)).await? else {
            return Ok(None);
        };
        job_record_from_item(&item).map(Some)
    }

    async fn release_dedupe_if_owner(&self, record: &JobRecord) -> Result<()> {
        let delete = Delete::builder()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(dedupe_pk(&record.key())))
            .condition_expression("job_id = :job_id")
            .expression_attribute_values(":job_id", AttributeValue::S(record.job_id.clone()))
            .build()
            .map_err(dynamodb_error)?;
        let result = self
            .client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().delete(delete).build())
            .send()
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if is_transaction_conflict(&error) => Ok(()),
            Err(error) => Err(dynamodb_error(error)),
        }
    }
}

fn job_pk(job_id: &str) -> String {
    format!("{JOB_PK_PREFIX}{job_id}")
}

fn dedupe_pk(key: &JobKey) -> String {
    format!(
        "{DEDUPE_PK_PREFIX}{}#{}#{}#{}",
        key.source, key.package, key.revision, key.source_url_hash
    )
}

fn now_string() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    millis.to_string()
}

fn job_item(record: &JobRecord) -> HashMap<String, AttributeValue> {
    let mut item = HashMap::new();
    item.insert("pk".to_string(), AttributeValue::S(job_pk(&record.job_id)));
    item.insert("item_type".to_string(), AttributeValue::S("job".to_string()));
    item.insert(
        "job_id".to_string(),
        AttributeValue::S(record.job_id.clone()),
    );
    item.insert(
        "status".to_string(),
        AttributeValue::S(record.status.as_str().to_string()),
    );
    item.insert(
        "source".to_string(),
        AttributeValue::S(record.source.clone()),
    );
    item.insert(
        "package".to_string(),
        AttributeValue::S(record.package.clone()),
    );
    item.insert(
        "revision".to_string(),
        AttributeValue::S(record.revision.clone()),
    );
    item.insert(
        "source_url".to_string(),
        AttributeValue::S(record.source_url.clone()),
    );
    item.insert(
        "source_url_hash".to_string(),
        AttributeValue::S(record.source_url_hash.clone()),
    );
    item.insert(
        "source_kind".to_string(),
        AttributeValue::S(record.source_kind.clone()),
    );
    item.insert(
        "caller_id".to_string(),
        AttributeValue::S(record.caller_id.clone()),
    );
    item.insert(
        "attempt".to_string(),
        AttributeValue::N(record.attempt.to_string()),
    );
    item.insert(
        "created_at".to_string(),
        AttributeValue::S(record.created_at.clone()),
    );
    item.insert(
        "updated_at".to_string(),
        AttributeValue::S(record.updated_at.clone()),
    );
    insert_optional_string(&mut item, "execution_arn", record.execution_arn.as_deref());
    insert_optional_string(&mut item, "stage", record.stage.as_deref());
    insert_optional_number(&mut item, "snapshot_id", record.snapshot_id);
    if let Some(row_counts) = &record.row_counts {
        item.insert(
            "row_counts".to_string(),
            AttributeValue::S(row_counts.to_string()),
        );
    }
    insert_optional_string(&mut item, "error_code", record.error_code.as_deref());
    insert_optional_string(&mut item, "error_detail", record.error_detail.as_deref());
    item
}

fn dedupe_item(key: &JobKey, record: &JobRecord) -> HashMap<String, AttributeValue> {
    let mut item = HashMap::new();
    item.insert("pk".to_string(), AttributeValue::S(dedupe_pk(key)));
    item.insert(
        "item_type".to_string(),
        AttributeValue::S("dedupe".to_string()),
    );
    item.insert(
        "job_id".to_string(),
        AttributeValue::S(record.job_id.clone()),
    );
    item.insert(
        "source".to_string(),
        AttributeValue::S(key.source.clone()),
    );
    item.insert(
        "package".to_string(),
        AttributeValue::S(key.package.clone()),
    );
    item.insert(
        "revision".to_string(),
        AttributeValue::S(key.revision.clone()),
    );
    item.insert(
        "source_url_hash".to_string(),
        AttributeValue::S(key.source_url_hash.clone()),
    );
    item.insert(
        "created_at".to_string(),
        AttributeValue::S(record.created_at.clone()),
    );
    item.insert(
        "updated_at".to_string(),
        AttributeValue::S(record.updated_at.clone()),
    );
    item
}

fn insert_optional_string(
    item: &mut HashMap<String, AttributeValue>,
    name: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        item.insert(name.to_string(), AttributeValue::S(value.to_string()));
    }
}

fn insert_optional_number(
    item: &mut HashMap<String, AttributeValue>,
    name: &str,
    value: Option<i64>,
) {
    if let Some(value) = value {
        item.insert(name.to_string(), AttributeValue::N(value.to_string()));
    }
}

fn job_record_from_item(item: &HashMap<String, AttributeValue>) -> Result<JobRecord> {
    let status = JobStatus::from_str(&string_attr(item, "status")?)
        .map_err(|error| malformed_item(error.to_string()))?;
    Ok(JobRecord {
        job_id: string_attr(item, "job_id")?,
        status,
        source: string_attr(item, "source")?,
        package: string_attr(item, "package")?,
        revision: string_attr(item, "revision")?,
        source_url: string_attr(item, "source_url")?,
        source_url_hash: string_attr(item, "source_url_hash")?,
        source_kind: string_attr(item, "source_kind")?,
        caller_id: string_attr(item, "caller_id")?,
        execution_arn: optional_string_attr(item, "execution_arn")?,
        attempt: number_attr(item, "attempt")?
            .parse()
            .map_err(|error| malformed_item(format!("invalid attempt value for job item: {error}")))?,
        stage: optional_string_attr(item, "stage")?,
        snapshot_id: optional_number_attr(item, "snapshot_id")?,
        row_counts: optional_json_attr(item, "row_counts")?,
        error_code: optional_string_attr(item, "error_code")?,
        error_detail: optional_string_attr(item, "error_detail")?,
        created_at: string_attr(item, "created_at")?,
        updated_at: string_attr(item, "updated_at")?,
    })
}

fn string_attr(item: &HashMap<String, AttributeValue>, name: &str) -> Result<String> {
    match item.get(name) {
        Some(AttributeValue::S(value)) => Ok(value.clone()),
        Some(_) => Err(malformed_item(format!("attribute {name} is not a string"))),
        None => Err(malformed_item(format!("missing string attribute {name}"))),
    }
}

fn optional_string_attr(item: &HashMap<String, AttributeValue>, name: &str) -> Result<Option<String>> {
    match item.get(name) {
        Some(AttributeValue::S(value)) => Ok(Some(value.clone())),
        Some(_) => Err(malformed_item(format!("attribute {name} is not a string"))),
        None => Ok(None),
    }
}

fn number_attr(item: &HashMap<String, AttributeValue>, name: &str) -> Result<String> {
    match item.get(name) {
        Some(AttributeValue::N(value)) => Ok(value.clone()),
        Some(_) => Err(malformed_item(format!("attribute {name} is not a number"))),
        None => Err(malformed_item(format!("missing number attribute {name}"))),
    }
}

fn optional_number_attr(item: &HashMap<String, AttributeValue>, name: &str) -> Result<Option<i64>> {
    match item.get(name) {
        Some(AttributeValue::N(value)) => value.parse().map(Some).map_err(|error| {
            malformed_item(format!("invalid number attribute {name}: {error}"))
        }),
        Some(_) => Err(malformed_item(format!("attribute {name} is not a number"))),
        None => Ok(None),
    }
}

fn optional_json_attr(
    item: &HashMap<String, AttributeValue>,
    name: &str,
) -> Result<Option<serde_json::Value>> {
    match item.get(name) {
        Some(AttributeValue::S(value)) => serde_json::from_str(value).map(Some).map_err(|error| {
            malformed_item(format!("invalid json attribute {name}: {error}"))
        }),
        Some(_) => Err(malformed_item(format!(
            "attribute {name} is not a json string"
        ))),
        None => Ok(None),
    }
}

fn is_transaction_conflict(error: &SdkError<TransactWriteItemsError>) -> bool {
    match error {
        SdkError::ServiceError(service_error) => {
            transact_write_error_is_conflict(service_error.err())
        }
        _ => transaction_conflict_message(&error.to_string()),
    }
}

fn transact_write_error_is_conflict(error: &TransactWriteItemsError) -> bool {
    match error {
        TransactWriteItemsError::TransactionCanceledException(error) => error
            .cancellation_reasons()
            .iter()
            .any(|reason| cancellation_reason_is_conflict(reason.code()))
            || error.message().is_some_and(transaction_conflict_message),
        TransactWriteItemsError::TransactionInProgressException(_) => true,
        _ => transaction_conflict_message(&error.to_string()),
    }
}

fn cancellation_reason_is_conflict(code: Option<&str>) -> bool {
    matches!(code, Some("ConditionalCheckFailed" | "TransactionConflict"))
}

fn transaction_conflict_message(message: &str) -> bool {
    message.contains("TransactionCanceledException")
        || message.contains("ConditionalCheckFailed")
        || message.contains("TransactionConflict")
}

fn dynamodb_error(error: impl fmt::Display) -> JobsError {
    JobsError::Db(Box::new(StringJobError(format!(
        "dynamodb error: {error}"
    ))))
}

fn malformed_item(message: String) -> JobsError {
    JobsError::Db(Box::new(StringJobError(format!(
        "malformed index job item: {message}"
    ))))
}

#[derive(Debug)]
struct StringJobError(String);

impl fmt::Display for StringJobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl StdError for StringJobError {}

#[derive(Debug, thiserror::Error)]
pub enum JobsError {
    #[error("database error: {0}")]
    Db(Box<dyn StdError + Send + Sync>),
    #[error("conflicting index job")]
    Conflict,
    #[error("index job not found")]
    NotFound,
}

#[derive(Debug)]
struct InvalidJobStatus(String);

impl fmt::Display for InvalidJobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid index job status: {}", self.0)
    }
}

impl StdError for InvalidJobStatus {}

#[cfg(test)]
mod tests {
    use aws_sdk_dynamodb::{
        operation::transact_write_items::TransactWriteItemsError,
        types::{error::TransactionCanceledException, CancellationReason},
    };

    use super::*;

    #[test]
    fn transaction_canceled_conditional_check_is_conflict() {
        let error =
            TransactWriteItemsError::TransactionCanceledException(transaction_canceled_with_reason(
                "ConditionalCheckFailed",
            ));

        assert!(transact_write_error_is_conflict(&error));
    }

    #[test]
    fn transaction_canceled_transaction_conflict_is_conflict() {
        let error =
            TransactWriteItemsError::TransactionCanceledException(transaction_canceled_with_reason(
                "TransactionConflict",
            ));

        assert!(transact_write_error_is_conflict(&error));
    }

    #[test]
    fn transaction_canceled_validation_error_is_not_conflict() {
        let error =
            TransactWriteItemsError::TransactionCanceledException(transaction_canceled_with_reason(
                "ValidationError",
            ));

        assert!(!transact_write_error_is_conflict(&error));
    }

    fn transaction_canceled_with_reason(code: &str) -> TransactionCanceledException {
        TransactionCanceledException::builder()
            .cancellation_reasons(CancellationReason::builder().code(code).build())
            .build()
    }
}

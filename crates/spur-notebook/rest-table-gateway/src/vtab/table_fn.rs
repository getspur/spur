use std::{
    error::Error,
    sync::{Arc, Mutex},
};

use arrow_array::{Array, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, SchemaRef};
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId},
    vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab},
};

use crate::{
    adapter::{Adapter, ResolvedAuth, ScanRequest},
    error::{GatewayError, Result},
    vtab::bridge::IoBridge,
};

const CHUNK_SIZE: usize = 2048;

#[derive(Clone)]
pub struct ApiTableExtra {
    pub bridge: Arc<IoBridge>,
    pub adapter: Arc<dyn Adapter>,
    pub table: String,
    pub schema: SchemaRef,
}

pub struct ApiBindData {
    pub bridge: Arc<IoBridge>,
    pub adapter: Arc<dyn Adapter>,
    pub table: String,
    pub schema: SchemaRef,
}

impl ApiBindData {
    pub fn from_extra(extra: &ApiTableExtra) -> Self {
        Self {
            bridge: Arc::clone(&extra.bridge),
            adapter: Arc::clone(&extra.adapter),
            table: extra.table.clone(),
            schema: Arc::clone(&extra.schema),
        }
    }
}

pub struct ApiInitData {
    rows: Vec<RecordBatch>,
    cursor: Mutex<ApiCursor>,
}

#[derive(Default)]
struct ApiCursor {
    batch_idx: usize,
    row_idx: usize,
}

pub struct ApiTableVTab;

impl VTab for ApiTableVTab {
    type InitData = ApiInitData;
    type BindData = ApiBindData;

    fn bind(bind: &BindInfo) -> std::result::Result<Self::BindData, Box<dyn Error>> {
        let extra = unsafe { &*bind.get_extra_info::<ApiTableExtra>() };
        for field in extra.schema.fields() {
            let logical_type = LogicalTypeHandle::from(arrow_to_duckdb_type(field.data_type())?);
            bind.add_result_column(field.name(), logical_type);
        }
        Ok(ApiBindData::from_extra(extra))
    }

    fn init(init: &InitInfo) -> std::result::Result<Self::InitData, Box<dyn Error>> {
        let bind_data = unsafe { &*init.get_bind_data::<ApiBindData>() };
        let rows = bind_data.bridge.call(
            Arc::clone(&bind_data.adapter),
            ScanRequest {
                table: bind_data.table.clone(),
                predicates: vec![],
                projection: None,
                tvf_args: vec![],
                auth: ResolvedAuth::None,
            },
        )?;

        Ok(ApiInitData {
            rows,
            cursor: Mutex::new(ApiCursor::default()),
        })
    }

    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> std::result::Result<(), Box<dyn Error>> {
        let init_data = func.get_init_data();
        let bind_data = func.get_bind_data();
        let mut cursor = init_data
            .cursor
            .lock()
            .map_err(|err| GatewayError::Adapter(format!("table cursor lock poisoned: {err}")))?;

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

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        None
    }
}

pub fn arrow_to_duckdb_type(data_type: &DataType) -> Result<LogicalTypeId> {
    Ok(match data_type {
        DataType::Utf8 => LogicalTypeId::Varchar,
        DataType::Int64 => LogicalTypeId::Bigint,
        DataType::Float64 => LogicalTypeId::Double,
        DataType::Boolean => LogicalTypeId::Boolean,
        other => {
            return Err(GatewayError::Schema(format!(
                "unsupported Arrow type for DuckDB table function: {other:?}"
            )));
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
) -> Result<()> {
    for (column_idx, field) in schema.fields().iter().enumerate() {
        let column = batch.column(column_idx);
        match field.data_type() {
            DataType::Utf8 => {
                let array = column
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .ok_or_else(|| {
                        GatewayError::Schema(format!("column {} expected Utf8 array", field.name()))
                    })?;
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
                    .ok_or_else(|| {
                        GatewayError::Schema(format!(
                            "column {} expected Int64 array",
                            field.name()
                        ))
                    })?;
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
                    .ok_or_else(|| {
                        GatewayError::Schema(format!(
                            "column {} expected Float64 array",
                            field.name()
                        ))
                    })?;
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
                    .ok_or_else(|| {
                        GatewayError::Schema(format!(
                            "column {} expected Boolean array",
                            field.name()
                        ))
                    })?;
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
                return Err(GatewayError::Schema(format!(
                    "unsupported Arrow type for DuckDB table function: {other:?}"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::RecordBatch;
    use arrow_schema::{DataType, Field, Schema};
    use async_trait::async_trait;
    use duckdb::{
        core::{LogicalTypeHandle, LogicalTypeId},
        vtab::VTab,
    };

    use super::{arrow_to_duckdb_type, ApiBindData, ApiTableExtra, ApiTableVTab};
    use crate::{
        adapter::{Adapter, ScanRequest, TableDef},
        error::Result,
        vtab::bridge::IoBridge,
    };

    struct TestAdapter;

    #[async_trait]
    impl Adapter for TestAdapter {
        fn name(&self) -> &str {
            "test"
        }

        fn catalog(&self) -> Vec<TableDef> {
            vec![]
        }

        async fn scan(&self, _req: ScanRequest) -> Result<Vec<RecordBatch>> {
            Ok(vec![])
        }
    }

    #[test]
    fn arrow_to_duckdb_type_maps() {
        assert_eq!(
            arrow_to_duckdb_type(&DataType::Utf8).unwrap(),
            LogicalTypeId::Varchar
        );
        assert_eq!(
            arrow_to_duckdb_type(&DataType::Int64).unwrap(),
            LogicalTypeId::Bigint
        );
        assert_eq!(
            arrow_to_duckdb_type(&DataType::Float64).unwrap(),
            LogicalTypeId::Double
        );
        assert_eq!(
            arrow_to_duckdb_type(&DataType::Boolean).unwrap(),
            LogicalTypeId::Boolean
        );
        assert!(arrow_to_duckdb_type(&DataType::Int32).is_err());
    }

    #[test]
    fn zero_arg_parameters_and_bind_data_from_extra() {
        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Utf8, true)]));
        let adapter: Arc<dyn Adapter> = Arc::new(TestAdapter);
        let extra = ApiTableExtra {
            bridge: Arc::new(IoBridge::new()),
            adapter,
            table: "markets".to_string(),
            schema: schema.clone(),
        };

        assert!(ApiTableVTab::parameters().is_none());

        let bind_data = ApiBindData::from_extra(&extra);
        assert_eq!(bind_data.table, "markets");
        assert!(Arc::ptr_eq(&bind_data.schema, &schema));

        let logical_type = LogicalTypeHandle::from(
            arrow_to_duckdb_type(bind_data.schema.field(0).data_type()).unwrap(),
        );
        assert_eq!(logical_type.id(), LogicalTypeId::Varchar);
    }
}

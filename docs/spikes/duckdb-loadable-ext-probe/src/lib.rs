use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId},
    duckdb_entrypoint_c_api,
    vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab},
    Connection, Result,
};
use std::{error::Error, sync::Mutex};

const DEFAULT_ROWS: i64 = 3;

#[repr(C)]
struct ProbeBindData {
    row_count: i64,
}

#[repr(C)]
struct ProbeInitData {
    next_id: Mutex<i64>,
}

struct SpurProbeVTab;

impl VTab for SpurProbeVTab {
    type InitData = ProbeInitData;
    type BindData = ProbeBindData;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        bind.add_result_column("id", LogicalTypeHandle::from(LogicalTypeId::Bigint));
        bind.add_result_column("label", LogicalTypeHandle::from(LogicalTypeId::Varchar));

        let row_count = bind
            .get_named_parameter("n")
            .map(|value| value.to_int64())
            .unwrap_or(DEFAULT_ROWS);

        if row_count < 0 {
            return Err("spur_probe named parameter n must be non-negative".into());
        }

        bind.set_cardinality(row_count as u64, true);
        Ok(ProbeBindData { row_count })
    }

    fn init(_: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        Ok(ProbeInitData {
            next_id: Mutex::new(1),
        })
    }

    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn Error>> {
        let bind_data = func.get_bind_data();
        let init_data = func.get_init_data();

        let mut next_id = init_data
            .next_id
            .lock()
            .map_err(|err| format!("spur_probe cursor lock poisoned: {err}"))?;
        if *next_id > bind_data.row_count {
            output.set_len(0);
            return Ok(());
        }

        let mut id_vector = output.flat_vector(0);
        let label_vector = output.flat_vector(1);
        let capacity = id_vector.capacity();
        let remaining = (bind_data.row_count - *next_id + 1) as usize;
        let len = remaining.min(capacity);
        let id_slice = id_vector.as_mut_slice::<i64>();

        for (idx, id_slot) in id_slice.iter_mut().take(len).enumerate() {
            let id = *next_id + idx as i64;
            *id_slot = id;
            let label = format!("probe-{id}");
            label_vector.insert(idx, label.as_str());
        }

        *next_id += len as i64;
        output.set_len(len);
        Ok(())
    }

    fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
        Some(vec![(
            "n".to_string(),
            LogicalTypeHandle::from(LogicalTypeId::Bigint),
        )])
    }
}

#[duckdb_entrypoint_c_api(ext_name = "spur_probe", min_duckdb_version = "v1.2.0")]
pub fn extension_entrypoint(con: Connection) -> Result<(), Box<dyn Error>> {
    con.register_table_function::<SpurProbeVTab>("spur_probe")?;
    Ok(())
}

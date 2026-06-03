use std::sync::{mpsc, Arc};
use std::thread;

use arrow_array::RecordBatch;

use crate::adapter::{ActionRequest, Adapter, ScanRequest};
use crate::error::{GatewayError, Result};

enum Job {
    Scan(
        Arc<dyn Adapter>,
        ScanRequest,
        mpsc::Sender<Result<Vec<RecordBatch>>>,
    ),
    Act(
        Arc<dyn Adapter>,
        ActionRequest,
        mpsc::Sender<Result<Vec<RecordBatch>>>,
    ),
}

pub struct IoBridge {
    tx: mpsc::Sender<Job>,
}

impl IoBridge {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name("rest-gateway-io".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("io runtime");
                while let Ok(job) = rx.recv() {
                    match job {
                        Job::Scan(adapter, req, reply) => {
                            let res = rt.block_on(adapter.scan(req));
                            let _ = reply.send(res);
                        }
                        Job::Act(adapter, req, reply) => {
                            let res = rt.block_on(adapter.act(req));
                            let _ = reply.send(res);
                        }
                    }
                }
            })
            .expect("spawn io thread");
        Self { tx }
    }

    pub fn call(&self, adapter: Arc<dyn Adapter>, req: ScanRequest) -> Result<Vec<RecordBatch>> {
        let (rtx, rrx) = mpsc::channel();
        self.tx
            .send(Job::Scan(adapter, req, rtx))
            .map_err(|e| GatewayError::Adapter(format!("io bridge send: {e}")))?;
        rrx.recv()
            .map_err(|e| GatewayError::Adapter(format!("io bridge recv: {e}")))?
    }

    pub fn call_act(
        &self,
        adapter: Arc<dyn Adapter>,
        req: ActionRequest,
    ) -> Result<Vec<RecordBatch>> {
        let (rtx, rrx) = mpsc::channel();
        self.tx
            .send(Job::Act(adapter, req, rtx))
            .map_err(|e| GatewayError::Adapter(format!("io bridge send: {e}")))?;
        rrx.recv()
            .map_err(|e| GatewayError::Adapter(format!("io bridge recv: {e}")))?
    }
}

impl Default for IoBridge {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{Int64Array, RecordBatch};
    use arrow_schema::{DataType, Field, Schema};
    use async_trait::async_trait;

    use super::IoBridge;
    use crate::adapter::{ActionRequest, Adapter, ScanRequest, TableDef};
    use crate::error::Result;

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
            let schema = Arc::new(Schema::new(vec![Field::new(
                "value",
                DataType::Int64,
                false,
            )]));
            let values = Arc::new(Int64Array::from(vec![1, 2, 3]));
            Ok(vec![RecordBatch::try_new(schema, vec![values]).unwrap()])
        }
    }

    #[test]
    fn bridge_works_inside_outer_tokio_runtime() {
        let bridge = IoBridge::new();
        let adapter: Arc<dyn Adapter> = Arc::new(TestAdapter);
        let req = ScanRequest {
            table: "test".to_string(),
            predicates: vec![],
            projection: None,
            tvf_args: vec![],
            auth: Default::default(),
        };

        let outer = tokio::runtime::Runtime::new().unwrap();
        let result = outer.block_on(async {
            tokio::task::spawn_blocking(move || bridge.call(adapter, req))
                .await
                .unwrap()
        });

        let batches = result.unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 3);
    }

    #[test]
    fn bridge_dispatches_act() {
        struct ActAdapter;

        #[async_trait]
        impl Adapter for ActAdapter {
            fn name(&self) -> &str {
                "act"
            }

            fn catalog(&self) -> Vec<TableDef> {
                vec![]
            }

            async fn scan(&self, _req: ScanRequest) -> Result<Vec<RecordBatch>> {
                Ok(vec![])
            }

            async fn act(&self, _req: ActionRequest) -> Result<Vec<RecordBatch>> {
                let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Int64, false)]));
                let col = Arc::new(Int64Array::from(vec![7]));
                Ok(vec![RecordBatch::try_new(schema, vec![col]).unwrap()])
            }
        }

        let bridge = IoBridge::new();
        let adapter: Arc<dyn Adapter> = Arc::new(ActAdapter);
        let req = ActionRequest {
            name: "x".into(),
            method: "POST".into(),
            path: "/x".into(),
            query: vec![],
            body: None,
            auth: Default::default(),
            idempotency_key: None,
            dry_run: false,
        };

        let outer = tokio::runtime::Runtime::new().unwrap();
        let result = outer.block_on(async {
            tokio::task::spawn_blocking(move || bridge.call_act(adapter, req))
                .await
                .unwrap()
        });
        assert_eq!(result.unwrap()[0].num_rows(), 1);
    }
}

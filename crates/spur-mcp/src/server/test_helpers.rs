use super::McpCallbackServer;
use super::*;

impl McpCallbackServer {
    /// Test-only: invoke the `delegate_to_worker` JSON-RPC handler directly.
    ///
    /// Exposed solely so integration tests in sibling crates (e.g.
    /// `spur-core/tests/continuation_integration.rs`) can exercise the
    /// block-timeout / detached-completion paths without standing up the full
    /// HTTP stack. Returns the raw JSON-RPC response as a `serde_json::Value`.
    #[doc(hidden)]
    pub async fn __test_call_delegate_to_worker(&self, agent: &str, task: &str) -> Value {
        let resp = self
            .handle_delegate_to_worker(Value::from(1), json!({ "agent": agent, "task": task }))
            .await;
        serde_json::to_value(&resp).expect("serialize JsonRpcResponse")
    }

    /// Test-only: invoke the `cancel_delegation` JSON-RPC handler directly.
    ///
    /// Mirrors `__test_call_delegate_to_worker`: exposed solely so integration
    /// tests in sibling crates (e.g. `spur-core/tests/cancellation.rs`) can
    /// drive the INV-ASYNC-3 cancel path deterministically without standing
    /// up the full HTTP stack.
    #[doc(hidden)]
    pub async fn __test_call_cancel_delegation(&self, delegation_id: &str) -> Value {
        let resp = self
            .handle_cancel_delegation(Value::from(2), json!({ "delegation_id": delegation_id }))
            .await;
        serde_json::to_value(&resp).expect("serialize JsonRpcResponse")
    }

    /// Test-only: invoke the `delegate_parallel` JSON-RPC handler directly.
    ///
    /// Exposed for `crates/spur-mcp/tests/parallel_response_shape.rs` to exercise
    /// per-task parallelization without standing up the full HTTP stack.
    /// Returns the raw JSON-RPC response as a `serde_json::Value`.
    #[doc(hidden)]
    pub async fn __test_call_delegate_parallel(&self, tasks: Vec<(&str, &str)>) -> Value {
        let task_array: Value = Value::Array(
            tasks
                .iter()
                .enumerate()
                .map(|(idx, (agent, task))| {
                    json!({
                        "agent": agent,
                        "task": task,
                        "issue_id": format!("test-issue-{}", idx),
                    })
                })
                .collect(),
        );
        let args = json!({ "tasks": task_array });
        let resp = self.handle_delegate_parallel(Value::from(1), args).await;
        serde_json::to_value(&resp).expect("serialize JsonRpcResponse")
    }

    /// Test-only: invoke the `execute_epic` JSON-RPC handler directly.
    ///
    /// Exposed for integration tests that need to verify persisted label and
    /// audit behavior without standing up the full HTTP transport.
    #[doc(hidden)]
    pub async fn __test_call_execute_epic(
        &self,
        epic_id: &str,
        default_agent: Option<&str>,
    ) -> Value {
        let args = match default_agent {
            Some(agent) => json!({
                "epic_id": epic_id,
                "default_agent": agent,
            }),
            None => json!({
                "epic_id": epic_id,
            }),
        };
        let resp = self.handle_execute_epic(Value::from(1), args).await;
        serde_json::to_value(&resp).expect("serialize JsonRpcResponse")
    }

    /// Test-only: invoke the `submit_plan` JSON-RPC handler directly.
    ///
    /// Accepts a raw `arguments` object so integration tests can exercise both
    /// ephemeral and persisted submit paths without the HTTP transport.
    #[doc(hidden)]
    pub async fn __test_call_submit_plan(&self, arguments: Value) -> Value {
        let resp = self.handle_submit_plan(Value::from(1), arguments).await;
        serde_json::to_value(&resp).expect("serialize JsonRpcResponse")
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn __test_install_startup_recovery_probe(
        probe: Arc<StartupRecoveryProbe>,
    ) -> StartupRecoveryProbeGuard {
        *types::STARTUP_RECOVERY_PROBE.lock().unwrap() = Some(probe);
        StartupRecoveryProbeGuard
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn __test_request_startup_recovery(&self) {
        self.request_startup_recovery();
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn __test_drop_startup_recovery_handle(&self) {
        let handle = self.startup_recovery.lock().unwrap().handle.take();
        drop(handle);
    }

    /// Test-only: trigger the persisted-plan recovery path directly.
    #[doc(hidden)]
    pub async fn __test_recover_persisted_plans(&self) -> Result<(), String> {
        let Some(pm) = self.pm_service.clone() else {
            return Err("pm_service not configured".to_string());
        };
        self.recover_persisted_plans(pm)
            .await
            .map_err(|error| error.to_string())
    }

    /// Test-only: invoke any tool handler through the same JSON-RPC dispatch
    /// path used by the MCP transport.
    #[doc(hidden)]
    pub async fn __test_call_tool(&self, tool_name: &str, arguments: Value) -> Value {
        let response = self
            .handle_tool_call(
                Value::Null,
                json!({
                    "name": tool_name,
                    "arguments": arguments,
                }),
            )
            .await;
        serde_json::to_value(&response).expect("serialize JsonRpcResponse")
    }

    /// Test-only: install a plan state directly into the in-memory cache.
    #[doc(hidden)]
    pub async fn __test_install_plan(&self, state: crate::plan::PlanState) {
        self.install_projected_plan(state, false).await;
    }

    #[doc(hidden)]
    pub fn __test_set_pm_like(&mut self, pm: Arc<dyn crate::plan::PmLike>) {
        self.pm_service_like = Some(pm);
    }

    /// Test-only: mutate a cached plan entry into an impossible state so
    /// persisted read paths can prove they refresh from durable projection
    /// instead of trusting `active_plans`.
    #[doc(hidden)]
    pub async fn __test_corrupt_cached_plan(
        &self,
        plan_id: &str,
        task_id: &str,
        worker_branch: &str,
        base_snapshot_branch: &str,
    ) -> Result<(), String> {
        let plan = self
            .active_plans
            .lock()
            .await
            .get(plan_id)
            .cloned()
            .ok_or_else(|| format!("unknown cached plan '{plan_id}'"))?;
        let mut state = plan.state.lock().await;
        let entry = state
            .tasks
            .iter_mut()
            .find(|task| task.spec.task_id == task_id)
            .ok_or_else(|| format!("unknown task '{task_id}' in cached plan '{plan_id}'"))?;
        entry.status = crate::plan::PlanTaskStatus::Approved {
            summary: Some("corrupted-cache".into()),
        };
        entry.worker_branch = Some(worker_branch.to_string());
        state.base_snapshot_branch = Some(base_snapshot_branch.to_string());
        state.base_snapshot_oid = Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into());
        state.merge_state = crate::plan::PlanMergeState::Succeeded {
            merge_branch: "spur/bogus-merge".into(),
            merged_task_ids: vec![task_id.to_string()],
        };
        Ok(())
    }

    /// Test-only: peek whether a result is sitting in `completed_delegations`
    /// awaiting a `check_delegation_status` poll. Used to detect the
    /// double-delivery failure mode (map write AND continuation callback both
    /// firing for the same delegation).
    #[doc(hidden)]
    pub async fn __test_completed_has(&self, delegation_id: &str) -> bool {
        self.completed_delegations
            .lock()
            .await
            .contains_key(&DelegationId::from(delegation_id))
    }

    /// Test-only: current number of cached plan entries in `active_plans`.
    #[doc(hidden)]
    pub async fn __test_active_plan_count(&self) -> usize {
        self.active_plans.lock().await.len()
    }

    /// Test-only: clear the read-through active plan cache.
    #[doc(hidden)]
    pub async fn __test_clear_active_plans(&self) {
        self.active_plans.lock().await.clear();
    }

    /// Test-only: enable continuous audit-sequence churn for one epic.
    #[doc(hidden)]
    pub async fn __test_churn_beads_version_for_epic(&self, epic_id: impl Into<String>) {
        *self.version_churn_epic_for_test.lock().await = Some(epic_id.into());
    }

    /// Test-only: expose the raw load error instead of the tool-layer
    /// `Unknown plan_id` normalization.
    #[doc(hidden)]
    pub async fn __test_load_or_project_plan(
        &self,
        plan_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<crate::plan::PlanState>>, String> {
        self.load_or_project_plan(plan_id).await
    }

    /// Test-only: report whether the reconciler task has been spawned.
    #[doc(hidden)]
    pub fn __test_reconciler_running(&self) -> bool {
        self.reconciler_handle.lock().unwrap().is_some()
    }

    /// Test-only: report whether legacy startup recovery has been requested
    /// but is waiting for a bound brain_session_id.
    #[doc(hidden)]
    pub fn __test_startup_recovery_pending(&self) -> bool {
        self.startup_recovery.lock().unwrap().pending
    }

    /// Test-only: wait for the spawned startup recovery task to complete.
    #[doc(hidden)]
    pub async fn __test_wait_startup_recovery(&self) {
        let handle = self.startup_recovery.lock().unwrap().handle.take();
        if let Some(handle) = handle {
            handle.wait().await;
        }
    }
}

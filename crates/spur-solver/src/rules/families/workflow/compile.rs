//! Finite bounded-trace workflow validation and typed constraint lowering.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    rules::{
        compiler::{
            BoundedReachabilityScope, FamilyCompilation, FamilyCompileError, ModelProjection,
            RuleEvaluationScope, RuleFamilyCompiler,
        },
        manifest::validate_binding_contract,
        manifest_family_executable_rule_ids,
        manifest_format::NativeHandlerV1,
        primitives::{and, boolean, eq, or, request, var},
        CompiledRule, RuleSolveMode,
    },
    types::{
        ConstraintExpr, Variable, DEFAULT_TIMEOUT_MS, MAX_CONSTRAINTS, MAX_TIMEOUT_MS,
        MAX_VARIABLES,
    },
};

const MAX_WORKFLOW_HORIZON: u64 = MAX_CONSTRAINTS as u64;
const MAX_WORKFLOW_TRACE_SLOTS: usize = MAX_CONSTRAINTS * MAX_CONSTRAINTS;
const MAX_WORKFLOW_EXPRESSION_NODES: usize = MAX_CONSTRAINTS * MAX_VARIABLES;

/// Workflow compiler registered behind `solve_rules`.
pub static COMPILER: WorkflowCompiler = WorkflowCompiler;

/// Stateless bounded-trace workflow compiler.
pub struct WorkflowCompiler;

impl RuleFamilyCompiler for WorkflowCompiler {
    fn id(&self) -> &'static str {
        "workflow"
    }

    fn input_schema(&self) -> Value {
        input_schema()
    }

    fn compile(&self, input: Value) -> Result<FamilyCompilation, FamilyCompileError> {
        compile_with_evaluation_scope(input)
            .map(|(compiled, _)| compiled)
            .map_err(|message| FamilyCompileError::new(self.id(), message))
    }

    fn compile_with_evaluation_scope(
        &self,
        input: Value,
    ) -> Result<(FamilyCompilation, Option<RuleEvaluationScope>), FamilyCompileError> {
        compile_with_evaluation_scope(input)
            .map(|(compiled, scope)| (compiled, Some(scope)))
            .map_err(|message| FamilyCompileError::new(self.id(), message))
    }
}

fn compile_with_evaluation_scope(
    input: Value,
) -> Result<(FamilyCompilation, RuleEvaluationScope), String> {
    let input: WorkflowRequest =
        serde_json::from_value(input).map_err(|error| error.to_string())?;
    if input.family != "workflow" {
        return Err(format!(
            "workflow compiler requires family `workflow`, got `{}`",
            input.family
        ));
    }
    if input.rules.is_empty() {
        return Err("at least one workflow rule binding is required".to_owned());
    }
    if input.rules.len() > MAX_CONSTRAINTS {
        return Err(format!(
            "request has too many workflow rule bindings; maximum is {MAX_CONSTRAINTS}"
        ));
    }
    if !(1..=MAX_TIMEOUT_MS).contains(&input.timeout_ms) {
        return Err(format!(
            "timeout_ms must be in 1..={MAX_TIMEOUT_MS}, found {}",
            input.timeout_ms
        ));
    }
    if input.mode == RuleSolveMode::Verify && !input.unknowns.is_empty() {
        return Err("verification requires complete workflow traces and no unknowns".to_owned());
    }

    let bindings = input
        .rules
        .iter()
        .map(validate_manifest_binding)
        .collect::<Result<Vec<_>, _>>()?;
    let horizon = validate_facts(&input.facts, &input.unknowns, input.mode)?;
    let resolver = WorkflowResolver::new(input.facts, &input.unknowns, horizon)?;
    validate_expression_budget(&bindings, &resolver)?;
    let rules = bindings
        .iter()
        .enumerate()
        .map(|(index, binding)| {
            compile_binding(binding, &resolver).map(|predicate| {
                CompiledRule::new(binding.source.rule_id.clone(), index, predicate)
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let reachability = bindings
        .iter()
        .enumerate()
        .filter(|(_, binding)| {
            matches!(
                binding.handler,
                NativeHandlerV1::WorkflowBoundedReachability
            )
        })
        .map(|(binding_index, binding)| {
            let effective_bound = effective_reachability_bound(binding, horizon)?;
            Ok(BoundedReachabilityScope {
                rule_id: binding.source.rule_id.clone(),
                binding_index,
                effective_bound: u64::try_from(effective_bound)
                    .map_err(|_| "workflow reachability bound conversion overflow".to_owned())?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let evaluation_scope = RuleEvaluationScope::BoundedTrace {
        horizon: u64::try_from(horizon)
            .map_err(|_| "workflow horizon conversion overflow".to_owned())?,
        reachability,
    };

    let solver_request = request(
        "workflow",
        resolver.variables,
        &rules,
        input.timeout_ms,
        input.persist,
        input.include_smt,
    );
    solver_request
        .validate()
        .map_err(|error| format!("compiled workflow rules are invalid: {error}"))?;

    Ok((
        FamilyCompilation {
            mode: input.mode,
            request: solver_request,
            rules,
            projections: resolver.projections,
        },
        evaluation_scope,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowRequest {
    family: String,
    mode: RuleSolveMode,
    rules: Vec<WorkflowRuleBinding>,
    facts: WorkflowFacts,
    #[serde(default)]
    unknowns: Vec<WorkflowUnknown>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    persist: bool,
    #[serde(default)]
    include_smt: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowRuleBinding {
    rule_id: String,
    subjects: Vec<String>,
    #[serde(default)]
    parameters: WorkflowParameters,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkflowParameters {
    #[serde(skip_serializing_if = "Option::is_none")]
    bound: Option<u64>,
}

struct ValidatedWorkflowBinding<'a> {
    source: &'a WorkflowRuleBinding,
    handler: NativeHandlerV1,
    bound: Option<usize>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowFacts {
    horizon: u64,
    state_domain: Vec<String>,
    event_domain: Vec<String>,
    initial_states: Vec<String>,
    safe_states: Vec<String>,
    target_states: Vec<String>,
    enabled_transitions: Vec<TransitionFacts>,
    traces: BTreeMap<String, TraceFacts>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransitionFacts {
    step: u64,
    from: String,
    event: String,
    to: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceFacts {
    states: Vec<Option<String>>,
    events: Vec<Option<String>>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WorkflowUnknown {
    TraceState { trace: String, index: u64 },
    TraceEvent { trace: String, index: u64 },
}

impl WorkflowUnknown {
    fn trace(&self) -> &str {
        match self {
            Self::TraceState { trace, .. } | Self::TraceEvent { trace, .. } => trace,
        }
    }

    const fn index(&self) -> u64 {
        match self {
            Self::TraceState { index, .. } | Self::TraceEvent { index, .. } => *index,
        }
    }

    const fn kind(&self) -> SlotKind {
        match self {
            Self::TraceState { .. } => SlotKind::State,
            Self::TraceEvent { .. } => SlotKind::Event,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum SlotKind {
    State,
    Event,
}

struct WorkflowResolver {
    facts: WorkflowFacts,
    horizon: usize,
    unknown_variables: BTreeMap<(String, SlotKind, usize), String>,
    variables: Vec<Variable>,
    projections: Vec<ModelProjection>,
}

impl WorkflowResolver {
    fn new(
        facts: WorkflowFacts,
        unknowns: &[WorkflowUnknown],
        horizon: usize,
    ) -> Result<Self, String> {
        let mut unknown_variables = BTreeMap::new();
        let mut variables = Vec::with_capacity(unknowns.len());
        let mut projections = Vec::with_capacity(unknowns.len());
        for (position, unknown) in unknowns.iter().enumerate() {
            let index = usize::try_from(unknown.index())
                .map_err(|_| "workflow unknown index conversion overflow".to_owned())?;
            let variable = format!("workflow_u_{position}");
            let (values, field) = match unknown.kind() {
                SlotKind::State => (
                    facts.state_domain.clone(),
                    format!("traces.{}.states[{index}]", unknown.trace()),
                ),
                SlotKind::Event => (
                    facts.event_domain.clone(),
                    format!("traces.{}.events[{index}]", unknown.trace()),
                ),
            };
            variables.push(Variable::Enum {
                name: variable.clone(),
                values,
            });
            projections.push(ModelProjection {
                variable: variable.clone(),
                subject: unknown.trace().to_owned(),
                field,
            });
            unknown_variables.insert(
                (unknown.trace().to_owned(), unknown.kind(), index),
                variable,
            );
        }
        Ok(Self {
            facts,
            horizon,
            unknown_variables,
            variables,
            projections,
        })
    }

    fn require_trace(&self, trace: &str) -> Result<&TraceFacts, String> {
        self.facts
            .traces
            .get(trace)
            .ok_or_else(|| format!("unknown workflow trace `{trace}`"))
    }

    fn slot_matches(
        &self,
        trace: &str,
        kind: SlotKind,
        index: usize,
        label: &str,
    ) -> Result<ConstraintExpr, String> {
        let trace_facts = self.require_trace(trace)?;
        let fixed = match kind {
            SlotKind::State => trace_facts.states[index].as_deref(),
            SlotKind::Event => trace_facts.events[index].as_deref(),
        };
        Ok(match fixed {
            Some(value) => boolean(value == label),
            None => {
                let variable = self
                    .unknown_variables
                    .get(&(trace.to_owned(), kind, index))
                    .expect("null workflow slots require validated unknowns")
                    .clone();
                eq(
                    var(variable.clone()),
                    ConstraintExpr::EnumLabel {
                        var: variable,
                        label: label.to_owned(),
                    },
                )
            }
        })
    }

    fn state_in(
        &self,
        trace: &str,
        index: usize,
        states: &[String],
    ) -> Result<ConstraintExpr, String> {
        Ok(disjunction(
            states
                .iter()
                .map(|state| self.slot_matches(trace, SlotKind::State, index, state))
                .collect::<Result<Vec<_>, _>>()?,
        ))
    }

    fn transition_at(&self, trace: &str, step: usize) -> Result<ConstraintExpr, String> {
        let branches = self
            .facts
            .enabled_transitions
            .iter()
            .filter(|transition| transition.step == step as u64)
            .map(|transition| {
                Ok(conjunction(vec![
                    self.slot_matches(trace, SlotKind::State, step, &transition.from)?,
                    self.slot_matches(trace, SlotKind::Event, step, &transition.event)?,
                    self.slot_matches(trace, SlotKind::State, step + 1, &transition.to)?,
                ]))
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(disjunction(branches))
    }

    fn slot_match_node_count(
        &self,
        trace: &str,
        kind: SlotKind,
        index: usize,
    ) -> Result<usize, String> {
        let trace_facts = self.require_trace(trace)?;
        let fixed = match kind {
            SlotKind::State => trace_facts.states[index].is_some(),
            SlotKind::Event => trace_facts.events[index].is_some(),
        };
        Ok(if fixed { 1 } else { 3 })
    }

    fn state_membership_node_count(
        &self,
        trace: &str,
        index: usize,
        state_count: usize,
    ) -> Result<usize, String> {
        let slot_nodes = self.slot_match_node_count(trace, SlotKind::State, index)?;
        let children = slot_nodes
            .checked_mul(state_count)
            .ok_or_else(expression_size_overflow)?;
        grouped_expression_node_count(children, state_count)
    }

    fn transition_node_count(&self, trace: &str, step: usize) -> Result<usize, String> {
        let mut branch_count = 0usize;
        let mut branch_nodes = 0usize;
        for _transition in self
            .facts
            .enabled_transitions
            .iter()
            .filter(|transition| transition.step == step as u64)
        {
            let from_nodes = self.slot_match_node_count(trace, SlotKind::State, step)?;
            let event_nodes = self.slot_match_node_count(trace, SlotKind::Event, step)?;
            let to_nodes = self.slot_match_node_count(trace, SlotKind::State, step + 1)?;
            let nodes = from_nodes
                .checked_add(event_nodes)
                .and_then(|nodes| nodes.checked_add(to_nodes))
                .and_then(|nodes| nodes.checked_add(1))
                .ok_or_else(expression_size_overflow)?;
            branch_nodes = branch_nodes
                .checked_add(nodes)
                .ok_or_else(expression_size_overflow)?;
            branch_count = branch_count
                .checked_add(1)
                .ok_or_else(expression_size_overflow)?;
        }
        grouped_expression_node_count(branch_nodes, branch_count)
    }
}

fn validate_manifest_binding(
    binding: &WorkflowRuleBinding,
) -> Result<ValidatedWorkflowBinding<'_>, String> {
    let parameters =
        serde_json::to_value(&binding.parameters).map_err(|error| error.to_string())?;
    let Value::Object(parameters) = parameters else {
        return Err("workflow parameters did not serialize as an object".to_owned());
    };
    let validated = validate_binding_contract(&binding.rule_id, &binding.subjects, &parameters)?;
    let bound = validated
        .parameters
        .get("bound")
        .and_then(Value::as_u64)
        .map(|bound| {
            usize::try_from(bound)
                .map_err(|_| "workflow reachability bound conversion overflow".to_owned())
        })
        .transpose()?;
    Ok(ValidatedWorkflowBinding {
        source: binding,
        handler: validated.handler,
        bound,
    })
}

fn validate_facts(
    facts: &WorkflowFacts,
    unknowns: &[WorkflowUnknown],
    mode: RuleSolveMode,
) -> Result<usize, String> {
    let horizon = usize::try_from(facts.horizon)
        .map_err(|_| "workflow horizon conversion overflow".to_owned())?;
    let state_slots = horizon
        .checked_add(1)
        .ok_or_else(|| "workflow horizon state-slot arithmetic overflow".to_owned())?;
    if facts.horizon > MAX_WORKFLOW_HORIZON {
        return Err(format!(
            "workflow horizon must be in 0..={MAX_WORKFLOW_HORIZON}, found {}",
            facts.horizon
        ));
    }

    let state_domain = validate_domain(&facts.state_domain, "state_domain")?;
    let event_domain = validate_domain(&facts.event_domain, "event_domain")?;
    validate_subset("initial_states", &facts.initial_states, &state_domain)?;
    validate_subset("safe_states", &facts.safe_states, &state_domain)?;
    validate_subset("target_states", &facts.target_states, &state_domain)?;

    if facts.enabled_transitions.len() > MAX_CONSTRAINTS {
        return Err(format!(
            "workflow enabled_transitions maximum is {MAX_CONSTRAINTS}"
        ));
    }
    let mut transition_keys = BTreeSet::new();
    let mut covered_steps = BTreeSet::new();
    for transition in &facts.enabled_transitions {
        let step = usize::try_from(transition.step)
            .map_err(|_| "workflow transition step conversion overflow".to_owned())?;
        if step >= horizon {
            return Err(format!(
                "enabled transition step {step} is out of range for horizon {horizon}"
            ));
        }
        require_label("state", &transition.from, &state_domain)?;
        require_label("event", &transition.event, &event_domain)?;
        require_label("state", &transition.to, &state_domain)?;
        if !transition_keys.insert((
            step,
            transition.from.clone(),
            transition.event.clone(),
            transition.to.clone(),
        )) {
            return Err(format!(
                "duplicate workflow transition at step {step}: {} --{}--> {}",
                transition.from, transition.event, transition.to
            ));
        }
        covered_steps.insert(step);
    }
    for step in 0..horizon {
        if !covered_steps.contains(&step) {
            return Err(format!(
                "enabled_transitions has no relation for workflow step {step}"
            ));
        }
    }

    if facts.traces.is_empty() || facts.traces.len() > MAX_CONSTRAINTS {
        return Err(format!(
            "workflow traces must contain 1..={MAX_CONSTRAINTS} entries"
        ));
    }
    let slots_per_trace = state_slots
        .checked_add(horizon)
        .ok_or_else(|| "workflow per-trace slot arithmetic overflow".to_owned())?;
    let total_slots = facts
        .traces
        .len()
        .checked_mul(slots_per_trace)
        .ok_or_else(|| "workflow total trace-slot arithmetic overflow".to_owned())?;
    if total_slots > MAX_WORKFLOW_TRACE_SLOTS {
        return Err(format!(
            "workflow trace model exceeds checked slot budget {MAX_WORKFLOW_TRACE_SLOTS}"
        ));
    }
    if unknowns.len() > MAX_VARIABLES {
        return Err(format!("workflow unknown maximum is {MAX_VARIABLES}"));
    }

    for (trace_id, trace) in &facts.traces {
        if trace_id.is_empty() {
            return Err("workflow trace IDs must not be empty".to_owned());
        }
        if trace.states.len() != state_slots || trace.events.len() != horizon {
            return Err(format!(
                "traces.{trace_id} requires exactly {state_slots} states and {horizon} events"
            ));
        }
        for (index, state) in trace.states.iter().enumerate() {
            if let Some(state) = state {
                require_label("state", state, &state_domain).map_err(|_| {
                    format!(
                        "traces.{trace_id}.states[{index}] references undeclared state `{state}`"
                    )
                })?;
            }
        }
        for (index, event) in trace.events.iter().enumerate() {
            if let Some(event) = event {
                require_label("event", event, &event_domain).map_err(|_| {
                    format!(
                        "traces.{trace_id}.events[{index}] references undeclared event `{event}`"
                    )
                })?;
            }
        }
    }

    let mut unknown_keys = BTreeSet::new();
    for unknown in unknowns {
        let trace = facts
            .traces
            .get(unknown.trace())
            .ok_or_else(|| format!("unknown workflow trace `{}`", unknown.trace()))?;
        let index = usize::try_from(unknown.index())
            .map_err(|_| "workflow unknown index conversion overflow".to_owned())?;
        let fixed = match unknown.kind() {
            SlotKind::State => trace.states.get(index),
            SlotKind::Event => trace.events.get(index),
        }
        .ok_or_else(|| {
            let slot = match unknown.kind() {
                SlotKind::State => "state",
                SlotKind::Event => "event",
            };
            format!(
                "workflow {slot} unknown index {index} is out of range for trace `{}`",
                unknown.trace()
            )
        })?;
        if fixed.is_some() {
            let field = match unknown.kind() {
                SlotKind::State => "states",
                SlotKind::Event => "events",
            };
            return Err(format!(
                "traces.{}.{field}[{index}] is already fixed",
                unknown.trace()
            ));
        }
        let key = (unknown.trace().to_owned(), unknown.kind(), index);
        if !unknown_keys.insert(key) {
            return Err(format!(
                "duplicate workflow unknown for trace `{}` index {index}",
                unknown.trace()
            ));
        }
    }

    for (trace_id, trace) in &facts.traces {
        for (index, state) in trace.states.iter().enumerate() {
            if state.is_none()
                && !unknown_keys.contains(&(trace_id.clone(), SlotKind::State, index))
            {
                if mode == RuleSolveMode::Verify {
                    return Err(format!(
                        "verification requires complete workflow traces: traces.{trace_id}.states[{index}] is null"
                    ));
                }
                return Err(format!(
                    "traces.{trace_id}.states[{index}] is null without a declared unknown"
                ));
            }
        }
        for (index, event) in trace.events.iter().enumerate() {
            if event.is_none()
                && !unknown_keys.contains(&(trace_id.clone(), SlotKind::Event, index))
            {
                if mode == RuleSolveMode::Verify {
                    return Err(format!(
                        "verification requires complete workflow traces: traces.{trace_id}.events[{index}] is null"
                    ));
                }
                return Err(format!(
                    "traces.{trace_id}.events[{index}] is null without a declared unknown"
                ));
            }
        }
    }
    Ok(horizon)
}

fn validate_domain(labels: &[String], name: &str) -> Result<BTreeSet<String>, String> {
    if labels.is_empty() || labels.len() > MAX_CONSTRAINTS {
        return Err(format!(
            "workflow {name} must contain 1..={MAX_CONSTRAINTS} labels"
        ));
    }
    let mut unique = BTreeSet::new();
    for label in labels {
        if label.is_empty() {
            return Err(format!("workflow {name} labels must not be empty"));
        }
        if !unique.insert(label.clone()) {
            return Err(format!(
                "workflow {name} contains duplicate label `{label}`"
            ));
        }
    }
    Ok(unique)
}

fn validate_subset(name: &str, labels: &[String], domain: &BTreeSet<String>) -> Result<(), String> {
    if labels.is_empty() || labels.len() > MAX_CONSTRAINTS {
        return Err(format!(
            "workflow {name} must contain 1..={MAX_CONSTRAINTS} labels"
        ));
    }
    let mut unique = BTreeSet::new();
    for label in labels {
        require_label("state", label, domain)?;
        if !unique.insert(label) {
            return Err(format!(
                "workflow {name} contains duplicate label `{label}`"
            ));
        }
    }
    Ok(())
}

fn require_label(kind: &str, label: &str, domain: &BTreeSet<String>) -> Result<(), String> {
    if domain.contains(label) {
        Ok(())
    } else {
        Err(format!("undeclared {kind} `{label}`"))
    }
}

fn validate_expression_budget(
    bindings: &[ValidatedWorkflowBinding<'_>],
    resolver: &WorkflowResolver,
) -> Result<(), String> {
    let mut total = 0usize;
    for binding in bindings {
        let nodes = binding_expression_node_count(binding, resolver)?;
        total = total
            .checked_add(nodes)
            .ok_or_else(expression_size_overflow)?;
        if total > MAX_WORKFLOW_EXPRESSION_NODES {
            return Err(format!(
                "workflow expressions exceed the checked node budget {MAX_WORKFLOW_EXPRESSION_NODES}"
            ));
        }
    }
    Ok(())
}

fn binding_expression_node_count(
    binding: &ValidatedWorkflowBinding<'_>,
    resolver: &WorkflowResolver,
) -> Result<usize, String> {
    let trace_id = &binding.source.subjects[0];
    resolver.require_trace(trace_id)?;
    match binding.handler {
        NativeHandlerV1::WorkflowInitialStateAllowed => {
            resolver.state_membership_node_count(trace_id, 0, resolver.facts.initial_states.len())
        }
        NativeHandlerV1::WorkflowTransitionAllowed => {
            let mut nodes = 0usize;
            for step in 0..resolver.horizon {
                nodes = nodes
                    .checked_add(resolver.transition_node_count(trace_id, step)?)
                    .ok_or_else(expression_size_overflow)?;
            }
            grouped_expression_node_count(nodes, resolver.horizon)
        }
        NativeHandlerV1::WorkflowSafetyInvariant => {
            let state_count = resolver.facts.safe_states.len();
            let predicate_count = resolver
                .horizon
                .checked_add(1)
                .ok_or_else(expression_size_overflow)?;
            let mut nodes = 0usize;
            for index in 0..predicate_count {
                nodes = nodes
                    .checked_add(resolver.state_membership_node_count(
                        trace_id,
                        index,
                        state_count,
                    )?)
                    .ok_or_else(expression_size_overflow)?;
            }
            grouped_expression_node_count(nodes, predicate_count)
        }
        NativeHandlerV1::WorkflowBoundedReachability => {
            let bound = effective_reachability_bound(binding, resolver.horizon)?;
            let predicate_count = bound.checked_add(1).ok_or_else(expression_size_overflow)?;
            let state_count = resolver.facts.target_states.len();
            let mut nodes = 0usize;
            for index in 0..predicate_count {
                nodes = nodes
                    .checked_add(resolver.state_membership_node_count(
                        trace_id,
                        index,
                        state_count,
                    )?)
                    .ok_or_else(expression_size_overflow)?;
            }
            grouped_expression_node_count(nodes, predicate_count)
        }
        _ => Err(format!(
            "unsupported workflow rule `{}`",
            binding.source.rule_id
        )),
    }
}

fn grouped_expression_node_count(children: usize, child_count: usize) -> Result<usize, String> {
    match child_count {
        0 => Ok(1),
        1 => Ok(children),
        _ => children.checked_add(1).ok_or_else(expression_size_overflow),
    }
}

fn expression_size_overflow() -> String {
    "workflow expression size overflowed".to_owned()
}

fn effective_reachability_bound(
    binding: &ValidatedWorkflowBinding<'_>,
    horizon: usize,
) -> Result<usize, String> {
    let bound = binding.bound.unwrap_or(horizon);
    if bound > horizon {
        return Err(format!(
            "workflow.bounded_reachability bound {bound} exceeds horizon {horizon}"
        ));
    }
    Ok(bound)
}

fn compile_binding(
    binding: &ValidatedWorkflowBinding<'_>,
    resolver: &WorkflowResolver,
) -> Result<ConstraintExpr, String> {
    let trace_id = &binding.source.subjects[0];
    resolver.require_trace(trace_id)?;
    match binding.handler {
        NativeHandlerV1::WorkflowInitialStateAllowed => {
            resolver.state_in(trace_id, 0, &resolver.facts.initial_states)
        }
        NativeHandlerV1::WorkflowTransitionAllowed => Ok(conjunction(
            (0..resolver.horizon)
                .map(|step| resolver.transition_at(trace_id, step))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        NativeHandlerV1::WorkflowSafetyInvariant => Ok(conjunction(
            (0..=resolver.horizon)
                .map(|index| resolver.state_in(trace_id, index, &resolver.facts.safe_states))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        NativeHandlerV1::WorkflowBoundedReachability => {
            let bound = effective_reachability_bound(binding, resolver.horizon)?;
            Ok(disjunction(
                (0..=bound)
                    .map(|index| resolver.state_in(trace_id, index, &resolver.facts.target_states))
                    .collect::<Result<Vec<_>, _>>()?,
            ))
        }
        _ => Err(format!(
            "unsupported workflow rule `{}`",
            binding.source.rule_id
        )),
    }
}

fn conjunction(predicates: Vec<ConstraintExpr>) -> ConstraintExpr {
    match predicates.len() {
        0 => boolean(true),
        1 => predicates.into_iter().next().expect("one predicate"),
        _ => and(predicates),
    }
}

fn disjunction(predicates: Vec<ConstraintExpr>) -> ConstraintExpr {
    match predicates.len() {
        0 => boolean(false),
        1 => predicates.into_iter().next().expect("one predicate"),
        _ => or(predicates),
    }
}

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn input_schema() -> Value {
    let rule_ids = manifest_family_executable_rule_ids("workflow").unwrap_or(&[]);
    let max_state_slots = MAX_CONSTRAINTS + 1;
    json!({
        "type": "object",
        "properties": {
            "family": {"const": "workflow"},
            "mode": {"type": "string", "enum": ["verify", "synthesize"]},
            "rules": {
                "type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS,
                "items": {
                    "type": "object",
                    "properties": {
                        "rule_id": {"type": "string", "enum": rule_ids},
                        "subjects": {"type": "array", "minItems": 1, "maxItems": 1, "items": {"type": "string"}},
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "bound": {"type": "integer", "minimum": 0, "maximum": MAX_WORKFLOW_HORIZON}
                            },
                            "additionalProperties": false
                        }
                    },
                    "required": ["rule_id", "subjects"],
                    "additionalProperties": false
                }
            },
            "facts": {
                "type": "object",
                "properties": {
                    "horizon": {"type": "integer", "minimum": 0, "maximum": MAX_WORKFLOW_HORIZON},
                    "state_domain": {"type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS, "items": {"type": "string"}},
                    "event_domain": {"type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS, "items": {"type": "string"}},
                    "initial_states": {"type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS, "items": {"type": "string"}},
                    "safe_states": {"type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS, "items": {"type": "string"}},
                    "target_states": {"type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS, "items": {"type": "string"}},
                    "enabled_transitions": {
                        "type": "array", "maxItems": MAX_CONSTRAINTS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "step": {"type": "integer", "minimum": 0},
                                "from": {"type": "string"},
                                "event": {"type": "string"},
                                "to": {"type": "string"}
                            },
                            "required": ["step", "from", "event", "to"],
                            "additionalProperties": false
                        }
                    },
                    "traces": {
                        "type": "object", "minProperties": 1, "maxProperties": MAX_CONSTRAINTS,
                        "additionalProperties": {
                            "type": "object",
                            "properties": {
                                "states": {
                                    "type": "array", "maxItems": max_state_slots,
                                    "items": {"type": ["string", "null"]}
                                },
                                "events": {
                                    "type": "array", "maxItems": MAX_CONSTRAINTS,
                                    "items": {"type": ["string", "null"]}
                                }
                            },
                            "required": ["states", "events"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": [
                    "horizon", "state_domain", "event_domain", "initial_states",
                    "safe_states", "target_states", "enabled_transitions", "traces"
                ],
                "additionalProperties": false
            },
            "unknowns": {
                "type": "array", "maxItems": MAX_VARIABLES, "default": [],
                "items": {
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": {
                                "kind": {"const": "trace_state"},
                                "trace": {"type": "string"},
                                "index": {"type": "integer", "minimum": 0}
                            },
                            "required": ["kind", "trace", "index"],
                            "additionalProperties": false
                        },
                        {
                            "type": "object",
                            "properties": {
                                "kind": {"const": "trace_event"},
                                "trace": {"type": "string"},
                                "index": {"type": "integer", "minimum": 0}
                            },
                            "required": ["kind", "trace", "index"],
                            "additionalProperties": false
                        }
                    ]
                }
            },
            "timeout_ms": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_MS, "default": DEFAULT_TIMEOUT_MS},
            "persist": {"type": "boolean", "default": false},
            "include_smt": {"type": "boolean", "default": false}
        },
        "required": ["family", "mode", "rules", "facts"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::COMPILER;
    use crate::rules::compiler::{
        BoundedReachabilityScope, RuleEvaluationScope, RuleFamilyCompiler,
    };
    use crate::{
        service::SolverService,
        types::{ModelValue, SolveStatus, MAX_VARIABLES},
    };

    fn rule(rule_id: &str) -> Value {
        json!({"rule_id": rule_id, "subjects": ["approval"], "parameters": {}})
    }

    fn approval_facts(horizon: u64, states: Vec<Value>, events: Vec<Value>) -> Value {
        let enabled_transitions = match horizon {
            0 => Vec::new(),
            1 => vec![json!({
                "step": 0, "from": "Draft", "event": "submit", "to": "Review"
            })],
            2 => vec![
                json!({"step": 0, "from": "Draft", "event": "submit", "to": "Review"}),
                json!({"step": 1, "from": "Review", "event": "approve", "to": "Approved"}),
                json!({"step": 1, "from": "Review", "event": "reject", "to": "Rejected"}),
            ],
            _ => (0..horizon)
                .map(
                    |step| json!({"step": step, "from": "Draft", "event": "submit", "to": "Draft"}),
                )
                .collect(),
        };
        json!({
            "horizon": horizon,
            "state_domain": ["Draft", "Review", "Approved", "Rejected"],
            "event_domain": ["submit", "approve", "reject"],
            "initial_states": ["Draft"],
            "safe_states": ["Draft", "Review", "Approved"],
            "target_states": ["Rejected"],
            "enabled_transitions": enabled_transitions,
            "traces": {
                "approval": {"states": states, "events": events}
            }
        })
    }

    fn request(mode: &str, rules: Vec<Value>, facts: Value, unknowns: Vec<Value>) -> Value {
        json!({
            "family": "workflow",
            "mode": mode,
            "rules": rules,
            "facts": facts,
            "unknowns": unknowns
        })
    }

    fn unsafe_witness_request(bound: u64) -> Value {
        let mut reachability = rule("workflow.bounded_reachability");
        reachability["parameters"] = json!({"bound": bound});
        request(
            "synthesize",
            vec![
                rule("workflow.initial_state_allowed"),
                rule("workflow.transition_allowed"),
                reachability,
            ],
            approval_facts(
                2,
                vec![json!("Draft"), Value::Null, Value::Null],
                vec![Value::Null, Value::Null],
            ),
            vec![
                json!({"kind": "trace_state", "trace": "approval", "index": 1}),
                json!({"kind": "trace_state", "trace": "approval", "index": 2}),
                json!({"kind": "trace_event", "trace": "approval", "index": 0}),
                json!({"kind": "trace_event", "trace": "approval", "index": 1}),
            ],
        )
    }

    async fn solve(
        input: Value,
    ) -> (
        crate::rules::compiler::FamilyCompilation,
        crate::types::SolveConstraintsResponse,
    ) {
        let compiled = COMPILER
            .compile(input)
            .expect("workflow request must compile");
        let response = SolverService::new()
            .solve_constraints(compiled.request.clone())
            .await
            .expect("workflow request must solve");
        (compiled, response)
    }

    #[test]
    fn workflow_compiler_is_registered_after_scheduling() {
        let compiler_ids = crate::rules::families::compilers()
            .iter()
            .map(|compiler| compiler.id())
            .collect::<Vec<_>>();

        assert_eq!(
            compiler_ids,
            [
                "accessibility",
                "configuration",
                "design",
                "policy",
                "resource",
                "scheduling",
                "workflow",
            ]
        );
        assert_eq!(
            crate::rules::families::compiler("workflow").map(RuleFamilyCompiler::id),
            Some("workflow")
        );
    }

    #[tokio::test]
    async fn workflow_initial_state_rejection_is_unsatisfiable() {
        let input = request(
            "verify",
            vec![rule("workflow.initial_state_allowed")],
            approval_facts(0, vec![json!("Review")], vec![]),
            vec![],
        );
        let (_, response) = solve(input).await;
        assert_eq!(response.status, SolveStatus::Unsat);
    }

    #[tokio::test]
    async fn workflow_earliest_illegal_transition_is_unsatisfiable() {
        let mut facts = approval_facts(
            2,
            vec![json!("Draft"), json!("Approved"), json!("Approved")],
            vec![json!("approve"), json!("approve")],
        );
        facts["enabled_transitions"] = json!([
            {"step": 0, "from": "Draft", "event": "submit", "to": "Review"},
            {"step": 1, "from": "Approved", "event": "approve", "to": "Approved"}
        ]);
        let input = request(
            "verify",
            vec![rule("workflow.transition_allowed")],
            facts,
            vec![],
        );
        let (_, response) = solve(input).await;
        assert_eq!(response.status, SolveStatus::Unsat);
    }

    #[tokio::test]
    async fn workflow_safety_rejection_is_unsatisfiable() {
        let input = request(
            "verify",
            vec![rule("workflow.safety_invariant")],
            approval_facts(
                2,
                vec![json!("Draft"), json!("Review"), json!("Rejected")],
                vec![json!("submit"), json!("reject")],
            ),
            vec![],
        );
        let (_, response) = solve(input).await;
        assert_eq!(response.status, SolveStatus::Unsat);
    }

    #[tokio::test]
    async fn workflow_reachable_unsafe_target_returns_bounded_witness_and_projection() {
        let (compiled, response) = solve(unsafe_witness_request(2)).await;
        assert_eq!(response.status, SolveStatus::Sat);
        assert_eq!(compiled.projections.len(), 4);

        let assignments = COMPILER.project_model(
            &compiled.projections,
            response.model.as_ref().expect("bounded witness model"),
        );
        assert!(assignments.iter().any(|assignment| {
            assignment.field == "traces.approval.states[1]"
                && assignment.value == ModelValue::Enum("Review".to_owned())
        }));
        assert!(assignments.iter().any(|assignment| {
            assignment.field == "traces.approval.states[2]"
                && assignment.value == ModelValue::Enum("Rejected".to_owned())
        }));
        assert!(assignments.iter().any(|assignment| {
            assignment.field == "traces.approval.events[0]"
                && assignment.value == ModelValue::Enum("submit".to_owned())
        }));
        assert!(assignments.iter().any(|assignment| {
            assignment.field == "traces.approval.events[1]"
                && assignment.value == ModelValue::Enum("reject".to_owned())
        }));
    }

    #[tokio::test]
    async fn workflow_bound_override_makes_later_target_unsatisfiable() {
        let (_, response) = solve(unsafe_witness_request(1)).await;
        assert_eq!(response.status, SolveStatus::Unsat);
    }

    #[test]
    fn workflow_scope_preserves_reachability_binding_order_and_horizon_default() {
        let mut input = unsafe_witness_request(1);
        input["rules"]
            .as_array_mut()
            .expect("workflow rules")
            .push(rule("workflow.bounded_reachability"));

        let (_, scope) = COMPILER
            .compile_with_evaluation_scope(input)
            .expect("workflow request must compile with scope");

        assert_eq!(
            scope,
            Some(RuleEvaluationScope::BoundedTrace {
                horizon: 2,
                reachability: vec![
                    BoundedReachabilityScope {
                        rule_id: "workflow.bounded_reachability".to_owned(),
                        binding_index: 2,
                        effective_bound: 1,
                    },
                    BoundedReachabilityScope {
                        rule_id: "workflow.bounded_reachability".to_owned(),
                        binding_index: 3,
                        effective_bound: 2,
                    },
                ],
            })
        );
    }

    #[test]
    fn workflow_projection_paths_are_trace_indexed_in_unknown_order() {
        let compiled = COMPILER
            .compile(unsafe_witness_request(2))
            .expect("bounded witness must compile");
        assert_eq!(
            compiled
                .projections
                .iter()
                .map(|projection| projection.field.as_str())
                .collect::<Vec<_>>(),
            [
                "traces.approval.states[1]",
                "traces.approval.states[2]",
                "traces.approval.events[0]",
                "traces.approval.events[1]",
            ]
        );
    }

    #[test]
    fn workflow_verification_requires_complete_traces_and_no_unknowns() {
        let incomplete = request(
            "verify",
            vec![rule("workflow.safety_invariant")],
            approval_facts(1, vec![json!("Draft"), Value::Null], vec![json!("submit")]),
            vec![],
        );
        assert!(COMPILER
            .compile(incomplete)
            .expect_err("verification trace must be complete")
            .message
            .contains("verification requires complete workflow traces"));

        let with_unknown = request(
            "verify",
            vec![rule("workflow.transition_allowed")],
            approval_facts(1, vec![json!("Draft"), Value::Null], vec![json!("submit")]),
            vec![json!({"kind": "trace_state", "trace": "approval", "index": 1})],
        );
        assert!(COMPILER
            .compile(with_unknown)
            .expect_err("verification unknowns must be rejected")
            .message
            .contains("verification requires complete workflow traces"));
    }

    #[test]
    fn workflow_synthesis_rejects_missing_duplicate_fixed_and_out_of_range_unknowns() {
        let missing = request(
            "synthesize",
            vec![rule("workflow.transition_allowed")],
            approval_facts(1, vec![json!("Draft"), Value::Null], vec![json!("submit")]),
            vec![],
        );
        assert!(COMPILER
            .compile(missing)
            .expect_err("null slot needs an unknown")
            .message
            .contains("without a declared unknown"));

        let fixed = request(
            "synthesize",
            vec![rule("workflow.transition_allowed")],
            approval_facts(
                1,
                vec![json!("Draft"), json!("Review")],
                vec![json!("submit")],
            ),
            vec![json!({"kind": "trace_state", "trace": "approval", "index": 1})],
        );
        assert!(COMPILER
            .compile(fixed)
            .expect_err("fixed slot cannot be unknown")
            .message
            .contains("already fixed"));

        let duplicate_unknown = json!({
            "kind": "trace_state", "trace": "approval", "index": 1
        });
        let duplicate = request(
            "synthesize",
            vec![rule("workflow.transition_allowed")],
            approval_facts(1, vec![json!("Draft"), Value::Null], vec![json!("submit")]),
            vec![duplicate_unknown.clone(), duplicate_unknown],
        );
        assert!(COMPILER
            .compile(duplicate)
            .expect_err("duplicate unknown must be rejected")
            .message
            .contains("duplicate workflow unknown"));

        let out_of_range = request(
            "synthesize",
            vec![rule("workflow.transition_allowed")],
            approval_facts(
                1,
                vec![json!("Draft"), json!("Review")],
                vec![json!("submit")],
            ),
            vec![json!({"kind": "trace_event", "trace": "approval", "index": 1})],
        );
        assert!(COMPILER
            .compile(out_of_range)
            .expect_err("event index equals horizon")
            .message
            .contains("out of range"));
    }

    #[test]
    fn workflow_rejects_undeclared_labels_model_size_and_arithmetic_overflow() {
        let undeclared = request(
            "verify",
            vec![rule("workflow.safety_invariant")],
            approval_facts(0, vec![json!("Ghost")], vec![]),
            vec![],
        );
        assert!(COMPILER
            .compile(undeclared)
            .expect_err("undeclared state label")
            .message
            .contains("undeclared state `Ghost`"));

        let horizon = MAX_VARIABLES as u64;
        let oversized = request(
            "synthesize",
            vec![rule("workflow.safety_invariant")],
            approval_facts(
                horizon,
                vec![Value::Null; MAX_VARIABLES + 1],
                vec![json!("submit"); MAX_VARIABLES],
            ),
            (0..=MAX_VARIABLES)
                .map(|index| json!({"kind": "trace_state", "trace": "approval", "index": index}))
                .collect(),
        );
        assert!(COMPILER
            .compile(oversized)
            .expect_err("too many workflow unknowns")
            .message
            .contains(&format!("unknown maximum is {MAX_VARIABLES}")));

        let mut overflow_facts = approval_facts(0, vec![json!("Draft")], vec![]);
        overflow_facts["horizon"] = json!(u64::MAX);
        let overflow = request(
            "verify",
            vec![rule("workflow.initial_state_allowed")],
            overflow_facts,
            vec![],
        );
        assert!(COMPILER
            .compile(overflow)
            .expect_err("horizon arithmetic must be checked")
            .message
            .contains("overflow"));
    }

    #[test]
    fn workflow_checked_aggregate_expression_budget_rejects_repeated_expansion() {
        let horizon = MAX_VARIABLES as u64;
        let state_domain = (0..MAX_VARIABLES)
            .map(|index| format!("s{index}"))
            .collect::<Vec<_>>();
        let enabled_transitions = (0..horizon)
            .map(|step| json!({"step": step, "from": "s0", "event": "tick", "to": "s0"}))
            .collect::<Vec<_>>();
        let facts = json!({
            "horizon": horizon,
            "state_domain": state_domain,
            "event_domain": ["tick"],
            "initial_states": ["s0"],
            "safe_states": state_domain,
            "target_states": ["s0"],
            "enabled_transitions": enabled_transitions,
            "traces": {
                "approval": {
                    "states": vec!["s0"; MAX_VARIABLES + 1],
                    "events": vec!["tick"; MAX_VARIABLES]
                }
            }
        });
        let input = request(
            "verify",
            vec![
                rule("workflow.safety_invariant"),
                rule("workflow.safety_invariant"),
                rule("workflow.safety_invariant"),
                rule("workflow.safety_invariant"),
            ],
            facts,
            vec![],
        );

        assert!(COMPILER
            .compile(input)
            .expect_err("aggregate workflow AST must be bounded before lowering")
            .message
            .contains("checked node budget"));
    }
}

//! Finite-horizon scheduling validation and typed constraint lowering.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    rules::{
        compiler::{
            FamilyCompilation, FamilyCompileError, ModelProjection, RuleAssignment,
            RuleFamilyCompiler,
        },
        manifest::validate_binding_contract,
        manifest_family_executable_rule_ids,
        manifest_format::NativeHandlerV1,
        primitives::{and, boolean, eq, ge, int, le, mul, request, var},
        CompiledRule, RuleSolveMode,
    },
    types::{
        ConstraintExpr, ModelValue, Objective, ObjectiveOp, SolveModel, Variable,
        DEFAULT_TIMEOUT_MS, MAX_CONSTRAINTS, MAX_TIMEOUT_MS, MAX_VARIABLES,
    },
};

const MAX_SCHEDULING_HORIZON: i64 = MAX_CONSTRAINTS as i64;
const MAX_SCHEDULING_CAPACITY_EXPRESSION_NODES: usize = MAX_CONSTRAINTS * MAX_VARIABLES;

/// Scheduling compiler registered behind `solve_rules`.
pub static COMPILER: SchedulingCompiler = SchedulingCompiler;

/// Stateless finite-horizon scheduling compiler.
pub struct SchedulingCompiler;

impl RuleFamilyCompiler for SchedulingCompiler {
    fn id(&self) -> &'static str {
        "scheduling"
    }

    fn input_schema(&self) -> Value {
        input_schema()
    }

    fn compile(&self, input: Value) -> Result<FamilyCompilation, FamilyCompileError> {
        compile(input).map_err(|message| FamilyCompileError::new(self.id(), message))
    }

    fn project_model(
        &self,
        projections: &[ModelProjection],
        model: &SolveModel,
    ) -> Vec<RuleAssignment> {
        let mut selected = BTreeMap::new();
        let mut makespan = None;
        for projection in projections {
            if projection.field == "makespan" {
                makespan = model.get(&projection.variable).cloned();
                continue;
            }
            let Ok(candidate) = serde_json::from_str::<AssignmentProjection>(&projection.field)
            else {
                continue;
            };
            if model.get(&projection.variable) == Some(&ModelValue::Int(1)) {
                selected.insert(
                    projection.subject.clone(),
                    (candidate.machine, candidate.start),
                );
            }
        }

        let mut assignments = Vec::with_capacity(selected.len().saturating_mul(2) + 1);
        for (job, (machine, start)) in selected {
            assignments.push(RuleAssignment {
                node: job.clone(),
                field: "assignment.machine".to_owned(),
                value: ModelValue::Enum(machine),
            });
            assignments.push(RuleAssignment {
                node: job,
                field: "assignment.start".to_owned(),
                value: ModelValue::Int(start),
            });
        }
        if let Some(value) = makespan {
            assignments.push(RuleAssignment {
                node: "schedule".to_owned(),
                field: "makespan".to_owned(),
                value,
            });
        }
        assignments
    }
}

fn compile(input: Value) -> Result<FamilyCompilation, String> {
    let input: SchedulingRequest =
        serde_json::from_value(input).map_err(|error| error.to_string())?;
    if input.family != "scheduling" {
        return Err(format!(
            "scheduling compiler requires family `scheduling`, got `{}`",
            input.family
        ));
    }
    if input.rules.is_empty() {
        return Err("at least one scheduling rule binding is required".to_owned());
    }
    if input.rules.len() > MAX_CONSTRAINTS {
        return Err(format!(
            "request has too many scheduling rule bindings; maximum is {MAX_CONSTRAINTS}"
        ));
    }
    if !(1..=MAX_TIMEOUT_MS).contains(&input.timeout_ms) {
        return Err(format!(
            "timeout_ms must be in 1..={MAX_TIMEOUT_MS}, found {}",
            input.timeout_ms
        ));
    }
    if input.mode == RuleSolveMode::Verify && !input.unknowns.is_empty() {
        return Err("verification requires complete scheduling facts".to_owned());
    }

    let bindings = input
        .rules
        .iter()
        .map(validate_manifest_binding)
        .collect::<Result<Vec<_>, _>>()?;
    let makespan_bindings = bindings
        .iter()
        .filter(|binding| binding.handler == NativeHandlerV1::SchedulingMinimizeMakespan)
        .count();
    if makespan_bindings > 1 {
        return Err("at most one scheduling.minimize_makespan binding is allowed".to_owned());
    }
    validate_facts(&input.facts, &input.unknowns)?;
    validate_mode_parameters(input.mode, &bindings)?;

    let resolver = SchedulingResolver::new(
        input.facts,
        &input.unknowns,
        makespan_bindings == 1,
        input.mode == RuleSolveMode::Synthesize,
    )?;
    validate_capacity_expression_budget(&bindings, &resolver)?;
    let rules = bindings
        .iter()
        .enumerate()
        .map(|(index, binding)| {
            compile_binding(binding, &resolver).map(|predicate| {
                CompiledRule::new(binding.source.rule_id.clone(), index, predicate)
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut solver_request = request(
        "scheduling",
        resolver.variables,
        &rules,
        input.timeout_ms,
        input.persist,
        input.include_smt,
    );
    if input.mode == RuleSolveMode::Synthesize && makespan_bindings == 1 {
        solver_request.objectives.push(Objective {
            op: ObjectiveOp::Minimize,
            expr: var(resolver
                .makespan_variable
                .as_ref()
                .expect("makespan binding creates a variable")),
        });
    }
    solver_request
        .validate()
        .map_err(|error| format!("compiled scheduling rules are invalid: {error}"))?;

    Ok(FamilyCompilation {
        mode: input.mode,
        request: solver_request,
        rules,
        projections: resolver.projections,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SchedulingRequest {
    family: String,
    mode: RuleSolveMode,
    rules: Vec<SchedulingRuleBinding>,
    facts: SchedulingFacts,
    #[serde(default)]
    unknowns: Vec<SchedulingUnknown>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    persist: bool,
    #[serde(default)]
    include_smt: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SchedulingRuleBinding {
    rule_id: String,
    subjects: Vec<String>,
    #[serde(default)]
    parameters: SchedulingParameters,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SchedulingParameters {
    #[serde(skip_serializing_if = "Option::is_none")]
    maximum_makespan: Option<i64>,
}

struct ValidatedSchedulingBinding<'a> {
    source: &'a SchedulingRuleBinding,
    handler: NativeHandlerV1,
    maximum_makespan: Option<i64>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SchedulingFacts {
    horizon: i64,
    jobs: BTreeMap<String, JobFacts>,
    machines: BTreeMap<String, MachineFacts>,
    #[serde(default)]
    precedence: Vec<PrecedenceFacts>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobFacts {
    release: i64,
    deadline: i64,
    durations: BTreeMap<String, i64>,
    eligible_machines: Vec<String>,
    demands: BTreeMap<String, i64>,
    assignment: Option<AssignmentFacts>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssignmentFacts {
    machine: String,
    start: i64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct MachineFacts {
    capacities: BTreeMap<String, i64>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrecedenceFacts {
    before: String,
    after: String,
    minimum_lag: i64,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SchedulingUnknown {
    Assignment { job: String },
}

impl SchedulingUnknown {
    fn job(&self) -> &str {
        match self {
            Self::Assignment { job } => job,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AssignmentProjection {
    machine: String,
    start: i64,
}

#[derive(Clone)]
struct Placement {
    job: String,
    machine: String,
    start: i64,
    duration: i64,
    value: ConstraintExpr,
}

struct SchedulingResolver {
    facts: SchedulingFacts,
    unknown_jobs: BTreeSet<String>,
    placements: Vec<Placement>,
    variables: Vec<Variable>,
    projections: Vec<ModelProjection>,
    makespan_variable: Option<String>,
}

impl SchedulingResolver {
    fn new(
        facts: SchedulingFacts,
        unknowns: &[SchedulingUnknown],
        needs_makespan: bool,
        project_makespan: bool,
    ) -> Result<Self, String> {
        let unknown_jobs = unknowns
            .iter()
            .map(|unknown| unknown.job().to_owned())
            .collect::<BTreeSet<_>>();
        let variable_limit = MAX_VARIABLES.saturating_sub(usize::from(needs_makespan));
        let mut variables = Vec::new();
        let mut projections = Vec::new();
        let mut placements = Vec::new();

        for (job_id, job) in &facts.jobs {
            if unknown_jobs.contains(job_id) {
                for (machine, duration) in &job.durations {
                    let Some(last_start) = facts.horizon.checked_sub(*duration) else {
                        continue;
                    };
                    if last_start < 0 {
                        continue;
                    }
                    for start in 0..=last_start {
                        if variables.len() >= variable_limit {
                            return Err(format!(
                                "scheduling placement variable count exceeds maximum {variable_limit}"
                            ));
                        }
                        let variable = format!("scheduling_x_{}", variables.len());
                        variables.push(Variable::IntRange {
                            name: variable.clone(),
                            min: 0,
                            max: 1,
                        });
                        projections.push(ModelProjection {
                            variable: variable.clone(),
                            subject: job_id.clone(),
                            field: serde_json::to_string(&AssignmentProjection {
                                machine: machine.clone(),
                                start,
                            })
                            .map_err(|error| error.to_string())?,
                        });
                        placements.push(Placement {
                            job: job_id.clone(),
                            machine: machine.clone(),
                            start,
                            duration: *duration,
                            value: var(variable),
                        });
                    }
                }
            } else if let Some(assignment) = &job.assignment {
                let Some(duration) = job.durations.get(&assignment.machine) else {
                    continue;
                };
                if assignment.start < 0
                    || assignment
                        .start
                        .checked_add(*duration)
                        .is_none_or(|completion| completion > facts.horizon)
                {
                    continue;
                }
                placements.push(Placement {
                    job: job_id.clone(),
                    machine: assignment.machine.clone(),
                    start: assignment.start,
                    duration: *duration,
                    value: int(1),
                });
            }
        }

        let makespan_variable = needs_makespan.then(|| "scheduling_cmax".to_owned());
        if let Some(variable) = &makespan_variable {
            variables.push(Variable::IntRange {
                name: variable.clone(),
                min: 0,
                max: facts.horizon,
            });
            if project_makespan {
                projections.push(ModelProjection {
                    variable: variable.clone(),
                    subject: "schedule".to_owned(),
                    field: "makespan".to_owned(),
                });
            }
        }

        Ok(Self {
            facts,
            unknown_jobs,
            placements,
            variables,
            projections,
            makespan_variable,
        })
    }

    fn require_job(&self, job: &str) -> Result<&JobFacts, String> {
        self.facts
            .jobs
            .get(job)
            .ok_or_else(|| format!("unknown scheduling job `{job}`"))
    }

    fn placements_for<'a>(&'a self, job: &'a str) -> impl Iterator<Item = &'a Placement> {
        self.placements
            .iter()
            .filter(move |placement| placement.job == job)
    }

    fn assignment_is_represented(&self, job: &str) -> ConstraintExpr {
        boolean(self.unknown_jobs.contains(job) || self.placements_for(job).next().is_some())
    }

    fn start(&self, job: &str) -> ConstraintExpr {
        sum(self
            .placements_for(job)
            .map(|placement| mul(vec![int(placement.start), placement.value.clone()]))
            .collect())
    }

    fn completion(&self, job: &str) -> ConstraintExpr {
        sum(self
            .placements_for(job)
            .map(|placement| {
                mul(vec![
                    int(placement.start + placement.duration),
                    placement.value.clone(),
                ])
            })
            .collect())
    }
}

fn validate_manifest_binding(
    binding: &SchedulingRuleBinding,
) -> Result<ValidatedSchedulingBinding<'_>, String> {
    let parameters =
        serde_json::to_value(&binding.parameters).map_err(|error| error.to_string())?;
    let Value::Object(parameters) = parameters else {
        return Err("scheduling parameters did not serialize as an object".to_owned());
    };
    let validated = validate_binding_contract(&binding.rule_id, &binding.subjects, &parameters)?;
    Ok(ValidatedSchedulingBinding {
        source: binding,
        handler: validated.handler,
        maximum_makespan: validated
            .parameters
            .get("maximum_makespan")
            .and_then(Value::as_i64),
    })
}

fn validate_mode_parameters(
    mode: RuleSolveMode,
    bindings: &[ValidatedSchedulingBinding<'_>],
) -> Result<(), String> {
    for binding in bindings {
        if binding.handler != NativeHandlerV1::SchedulingMinimizeMakespan {
            continue;
        }
        match (mode, binding.maximum_makespan) {
            (RuleSolveMode::Verify, None) => {
                return Err(
                    "scheduling.minimize_makespan verification requires maximum_makespan"
                        .to_owned(),
                )
            }
            (RuleSolveMode::Synthesize, Some(_)) => {
                return Err(
                    "scheduling.minimize_makespan synthesis does not accept maximum_makespan"
                        .to_owned(),
                )
            }
            (RuleSolveMode::Verify, Some(_)) | (RuleSolveMode::Synthesize, None) => {}
        }
    }
    Ok(())
}

fn validate_capacity_expression_budget(
    bindings: &[ValidatedSchedulingBinding<'_>],
    resolver: &SchedulingResolver,
) -> Result<(), String> {
    let horizon = usize::try_from(resolver.facts.horizon)
        .map_err(|_| "scheduling horizon does not fit the host size".to_owned())?;
    let mut total = 0usize;
    for binding in bindings {
        if binding.handler != NativeHandlerV1::SchedulingCumulativeCapacity {
            continue;
        }
        let machine_id = &binding.source.subjects[0];
        let machine = resolver
            .facts
            .machines
            .get(machine_id)
            .ok_or_else(|| format!("unknown scheduling machine `{machine_id}`"))?;
        let mut resources = machine.capacities.keys().collect::<BTreeSet<_>>();
        for job in resolver.facts.jobs.values() {
            if job.durations.contains_key(machine_id) {
                resources.extend(job.demands.keys());
            }
        }
        let placements = resolver
            .placements
            .iter()
            .filter(|placement| placement.machine == *machine_id)
            .count();
        let cells = resources
            .len()
            .checked_mul(horizon)
            .ok_or_else(|| "scheduling capacity cell count overflowed".to_owned())?;
        let nodes_per_cell = placements
            .checked_mul(4)
            .and_then(|nodes| nodes.checked_add(3))
            .ok_or_else(|| "scheduling capacity expression size overflowed".to_owned())?;
        let binding_nodes = cells
            .checked_mul(nodes_per_cell)
            .ok_or_else(|| "scheduling capacity expression size overflowed".to_owned())?;
        total = total
            .checked_add(binding_nodes)
            .ok_or_else(|| "scheduling capacity expression size overflowed".to_owned())?;
        if total > MAX_SCHEDULING_CAPACITY_EXPRESSION_NODES {
            return Err(format!(
                "scheduling capacity expressions exceed the checked node budget {MAX_SCHEDULING_CAPACITY_EXPRESSION_NODES}"
            ));
        }
    }
    Ok(())
}

fn validate_facts(facts: &SchedulingFacts, unknowns: &[SchedulingUnknown]) -> Result<(), String> {
    if !(1..=MAX_SCHEDULING_HORIZON).contains(&facts.horizon) {
        return Err(format!(
            "scheduling horizon must be in 1..={MAX_SCHEDULING_HORIZON}, found {}",
            facts.horizon
        ));
    }
    if facts.jobs.is_empty() || facts.jobs.len() > MAX_CONSTRAINTS {
        return Err(format!(
            "scheduling jobs must contain 1..={MAX_CONSTRAINTS} entries"
        ));
    }
    if facts.machines.is_empty() || facts.machines.len() > MAX_CONSTRAINTS {
        return Err(format!(
            "scheduling machines must contain 1..={MAX_CONSTRAINTS} entries"
        ));
    }
    if facts.precedence.len() > MAX_CONSTRAINTS {
        return Err(format!(
            "scheduling precedence maximum is {MAX_CONSTRAINTS}"
        ));
    }
    if unknowns.len() > MAX_VARIABLES {
        return Err(format!("scheduling unknown maximum is {MAX_VARIABLES}"));
    }

    for (machine, machine_facts) in &facts.machines {
        if machine.is_empty() {
            return Err("scheduling machine IDs must not be empty".to_owned());
        }
        if machine_facts.capacities.len() > MAX_CONSTRAINTS {
            return Err(format!("machine `{machine}` has too many capacities"));
        }
        for (resource, capacity) in &machine_facts.capacities {
            if resource.is_empty() || *capacity < 0 {
                return Err(format!(
                    "machines.{machine}.capacities must use non-empty resources and nonnegative values"
                ));
            }
        }
    }

    for (job, job_facts) in &facts.jobs {
        if job.is_empty() {
            return Err("scheduling job IDs must not be empty".to_owned());
        }
        if job_facts.release < 0
            || job_facts.release > job_facts.deadline
            || job_facts.deadline > facts.horizon
        {
            return Err(format!(
                "jobs.{job} must have 0 <= release <= deadline <= horizon"
            ));
        }
        if job_facts.durations.is_empty() || job_facts.durations.len() > MAX_CONSTRAINTS {
            return Err(format!(
                "jobs.{job}.durations must contain 1..={MAX_CONSTRAINTS} entries"
            ));
        }
        for (machine, duration) in &job_facts.durations {
            if !facts.machines.contains_key(machine) {
                return Err(format!(
                    "jobs.{job}.durations references unknown machine `{machine}`"
                ));
            }
            if *duration <= 0 {
                return Err(format!("jobs.{job}.durations.{machine} must be positive"));
            }
        }
        reject_duplicates(
            &job_facts.eligible_machines,
            &format!("jobs.{job}.eligible_machines"),
        )?;
        for machine in &job_facts.eligible_machines {
            if !facts.machines.contains_key(machine) {
                return Err(format!(
                    "jobs.{job}.eligible_machines references unknown machine `{machine}`"
                ));
            }
        }
        if job_facts.demands.len() > MAX_CONSTRAINTS {
            return Err(format!("jobs.{job}.demands has too many resources"));
        }
        for (resource, demand) in &job_facts.demands {
            if resource.is_empty() || *demand < 0 {
                return Err(format!(
                    "jobs.{job}.demands must use non-empty resources and nonnegative values"
                ));
            }
        }
    }

    let mut precedence_pairs = BTreeSet::new();
    for edge in &facts.precedence {
        if !facts.jobs.contains_key(&edge.before) || !facts.jobs.contains_key(&edge.after) {
            return Err(format!(
                "precedence `{}` -> `{}` references an unknown job",
                edge.before, edge.after
            ));
        }
        if edge.before == edge.after || edge.minimum_lag < 0 {
            return Err(format!(
                "precedence `{}` -> `{}` requires distinct jobs and a nonnegative lag",
                edge.before, edge.after
            ));
        }
        if !precedence_pairs.insert((edge.before.clone(), edge.after.clone())) {
            return Err(format!(
                "duplicate precedence edge `{}` -> `{}`",
                edge.before, edge.after
            ));
        }
    }

    let mut unknown_jobs = BTreeSet::new();
    for unknown in unknowns {
        let job = unknown.job();
        let job_facts = facts
            .jobs
            .get(job)
            .ok_or_else(|| format!("unknown scheduling job `{job}`"))?;
        if job_facts.assignment.is_some() {
            return Err(format!("jobs.{job}.assignment is already fixed"));
        }
        if !unknown_jobs.insert(job.to_owned()) {
            return Err(format!("duplicate scheduling assignment unknown `{job}`"));
        }
    }

    for (job, job_facts) in &facts.jobs {
        let Some(assignment) = &job_facts.assignment else {
            if !unknown_jobs.contains(job) {
                return Err(format!(
                    "jobs.{job}.assignment requires either a fixed assignment or a declared assignment unknown"
                ));
            }
            continue;
        };

        if !facts.machines.contains_key(&assignment.machine) {
            return Err(format!(
                "jobs.{job}.assignment references unknown machine `{}`",
                assignment.machine
            ));
        }
        let duration = job_facts
            .durations
            .get(&assignment.machine)
            .ok_or_else(|| {
                format!(
                    "jobs.{job}.assignment machine `{}` has no declared duration",
                    assignment.machine
                )
            })?;
        if assignment.start < 0 || assignment.start > facts.horizon {
            return Err(format!(
                "jobs.{job}.assignment start must be within the scheduling horizon"
            ));
        }
        let completion = assignment.start.checked_add(*duration).ok_or_else(|| {
            format!("jobs.{job}.assignment completion overflows the integer domain")
        })?;
        if completion > facts.horizon {
            return Err(format!(
                "jobs.{job}.assignment completion must be within the scheduling horizon"
            ));
        }
    }
    Ok(())
}

fn compile_binding(
    binding: &ValidatedSchedulingBinding<'_>,
    resolver: &SchedulingResolver,
) -> Result<ConstraintExpr, String> {
    let subjects = &binding.source.subjects;
    reject_duplicates(subjects, &format!("{} subjects", binding.source.rule_id))?;
    match binding.handler {
        NativeHandlerV1::SchedulingAssignmentExactlyOnce => {
            let job = &subjects[0];
            resolver.require_job(job)?;
            Ok(eq(
                sum(resolver
                    .placements_for(job)
                    .map(|placement| placement.value.clone())
                    .collect()),
                int(1),
            ))
        }
        NativeHandlerV1::SchedulingPlacementAllowed => {
            let job_id = &subjects[0];
            let job = resolver.require_job(job_id)?;
            let eligible = job.eligible_machines.iter().collect::<BTreeSet<_>>();
            let mut predicates = vec![resolver.assignment_is_represented(job_id)];
            for placement in resolver.placements_for(job_id) {
                let completion = placement
                    .start
                    .checked_add(placement.duration)
                    .unwrap_or(i64::MAX);
                let allowed = eligible.contains(&placement.machine)
                    && placement.start >= job.release
                    && completion <= job.deadline;
                if !allowed {
                    predicates.push(eq(placement.value.clone(), int(0)));
                }
            }
            Ok(conjunction(predicates))
        }
        NativeHandlerV1::SchedulingPrecedenceFinishStart => {
            let before = &subjects[0];
            let after = &subjects[1];
            resolver.require_job(before)?;
            resolver.require_job(after)?;
            let matches = resolver
                .facts
                .precedence
                .iter()
                .filter(|edge| edge.before == *before && edge.after == *after)
                .collect::<Vec<_>>();
            let [edge] = matches.as_slice() else {
                return Err(format!(
                    "scheduling.precedence_finish_start requires exactly one edge `{before}` -> `{after}`"
                ));
            };
            Ok(conjunction(vec![
                resolver.assignment_is_represented(before),
                resolver.assignment_is_represented(after),
                le(
                    crate::rules::primitives::add(vec![
                        resolver.completion(before),
                        int(edge.minimum_lag),
                    ]),
                    resolver.start(after),
                ),
            ]))
        }
        NativeHandlerV1::SchedulingCumulativeCapacity => {
            let machine_id = &subjects[0];
            let machine = resolver
                .facts
                .machines
                .get(machine_id)
                .ok_or_else(|| format!("unknown scheduling machine `{machine_id}`"))?;
            let mut resources = machine.capacities.keys().cloned().collect::<BTreeSet<_>>();
            for job in resolver.facts.jobs.values() {
                if job.durations.contains_key(machine_id) {
                    resources.extend(job.demands.keys().cloned());
                }
            }
            let mut predicates = Vec::new();
            for resource in resources {
                let capacity = machine.capacities.get(&resource).copied().unwrap_or(0);
                for tick in 0..resolver.facts.horizon {
                    let demand = resolver
                        .placements
                        .iter()
                        .filter(|placement| {
                            placement.machine == *machine_id
                                && placement.start <= tick
                                && tick < placement.start + placement.duration
                        })
                        .filter_map(|placement| {
                            let amount = resolver.facts.jobs[&placement.job]
                                .demands
                                .get(&resource)
                                .copied()
                                .unwrap_or(0);
                            (amount != 0).then(|| mul(vec![int(amount), placement.value.clone()]))
                        })
                        .collect();
                    predicates.push(le(sum(demand), int(capacity)));
                }
            }
            Ok(conjunction(predicates))
        }
        NativeHandlerV1::SchedulingMinimizeMakespan => {
            let makespan = var(resolver
                .makespan_variable
                .as_ref()
                .expect("makespan binding creates a variable"));
            let mut predicates = Vec::with_capacity(subjects.len() + 1);
            for job in subjects {
                resolver.require_job(job)?;
                predicates.push(resolver.assignment_is_represented(job));
                predicates.push(ge(makespan.clone(), resolver.completion(job)));
            }
            if let Some(maximum) = binding.maximum_makespan {
                predicates.push(le(makespan, int(maximum)));
            }
            Ok(conjunction(predicates))
        }
        _ => Err(format!(
            "unsupported scheduling rule `{}`",
            binding.source.rule_id
        )),
    }
}

fn sum(expressions: Vec<ConstraintExpr>) -> ConstraintExpr {
    match expressions.as_slice() {
        [] => int(0),
        [expression] => expression.clone(),
        _ => crate::rules::primitives::add(expressions),
    }
}

fn conjunction(expressions: Vec<ConstraintExpr>) -> ConstraintExpr {
    match expressions.as_slice() {
        [] => boolean(true),
        [expression] => expression.clone(),
        _ => and(expressions),
    }
}

fn reject_duplicates(items: &[String], context: &str) -> Result<(), String> {
    if items.iter().collect::<BTreeSet<_>>().len() != items.len() {
        return Err(format!("{context} contains duplicates"));
    }
    Ok(())
}

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn input_schema() -> Value {
    let rule_ids = manifest_family_executable_rule_ids("scheduling").unwrap_or(&[]);
    json!({
        "type": "object",
        "properties": {
            "family": {"const": "scheduling"},
            "mode": {"type": "string", "enum": ["verify", "synthesize"]},
            "rules": {
                "type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS,
                "items": {
                    "type": "object",
                    "properties": {
                        "rule_id": {"type": "string", "enum": rule_ids},
                        "subjects": {"type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS, "items": {"type": "string"}},
                        "parameters": {
                            "type": "object",
                            "properties": {"maximum_makespan": {"type": "integer", "minimum": 0}},
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
                    "horizon": {"type": "integer", "minimum": 1, "maximum": MAX_SCHEDULING_HORIZON},
                    "jobs": {"type": "object", "minProperties": 1, "maxProperties": MAX_CONSTRAINTS, "additionalProperties": true},
                    "machines": {"type": "object", "minProperties": 1, "maxProperties": MAX_CONSTRAINTS, "additionalProperties": true},
                    "precedence": {"type": "array", "maxItems": MAX_CONSTRAINTS}
                },
                "required": ["horizon", "jobs", "machines", "precedence"],
                "additionalProperties": false
            },
            "unknowns": {
                "type": "array", "maxItems": MAX_VARIABLES, "default": [],
                "items": {
                    "type": "object",
                    "properties": {"kind": {"const": "assignment"}, "job": {"type": "string"}},
                    "required": ["kind", "job"], "additionalProperties": false
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
    use std::collections::BTreeMap;

    use serde_json::{json, Value};

    use super::*;
    use crate::{
        rules::compiler::RuleFamilyCompiler,
        service::SolverService,
        types::{ModelValue, ObjectiveBound, ObjectiveOp, SolveStatus},
    };

    fn fixed_facts(assignments: [(&str, &str, i64); 2]) -> Value {
        let assignments = assignments
            .into_iter()
            .map(|(job, machine, start)| (job, json!({"machine": machine, "start": start})))
            .collect::<BTreeMap<_, _>>();
        json!({
            "horizon": 5,
            "jobs": {
                "a": {
                    "release": 0, "deadline": 5, "durations": {"m1": 2, "m2": 2},
                    "eligible_machines": ["m1", "m2"], "demands": {"cpu": 1},
                    "assignment": assignments["a"]
                },
                "b": {
                    "release": 0, "deadline": 5, "durations": {"m1": 2, "m2": 2},
                    "eligible_machines": ["m1", "m2"], "demands": {"cpu": 1},
                    "assignment": assignments["b"]
                }
            },
            "machines": {
                "m1": {"capacities": {"cpu": 1}},
                "m2": {"capacities": {"cpu": 1}}
            },
            "precedence": [{"before": "a", "after": "b", "minimum_lag": 0}]
        })
    }

    fn verify(rule_id: &str, subjects: &[&str], facts: Value) -> Value {
        json!({
            "family": "scheduling",
            "mode": "verify",
            "rules": [{"rule_id": rule_id, "subjects": subjects, "parameters": {}}],
            "facts": facts,
            "unknowns": []
        })
    }

    fn optimum_request() -> Value {
        json!({
            "family": "scheduling",
            "mode": "synthesize",
            "rules": [
                {"rule_id": "scheduling.assignment_exactly_once", "subjects": ["a"], "parameters": {}},
                {"rule_id": "scheduling.assignment_exactly_once", "subjects": ["b"], "parameters": {}},
                {"rule_id": "scheduling.assignment_exactly_once", "subjects": ["c"], "parameters": {}},
                {"rule_id": "scheduling.placement_allowed", "subjects": ["a"], "parameters": {}},
                {"rule_id": "scheduling.placement_allowed", "subjects": ["b"], "parameters": {}},
                {"rule_id": "scheduling.placement_allowed", "subjects": ["c"], "parameters": {}},
                {"rule_id": "scheduling.cumulative_capacity", "subjects": ["m1"], "parameters": {}},
                {"rule_id": "scheduling.cumulative_capacity", "subjects": ["m2"], "parameters": {}},
                {"rule_id": "scheduling.minimize_makespan", "subjects": ["a", "b", "c"], "parameters": {}}
            ],
            "facts": {
                "horizon": 7,
                "jobs": {
                    "a": {"release": 0, "deadline": 7, "durations": {"m1": 3, "m2": 3}, "eligible_machines": ["m1", "m2"], "demands": {"cpu": 1}, "assignment": null},
                    "b": {"release": 0, "deadline": 7, "durations": {"m1": 2, "m2": 2}, "eligible_machines": ["m1", "m2"], "demands": {"cpu": 1}, "assignment": null},
                    "c": {"release": 0, "deadline": 7, "durations": {"m1": 2, "m2": 2}, "eligible_machines": ["m1", "m2"], "demands": {"cpu": 1}, "assignment": null}
                },
                "machines": {
                    "m1": {"capacities": {"cpu": 1}},
                    "m2": {"capacities": {"cpu": 1}}
                },
                "precedence": []
            },
            "unknowns": [
                {"kind": "assignment", "job": "a"},
                {"kind": "assignment", "job": "b"},
                {"kind": "assignment", "job": "c"}
            ]
        })
    }

    async fn status(input: Value) -> SolveStatus {
        let compiled = COMPILER
            .compile(input)
            .expect("scheduling request must compile");
        SolverService::new()
            .solve_constraints(compiled.request)
            .await
            .expect("scheduling request must solve")
            .status
    }

    #[tokio::test]
    async fn fixed_feasible_schedule_is_satisfiable() {
        let facts = fixed_facts([("a", "m1", 0), ("b", "m2", 2)]);
        let mut input = verify("scheduling.precedence_finish_start", &["a", "b"], facts);
        input["rules"] = json!([
            {"rule_id": "scheduling.assignment_exactly_once", "subjects": ["a"], "parameters": {}},
            {"rule_id": "scheduling.assignment_exactly_once", "subjects": ["b"], "parameters": {}},
            {"rule_id": "scheduling.placement_allowed", "subjects": ["a"], "parameters": {}},
            {"rule_id": "scheduling.placement_allowed", "subjects": ["b"], "parameters": {}},
            {"rule_id": "scheduling.precedence_finish_start", "subjects": ["a", "b"], "parameters": {}},
            {"rule_id": "scheduling.cumulative_capacity", "subjects": ["m1"], "parameters": {}},
            {"rule_id": "scheduling.cumulative_capacity", "subjects": ["m2"], "parameters": {}}
        ]);
        assert_eq!(status(input).await, SolveStatus::Sat);
    }

    #[tokio::test]
    async fn precedence_conflict_is_unsatisfiable() {
        let input = verify(
            "scheduling.precedence_finish_start",
            &["a", "b"],
            fixed_facts([("a", "m1", 0), ("b", "m2", 1)]),
        );
        assert_eq!(status(input).await, SolveStatus::Unsat);
    }

    #[tokio::test]
    async fn overlapping_jobs_exceed_cumulative_capacity() {
        let input = verify(
            "scheduling.cumulative_capacity",
            &["m1"],
            fixed_facts([("a", "m1", 0), ("b", "m1", 1)]),
        );
        assert_eq!(status(input).await, SolveStatus::Unsat);
    }

    #[tokio::test]
    async fn capacity_attribution_ignores_jobs_that_cannot_use_the_machine() {
        let mut facts = fixed_facts([("a", "m1", 0), ("b", "m2", 2)]);
        facts["jobs"]["b"]["durations"] = json!({"m2": 2});
        facts["jobs"]["b"]["eligible_machines"] = json!(["m2"]);
        let input = verify("scheduling.cumulative_capacity", &["m1"], facts);
        assert_eq!(status(input).await, SolveStatus::Sat);
    }

    #[test]
    fn verification_rejects_a_job_without_a_fixed_assignment() {
        let mut facts = fixed_facts([("a", "m1", 0), ("b", "m2", 2)]);
        facts["jobs"]["b"]["assignment"] = Value::Null;

        let error = COMPILER
            .compile(verify("scheduling.cumulative_capacity", &["m1"], facts))
            .expect_err("verification facts must assign every job");
        assert!(error.message.contains(
            "jobs.b.assignment requires either a fixed assignment or a declared assignment unknown"
        ));
    }

    #[test]
    fn synthesis_rejects_a_null_assignment_without_a_declared_unknown() {
        let mut input = optimum_request();
        input["unknowns"] = json!([
            {"kind": "assignment", "job": "a"},
            {"kind": "assignment", "job": "b"}
        ]);

        let error = COMPILER
            .compile(input)
            .expect_err("every null assignment must have a matching unknown");
        assert!(error.message.contains(
            "jobs.c.assignment requires either a fixed assignment or a declared assignment unknown"
        ));
    }

    #[test]
    fn malformed_fixed_assignments_are_rejected_before_lowering() {
        let cases = [
            (
                "unknown machine",
                json!({"machine": "m3", "start": 0}),
                "references unknown machine `m3`",
            ),
            (
                "machine without a duration",
                json!({"machine": "m2", "start": 0}),
                "machine `m2` has no declared duration",
            ),
            (
                "negative start",
                json!({"machine": "m1", "start": -1}),
                "start must be within the scheduling horizon",
            ),
            (
                "completion after horizon",
                json!({"machine": "m1", "start": 4}),
                "completion must be within the scheduling horizon",
            ),
        ];

        for (case, assignment, expected) in cases {
            let mut facts = fixed_facts([("a", "m1", 0), ("b", "m2", 2)]);
            facts["jobs"]["a"]["assignment"] = assignment;
            if case == "machine without a duration" {
                facts["jobs"]["a"]["durations"] = json!({"m1": 2});
            }

            let error = COMPILER
                .compile(verify("scheduling.assignment_exactly_once", &["b"], facts))
                .expect_err(case);
            assert!(
                error.message.contains(expected),
                "{case}: expected `{expected}`, got `{}`",
                error.message
            );
        }
    }

    #[tokio::test]
    async fn synthesis_uses_one_typed_objective_and_decodes_optimum_four() {
        let compiled = COMPILER
            .compile(optimum_request())
            .expect("optimization request must compile");
        assert_eq!(compiled.request.objectives.len(), 1);
        assert_eq!(compiled.request.objectives[0].op, ObjectiveOp::Minimize);

        let response = SolverService::new()
            .solve_constraints(compiled.request)
            .await
            .expect("optimization request must solve");
        assert_eq!(response.status, SolveStatus::Sat);
        let optimization = response.optimization.expect("optimization metadata");
        let objective = &optimization.solutions[0].objectives[0];
        assert_eq!(objective.value, Some(ModelValue::Int(4)));
        assert_eq!(
            objective.bound,
            ObjectiveBound::Finite {
                exact: "4".to_owned()
            }
        );

        let assignments = COMPILER.project_model(
            &compiled.projections,
            response.model.as_ref().expect("sat optimization model"),
        );
        for job in ["a", "b", "c"] {
            assert_eq!(
                assignments
                    .iter()
                    .filter(|assignment| assignment.node == job)
                    .count(),
                2,
                "one decoded machine/start pair for {job}"
            );
        }
        assert!(assignments.iter().any(|assignment| {
            assignment.node == "schedule"
                && assignment.field == "makespan"
                && assignment.value == ModelValue::Int(4)
        }));
    }

    #[test]
    fn verification_requires_a_hard_makespan_bound() {
        let input = verify(
            "scheduling.minimize_makespan",
            &["a", "b"],
            fixed_facts([("a", "m1", 0), ("b", "m2", 2)]),
        );
        assert!(COMPILER
            .compile(input)
            .expect_err("verification bound is required")
            .message
            .contains("maximum_makespan"));
    }

    #[test]
    fn verification_does_not_project_an_existential_makespan_variable() {
        let mut input = verify(
            "scheduling.minimize_makespan",
            &["a", "b"],
            fixed_facts([("a", "m1", 0), ("b", "m2", 2)]),
        );
        input["rules"][0]["parameters"] = json!({"maximum_makespan": 5});
        let compiled = COMPILER
            .compile(input)
            .expect("bounded verification request must compile");
        assert!(compiled.projections.is_empty());
    }

    #[test]
    fn checked_model_size_guards_reject_expanded_capacity_expressions() {
        let resources = (0..64)
            .map(|index| (format!("r{index}"), json!(1)))
            .collect::<serde_json::Map<_, _>>();
        let input = json!({
            "family": "scheduling",
            "mode": "synthesize",
            "rules": [{
                "rule_id": "scheduling.cumulative_capacity",
                "subjects": ["m1"],
                "parameters": {}
            }],
            "facts": {
                "horizon": 64,
                "jobs": {
                    "a": {
                        "release": 0,
                        "deadline": 64,
                        "durations": {"m1": 1},
                        "eligible_machines": ["m1"],
                        "demands": resources,
                        "assignment": null
                    }
                },
                "machines": {"m1": {"capacities": resources}},
                "precedence": []
            },
            "unknowns": [{"kind": "assignment", "job": "a"}]
        });
        assert!(COMPILER
            .compile(input)
            .expect_err("expanded capacity model must be bounded")
            .message
            .contains("checked node budget"));
    }

    #[tokio::test]
    async fn makespan_below_the_fixed_schedule_is_unsatisfiable() {
        let mut input = verify(
            "scheduling.minimize_makespan",
            &["a", "b"],
            fixed_facts([("a", "m1", 0), ("b", "m2", 2)]),
        );
        input["rules"][0]["parameters"] = json!({"maximum_makespan": 3});
        assert_eq!(status(input).await, SolveStatus::Unsat);
    }

    #[test]
    fn scheduling_compiler_is_registered() {
        assert_eq!(
            crate::rules::families::compiler("scheduling").map(RuleFamilyCompiler::id),
            Some("scheduling")
        );
    }
}

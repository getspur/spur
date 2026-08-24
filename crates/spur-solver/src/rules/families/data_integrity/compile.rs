//! Strict finite-relational snapshot validation and typed solver preparation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    rules::{
        compiler::{FamilyCompilation, FamilyCompileError, ModelProjection, RuleFamilyCompiler},
        manifest::{validate_binding_contract, ValidatedBinding},
        manifest_family_executable_rule_ids,
        manifest_format::NativeHandlerV1,
        primitives::{add, and, boolean, eq, ge, int, le, lt, mul, not, or, request, var},
        CompiledRule, RuleSolveMode,
    },
    types::{
        ConstraintExpr, Variable, DEFAULT_TIMEOUT_MS, MAX_CONSTRAINTS, MAX_TIMEOUT_MS,
        MAX_VARIABLES,
    },
};

const MAX_DATA_INTEGRITY_EXPRESSION_NODES: usize = MAX_CONSTRAINTS * MAX_VARIABLES;

/// Data-integrity compiler prepared for registration after semantic lowerings land.
pub static COMPILER: DataIntegrityCompiler = DataIntegrityCompiler;

/// Stateless finite-relational snapshot compiler.
pub struct DataIntegrityCompiler;

impl RuleFamilyCompiler for DataIntegrityCompiler {
    fn id(&self) -> &'static str {
        "data_integrity"
    }

    fn input_schema(&self) -> Value {
        input_schema()
    }

    fn compile(&self, input: Value) -> Result<FamilyCompilation, FamilyCompileError> {
        compile(input).map_err(|message| FamilyCompileError::new(self.id(), message))
    }
}

fn compile(input: Value) -> Result<FamilyCompilation, String> {
    let prepared = prepare(input)?;
    let rules = prepared
        .bindings
        .iter()
        .enumerate()
        .map(|(index, binding)| {
            compile_binding(binding, &prepared.facts, &prepared.variable_names).map(|predicate| {
                CompiledRule::new(
                    binding.rule_id.clone(),
                    index,
                    and(vec![prepared.fixed_facts.clone(), predicate]),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let attributable_rules = rules
        .iter()
        .zip(&prepared.bindings)
        .map(|(rule, binding)| {
            CompiledRule::new(
                format!("{}__subject_{}", rule.rule_id, binding.subject),
                rule.binding_index,
                rule.predicate.clone(),
            )
        })
        .collect::<Vec<_>>();
    let solver_request = request(
        "data_integrity",
        prepared.variables,
        &attributable_rules,
        prepared.timeout_ms,
        prepared.persist,
        prepared.include_smt,
    );
    solver_request
        .validate()
        .map_err(|error| format!("compiled data integrity rules are invalid: {error}"))?;

    Ok(FamilyCompilation {
        mode: prepared.mode,
        request: solver_request,
        rules,
        projections: prepared.projections,
    })
}

struct PreparedDataIntegrity {
    mode: RuleSolveMode,
    bindings: Vec<ValidatedDataIntegrityBinding>,
    facts: DataIntegrityFacts,
    variable_names: BTreeMap<UnknownTarget, String>,
    variables: Vec<Variable>,
    projections: Vec<ModelProjection>,
    fixed_facts: ConstraintExpr,
    timeout_ms: u64,
    persist: bool,
    include_smt: bool,
}

fn prepare(input: Value) -> Result<PreparedDataIntegrity, String> {
    let input: DataIntegrityRequest =
        serde_json::from_value(input).map_err(|error| error.to_string())?;
    if input.family != "data_integrity" {
        return Err(format!(
            "data integrity request family must be `data_integrity`, got `{}`",
            input.family
        ));
    }
    if input.rules.is_empty() {
        return Err("at least one data integrity rule binding is required".to_owned());
    }
    if input.rules.len() > MAX_CONSTRAINTS {
        return Err(format!(
            "request has too many data integrity rule bindings; maximum is {MAX_CONSTRAINTS}"
        ));
    }
    if input.unknowns.len() > MAX_VARIABLES {
        return Err(format!(
            "request has too many data integrity unknowns; maximum is {MAX_VARIABLES}"
        ));
    }
    if !(1..=MAX_TIMEOUT_MS).contains(&input.timeout_ms) {
        return Err(format!("timeout_ms must be between 1 and {MAX_TIMEOUT_MS}"));
    }
    if input.mode == RuleSolveMode::Verify && !input.unknowns.is_empty() {
        return Err(
            "verification requires complete data integrity facts; remove unknown declarations"
                .to_owned(),
        );
    }

    validate_snapshot(&input.facts)?;
    let unknown_targets = validate_unknowns(&input.facts, &input.unknowns)?;
    validate_completeness(&input.facts, &unknown_targets)?;
    validate_definitions(&input.facts)?;
    validate_expression_budget(&input.facts)?;
    let bindings = input
        .rules
        .iter()
        .map(|binding| validate_manifest_binding(binding, &input.facts))
        .collect::<Result<Vec<_>, _>>()?;
    let resolver = SnapshotResolver::new(&input.facts, &input.unknowns)?;
    let SnapshotResolver {
        variables,
        projections,
        fixed_facts,
        names,
    } = resolver;

    Ok(PreparedDataIntegrity {
        mode: input.mode,
        bindings,
        facts: input.facts,
        variable_names: names,
        variables,
        projections,
        fixed_facts: conjunction(fixed_facts),
        timeout_ms: input.timeout_ms,
        persist: input.persist,
        include_smt: input.include_smt,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DataIntegrityRequest {
    family: String,
    mode: RuleSolveMode,
    rules: Vec<DataIntegrityRuleBinding>,
    facts: DataIntegrityFacts,
    #[serde(default)]
    unknowns: Vec<DataIntegrityUnknown>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    persist: bool,
    #[serde(default)]
    include_smt: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DataIntegrityRuleBinding {
    rule_id: String,
    subjects: Vec<String>,
    #[serde(default)]
    parameters: DataIntegrityParameters,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DataIntegrityParameters {}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DataIntegrityFacts {
    relations: BTreeMap<String, RelationFacts>,
    unique_constraints: BTreeMap<String, UniqueConstraintFacts>,
    foreign_keys: BTreeMap<String, ForeignKeyFacts>,
    cardinality_constraints: BTreeMap<String, CardinalityConstraintFacts>,
    value_ranges: BTreeMap<String, ValueRangeFacts>,
    conditional_requirements: BTreeMap<String, ConditionalRequirementFacts>,
    aggregate_balances: BTreeMap<String, AggregateBalanceFacts>,
    consistency_relations: BTreeMap<String, ConsistencyRelationFacts>,
    temporal_constraints: BTreeMap<String, TemporalConstraintFacts>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelationFacts {
    fields: BTreeMap<String, FieldDomainFacts>,
    rows: BTreeMap<String, RowFacts>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum FieldDomainFacts {
    Integer { minimum: i64, maximum: i64 },
    Enum { values: Vec<String> },
    Boolean,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RowFacts {
    active: Option<bool>,
    cells: BTreeMap<String, CellFacts>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CellFacts {
    present: Option<bool>,
    value: Option<CellValueFacts>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
enum CellValueFacts {
    Integer(i64),
    Boolean(bool),
    Enum(String),
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct UniqueConstraintFacts {
    relation: String,
    fields: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForeignKeyFacts {
    child_relation: String,
    child_fields: Vec<String>,
    parent_relation: String,
    parent_fields: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CardinalityConstraintFacts {
    relation: String,
    #[serde(default)]
    rows: Option<Vec<String>>,
    minimum: i64,
    maximum: i64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValueRangeFacts {
    relation: String,
    field: String,
    minimum: i64,
    maximum: i64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConditionalRequirementFacts {
    relation: String,
    trigger_field: String,
    expected: CellValueFacts,
    required_field: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AggregateBalanceFacts {
    terms: Vec<AggregateTermFacts>,
    target: i64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AggregateTermFacts {
    relation: String,
    row: String,
    field: String,
    coefficient: i64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsistencyRelationFacts {
    relation: String,
    fields: Vec<String>,
    allowed: Vec<Vec<CellValueFacts>>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TemporalConstraintFacts {
    relation: String,
    start_field: String,
    end_field: String,
    predecessors: Vec<PredecessorFacts>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PredecessorFacts {
    before: String,
    after: String,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DataIntegrityUnknown {
    RowActive {
        relation: String,
        row: String,
    },
    CellPresent {
        relation: String,
        row: String,
        field: String,
    },
    CellValue {
        relation: String,
        row: String,
        field: String,
    },
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum UnknownTarget {
    RowActive(String, String),
    CellPresent(String, String, String),
    CellValue(String, String, String),
}

impl DataIntegrityUnknown {
    fn target(&self) -> UnknownTarget {
        match self {
            Self::RowActive { relation, row } => {
                UnknownTarget::RowActive(relation.clone(), row.clone())
            }
            Self::CellPresent {
                relation,
                row,
                field,
            } => UnknownTarget::CellPresent(relation.clone(), row.clone(), field.clone()),
            Self::CellValue {
                relation,
                row,
                field,
            } => UnknownTarget::CellValue(relation.clone(), row.clone(), field.clone()),
        }
    }
}

struct ValidatedDataIntegrityBinding {
    rule_id: String,
    subject: String,
    handler: NativeHandlerV1,
}

fn validate_manifest_binding(
    binding: &DataIntegrityRuleBinding,
    facts: &DataIntegrityFacts,
) -> Result<ValidatedDataIntegrityBinding, String> {
    let parameters =
        serde_json::to_value(&binding.parameters).map_err(|error| error.to_string())?;
    let Value::Object(parameters) = parameters else {
        return Err("data integrity parameters did not serialize as an object".to_owned());
    };
    let ValidatedBinding { handler, .. } =
        validate_binding_contract(&binding.rule_id, &binding.subjects, &parameters)
            .map_err(|message| format!("data integrity rule `{}`: {message}", binding.rule_id))?;
    let subject = binding.subjects.first().ok_or_else(|| {
        format!(
            "data integrity rule `{}` requires one subject",
            binding.rule_id
        )
    })?;
    validate_binding_subject(handler, subject, facts)?;
    Ok(ValidatedDataIntegrityBinding {
        rule_id: binding.rule_id.clone(),
        subject: subject.clone(),
        handler,
    })
}

fn validate_binding_subject(
    handler: NativeHandlerV1,
    subject: &str,
    facts: &DataIntegrityFacts,
) -> Result<(), String> {
    let exists = match handler {
        NativeHandlerV1::DataIntegrityUnique => facts.unique_constraints.contains_key(subject),
        NativeHandlerV1::DataIntegrityForeignKey => facts.foreign_keys.contains_key(subject),
        NativeHandlerV1::DataIntegrityCardinality => {
            facts.cardinality_constraints.contains_key(subject)
        }
        NativeHandlerV1::DataIntegrityValueRange => facts.value_ranges.contains_key(subject),
        NativeHandlerV1::DataIntegrityConditionalRequired => {
            facts.conditional_requirements.contains_key(subject)
        }
        NativeHandlerV1::DataIntegrityAggregateBalance => {
            facts.aggregate_balances.contains_key(subject)
        }
        NativeHandlerV1::DataIntegrityMutuallyConsistent => {
            facts.consistency_relations.contains_key(subject)
        }
        NativeHandlerV1::DataIntegrityTemporalConsistency => {
            facts.temporal_constraints.contains_key(subject)
        }
        _ => {
            return Err(format!(
                "handler `{handler:?}` does not belong to data_integrity"
            ));
        }
    };
    if !exists {
        return Err(format!(
            "data integrity handler `{handler:?}` references unknown definition `{subject}`"
        ));
    }
    Ok(())
}

fn compile_binding(
    binding: &ValidatedDataIntegrityBinding,
    facts: &DataIntegrityFacts,
    variable_names: &BTreeMap<UnknownTarget, String>,
) -> Result<ConstraintExpr, String> {
    match binding.handler {
        NativeHandlerV1::DataIntegrityUnique => {
            compile_unique(&binding.subject, facts, variable_names)
        }
        NativeHandlerV1::DataIntegrityForeignKey => {
            compile_foreign_key(&binding.subject, facts, variable_names)
        }
        NativeHandlerV1::DataIntegrityCardinality => {
            compile_cardinality(&binding.subject, facts, variable_names)
        }
        NativeHandlerV1::DataIntegrityValueRange => {
            compile_value_range(&binding.subject, facts, variable_names)
        }
        NativeHandlerV1::DataIntegrityConditionalRequired => {
            compile_conditional_required(&binding.subject, facts, variable_names)
        }
        NativeHandlerV1::DataIntegrityAggregateBalance => {
            compile_aggregate_balance(&binding.subject, facts, variable_names)
        }
        NativeHandlerV1::DataIntegrityMutuallyConsistent => {
            compile_mutually_consistent(&binding.subject, facts, variable_names)
        }
        NativeHandlerV1::DataIntegrityTemporalConsistency => {
            compile_temporal_consistency(&binding.subject, facts, variable_names)
        }
        _ => Err(format!(
            "unsupported data integrity rule `{}`",
            binding.rule_id
        )),
    }
}

fn compile_unique(
    subject: &str,
    facts: &DataIntegrityFacts,
    variable_names: &BTreeMap<UnknownTarget, String>,
) -> Result<ConstraintExpr, String> {
    let definition = facts
        .unique_constraints
        .get(subject)
        .ok_or_else(|| format!("unknown unique constraint `{subject}`"))?;
    let relation = require_relation(facts, &definition.relation)?;
    let rows = relation.rows.keys().collect::<Vec<_>>();
    let mut pairs = Vec::new();
    for (left_index, left_row) in rows.iter().enumerate() {
        for right_row in rows.iter().skip(left_index + 1) {
            let mut antecedent = vec![
                is_one(row_active(variable_names, &definition.relation, left_row)?),
                is_one(row_active(variable_names, &definition.relation, right_row)?),
            ];
            let mut distinct = Vec::with_capacity(definition.fields.len());
            for field in &definition.fields {
                antecedent.push(is_one(cell_present(
                    variable_names,
                    &definition.relation,
                    left_row,
                    field,
                )?));
                antecedent.push(is_one(cell_present(
                    variable_names,
                    &definition.relation,
                    right_row,
                    field,
                )?));
                distinct.push(not(eq(
                    cell_value(variable_names, &definition.relation, left_row, field)?,
                    cell_value(variable_names, &definition.relation, right_row, field)?,
                )));
            }
            pairs.push(implies(conjunction(antecedent), disjunction(distinct)));
        }
    }
    Ok(conjunction(pairs))
}

fn compile_foreign_key(
    subject: &str,
    facts: &DataIntegrityFacts,
    variable_names: &BTreeMap<UnknownTarget, String>,
) -> Result<ConstraintExpr, String> {
    let definition = facts
        .foreign_keys
        .get(subject)
        .ok_or_else(|| format!("unknown foreign key `{subject}`"))?;
    let children = require_relation(facts, &definition.child_relation)?;
    let parents = require_relation(facts, &definition.parent_relation)?;
    let mut child_requirements = Vec::with_capacity(children.rows.len());
    for child_row in children.rows.keys() {
        let mut antecedent = vec![is_one(row_active(
            variable_names,
            &definition.child_relation,
            child_row,
        )?)];
        for child_field in &definition.child_fields {
            antecedent.push(is_one(cell_present(
                variable_names,
                &definition.child_relation,
                child_row,
                child_field,
            )?));
        }

        let mut candidates = Vec::with_capacity(parents.rows.len());
        for parent_row in parents.rows.keys() {
            let mut candidate = vec![is_one(row_active(
                variable_names,
                &definition.parent_relation,
                parent_row,
            )?)];
            for (child_field, parent_field) in definition
                .child_fields
                .iter()
                .zip(&definition.parent_fields)
            {
                candidate.push(is_one(cell_present(
                    variable_names,
                    &definition.parent_relation,
                    parent_row,
                    parent_field,
                )?));
                candidate.push(eq(
                    cell_value(
                        variable_names,
                        &definition.child_relation,
                        child_row,
                        child_field,
                    )?,
                    cell_value(
                        variable_names,
                        &definition.parent_relation,
                        parent_row,
                        parent_field,
                    )?,
                ));
            }
            candidates.push(conjunction(candidate));
        }
        child_requirements.push(implies(conjunction(antecedent), disjunction(candidates)));
    }
    Ok(conjunction(child_requirements))
}

fn compile_cardinality(
    subject: &str,
    facts: &DataIntegrityFacts,
    variable_names: &BTreeMap<UnknownTarget, String>,
) -> Result<ConstraintExpr, String> {
    let definition = facts
        .cardinality_constraints
        .get(subject)
        .ok_or_else(|| format!("unknown cardinality constraint `{subject}`"))?;
    let relation = require_relation(facts, &definition.relation)?;
    let active = match &definition.rows {
        Some(rows) => rows
            .iter()
            .map(|row| row_active(variable_names, &definition.relation, row))
            .collect::<Result<Vec<_>, _>>()?,
        None => relation
            .rows
            .keys()
            .map(|row| row_active(variable_names, &definition.relation, row))
            .collect::<Result<Vec<_>, _>>()?,
    };
    let count = linear_sum(active);
    Ok(and(vec![
        ge(count.clone(), int(definition.minimum)),
        le(count, int(definition.maximum)),
    ]))
}

fn compile_value_range(
    subject: &str,
    facts: &DataIntegrityFacts,
    variable_names: &BTreeMap<UnknownTarget, String>,
) -> Result<ConstraintExpr, String> {
    let definition = facts
        .value_ranges
        .get(subject)
        .ok_or_else(|| format!("unknown value range `{subject}`"))?;
    let relation = require_relation(facts, &definition.relation)?;
    let mut rows = Vec::with_capacity(relation.rows.len());
    for row in relation.rows.keys() {
        let antecedent = and(vec![
            is_one(row_active(variable_names, &definition.relation, row)?),
            is_one(cell_present(
                variable_names,
                &definition.relation,
                row,
                &definition.field,
            )?),
        ]);
        let value = cell_value(variable_names, &definition.relation, row, &definition.field)?;
        rows.push(implies(
            antecedent,
            and(vec![
                ge(value.clone(), int(definition.minimum)),
                le(value, int(definition.maximum)),
            ]),
        ));
    }
    Ok(conjunction(rows))
}

fn compile_conditional_required(
    subject: &str,
    facts: &DataIntegrityFacts,
    variable_names: &BTreeMap<UnknownTarget, String>,
) -> Result<ConstraintExpr, String> {
    let definition = facts
        .conditional_requirements
        .get(subject)
        .ok_or_else(|| format!("unknown conditional requirement `{subject}`"))?;
    let relation = require_relation(facts, &definition.relation)?;
    let mut rows = Vec::with_capacity(relation.rows.len());
    for row in relation.rows.keys() {
        let trigger = and(vec![
            is_one(row_active(variable_names, &definition.relation, row)?),
            is_one(cell_present(
                variable_names,
                &definition.relation,
                row,
                &definition.trigger_field,
            )?),
            typed_cell_equals(
                variable_names,
                &definition.relation,
                row,
                &definition.trigger_field,
                &definition.expected,
            )?,
        ]);
        rows.push(implies(
            trigger,
            is_one(cell_present(
                variable_names,
                &definition.relation,
                row,
                &definition.required_field,
            )?),
        ));
    }
    Ok(conjunction(rows))
}

fn compile_aggregate_balance(
    subject: &str,
    facts: &DataIntegrityFacts,
    variable_names: &BTreeMap<UnknownTarget, String>,
) -> Result<ConstraintExpr, String> {
    let definition = facts
        .aggregate_balances
        .get(subject)
        .ok_or_else(|| format!("unknown aggregate balance `{subject}`"))?;
    let mut present = Vec::with_capacity(definition.terms.len());
    let mut terms = Vec::with_capacity(definition.terms.len());
    for term in &definition.terms {
        present.push(is_one(cell_present(
            variable_names,
            &term.relation,
            &term.row,
            &term.field,
        )?));
        terms.push(mul(vec![
            int(term.coefficient),
            cell_value(variable_names, &term.relation, &term.row, &term.field)?,
        ]));
    }
    Ok(and(vec![
        conjunction(present),
        eq(linear_sum(terms), int(definition.target)),
    ]))
}

fn compile_mutually_consistent(
    subject: &str,
    facts: &DataIntegrityFacts,
    variable_names: &BTreeMap<UnknownTarget, String>,
) -> Result<ConstraintExpr, String> {
    let definition = facts
        .consistency_relations
        .get(subject)
        .ok_or_else(|| format!("unknown consistency relation `{subject}`"))?;
    let relation = require_relation(facts, &definition.relation)?;
    let mut rows = Vec::with_capacity(relation.rows.len());
    for row in relation.rows.keys() {
        let present = definition
            .fields
            .iter()
            .map(|field| cell_present(variable_names, &definition.relation, row, field).map(is_one))
            .collect::<Result<Vec<_>, _>>()?;
        let allowed = definition
            .allowed
            .iter()
            .map(|tuple| {
                definition
                    .fields
                    .iter()
                    .zip(tuple)
                    .map(|(field, value)| {
                        typed_cell_equals(variable_names, &definition.relation, row, field, value)
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(conjunction)
            })
            .collect::<Result<Vec<_>, _>>()?;
        rows.push(implies(
            is_one(row_active(variable_names, &definition.relation, row)?),
            and(vec![conjunction(present), disjunction(allowed)]),
        ));
    }
    Ok(conjunction(rows))
}

fn compile_temporal_consistency(
    subject: &str,
    facts: &DataIntegrityFacts,
    variable_names: &BTreeMap<UnknownTarget, String>,
) -> Result<ConstraintExpr, String> {
    let definition = facts
        .temporal_constraints
        .get(subject)
        .ok_or_else(|| format!("unknown temporal constraint `{subject}`"))?;
    let relation = require_relation(facts, &definition.relation)?;
    let mut requirements = Vec::with_capacity(
        relation
            .rows
            .len()
            .saturating_add(definition.predecessors.len()),
    );
    for row in relation.rows.keys() {
        requirements.push(implies(
            is_one(row_active(variable_names, &definition.relation, row)?),
            and(vec![
                is_one(cell_present(
                    variable_names,
                    &definition.relation,
                    row,
                    &definition.start_field,
                )?),
                is_one(cell_present(
                    variable_names,
                    &definition.relation,
                    row,
                    &definition.end_field,
                )?),
                lt(
                    cell_value(
                        variable_names,
                        &definition.relation,
                        row,
                        &definition.start_field,
                    )?,
                    cell_value(
                        variable_names,
                        &definition.relation,
                        row,
                        &definition.end_field,
                    )?,
                ),
            ]),
        ));
    }
    for edge in &definition.predecessors {
        requirements.push(implies(
            is_one(row_active(
                variable_names,
                &definition.relation,
                &edge.after,
            )?),
            and(vec![
                is_one(row_active(
                    variable_names,
                    &definition.relation,
                    &edge.before,
                )?),
                le(
                    cell_value(
                        variable_names,
                        &definition.relation,
                        &edge.before,
                        &definition.end_field,
                    )?,
                    cell_value(
                        variable_names,
                        &definition.relation,
                        &edge.after,
                        &definition.start_field,
                    )?,
                ),
            ]),
        ));
    }
    Ok(conjunction(requirements))
}

fn typed_cell_equals(
    variable_names: &BTreeMap<UnknownTarget, String>,
    relation: &str,
    row: &str,
    field: &str,
    expected: &CellValueFacts,
) -> Result<ConstraintExpr, String> {
    let target = UnknownTarget::CellValue(relation.to_owned(), row.to_owned(), field.to_owned());
    let variable = variable_names
        .get(&target)
        .ok_or_else(|| format!("validated data integrity target `{target:?}` has no variable"))?;
    Ok(eq(
        var(variable.clone()),
        literal_for_value(variable, expected),
    ))
}

fn row_active(
    variable_names: &BTreeMap<UnknownTarget, String>,
    relation: &str,
    row: &str,
) -> Result<ConstraintExpr, String> {
    snapshot_variable(
        variable_names,
        &UnknownTarget::RowActive(relation.to_owned(), row.to_owned()),
    )
}

fn cell_present(
    variable_names: &BTreeMap<UnknownTarget, String>,
    relation: &str,
    row: &str,
    field: &str,
) -> Result<ConstraintExpr, String> {
    snapshot_variable(
        variable_names,
        &UnknownTarget::CellPresent(relation.to_owned(), row.to_owned(), field.to_owned()),
    )
}

fn cell_value(
    variable_names: &BTreeMap<UnknownTarget, String>,
    relation: &str,
    row: &str,
    field: &str,
) -> Result<ConstraintExpr, String> {
    snapshot_variable(
        variable_names,
        &UnknownTarget::CellValue(relation.to_owned(), row.to_owned(), field.to_owned()),
    )
}

fn snapshot_variable(
    variable_names: &BTreeMap<UnknownTarget, String>,
    target: &UnknownTarget,
) -> Result<ConstraintExpr, String> {
    variable_names
        .get(target)
        .cloned()
        .map(var)
        .ok_or_else(|| format!("validated data integrity target `{target:?}` has no variable"))
}

fn is_one(expression: ConstraintExpr) -> ConstraintExpr {
    eq(expression, int(1))
}

fn implies(antecedent: ConstraintExpr, consequent: ConstraintExpr) -> ConstraintExpr {
    or(vec![not(antecedent), consequent])
}

fn disjunction(items: Vec<ConstraintExpr>) -> ConstraintExpr {
    match items.len() {
        0 => boolean(false),
        1 => items.into_iter().next().expect("one item"),
        _ => or(items),
    }
}

fn linear_sum(items: Vec<ConstraintExpr>) -> ConstraintExpr {
    match items.len() {
        0 => int(0),
        1 => items.into_iter().next().expect("one item"),
        _ => add(items),
    }
}

fn validate_snapshot(facts: &DataIntegrityFacts) -> Result<(), String> {
    for (relation_id, relation) in &facts.relations {
        require_id("relation", relation_id)?;
        if relation.fields.is_empty() {
            return Err(format!(
                "relation `{relation_id}` must declare at least one field"
            ));
        }
        for (field_id, domain) in &relation.fields {
            require_id("field", field_id)?;
            validate_domain(relation_id, field_id, domain)?;
        }
        for (row_id, row) in &relation.rows {
            require_id("row", row_id)?;
            for field_id in relation.fields.keys() {
                if !row.cells.contains_key(field_id) {
                    return Err(format!(
                        "relation `{relation_id}` row `{row_id}` is missing cell `{field_id}`"
                    ));
                }
            }
            for (field_id, cell) in &row.cells {
                let domain = relation.fields.get(field_id).ok_or_else(|| {
                    format!(
                        "relation `{relation_id}` row `{row_id}` references unknown field `{field_id}`"
                    )
                })?;
                if cell.present == Some(false) && cell.value.is_some() {
                    return Err(format!(
                        "relation `{relation_id}` row `{row_id}` cell `{field_id}` is absent but has a value"
                    ));
                }
                if let Some(value) = &cell.value {
                    validate_value(domain, value).map_err(|message| {
                        format!(
                            "relation `{relation_id}` row `{row_id}` cell `{field_id}` {message}"
                        )
                    })?;
                }
            }
        }
    }
    Ok(())
}

fn validate_domain(relation: &str, field: &str, domain: &FieldDomainFacts) -> Result<(), String> {
    match domain {
        FieldDomainFacts::Integer { minimum, maximum } if minimum > maximum => Err(format!(
            "relation `{relation}` field `{field}` minimum exceeds maximum"
        )),
        FieldDomainFacts::Enum { values } => {
            if values.is_empty() {
                return Err(format!(
                    "relation `{relation}` field `{field}` enum domain must not be empty"
                ));
            }
            let mut seen = BTreeSet::new();
            for value in values {
                require_id("enum label", value)?;
                if !seen.insert(value) {
                    return Err(format!(
                        "relation `{relation}` field `{field}` has duplicate enum label `{value}`"
                    ));
                }
            }
            Ok(())
        }
        FieldDomainFacts::Integer { .. } | FieldDomainFacts::Boolean => Ok(()),
    }
}

fn validate_value(domain: &FieldDomainFacts, value: &CellValueFacts) -> Result<(), String> {
    match (domain, value) {
        (FieldDomainFacts::Integer { minimum, maximum }, CellValueFacts::Integer(value))
            if (*minimum..=*maximum).contains(value) =>
        {
            Ok(())
        }
        (FieldDomainFacts::Integer { minimum, maximum }, CellValueFacts::Integer(value)) => Err(
            format!("value {value} is outside integer domain {minimum}..={maximum}"),
        ),
        (FieldDomainFacts::Enum { values }, CellValueFacts::Enum(value))
            if values.contains(value) =>
        {
            Ok(())
        }
        (FieldDomainFacts::Enum { .. }, CellValueFacts::Enum(value)) => {
            Err(format!("value `{value}` is outside enum domain"))
        }
        (FieldDomainFacts::Boolean, CellValueFacts::Boolean(_)) => Ok(()),
        _ => Err("value does not match the declared field domain".to_owned()),
    }
}

fn validate_unknowns(
    facts: &DataIntegrityFacts,
    unknowns: &[DataIntegrityUnknown],
) -> Result<BTreeSet<UnknownTarget>, String> {
    let mut targets = BTreeSet::new();
    for unknown in unknowns {
        let target = unknown.target();
        if !targets.insert(target.clone()) {
            return Err(format!("duplicate data integrity unknown `{target:?}`"));
        }
        match &target {
            UnknownTarget::RowActive(relation, row) => {
                let row = require_row(facts, relation, row)?;
                if row.active.is_some() {
                    return Err(format!(
                        "row_active unknown `{relation}.{}` already has a fixed fact",
                        row_id(&target)
                    ));
                }
            }
            UnknownTarget::CellPresent(relation, row, field) => {
                let cell = require_cell(facts, relation, row, field)?;
                if cell.present.is_some() {
                    return Err(format!(
                        "cell_present unknown `{relation}.{row}.{field}` already has a fixed fact"
                    ));
                }
            }
            UnknownTarget::CellValue(relation, row, field) => {
                let cell = require_cell(facts, relation, row, field)?;
                if cell.present == Some(false) {
                    return Err(format!(
                        "cell_value unknown `{relation}.{row}.{field}` targets an absent cell"
                    ));
                }
                if cell.value.is_some() {
                    return Err(format!(
                        "cell_value unknown `{relation}.{row}.{field}` already has a fixed fact"
                    ));
                }
            }
        }
    }
    Ok(targets)
}

fn row_id(target: &UnknownTarget) -> &str {
    match target {
        UnknownTarget::RowActive(_, row) => row,
        UnknownTarget::CellPresent(_, row, _) | UnknownTarget::CellValue(_, row, _) => row,
    }
}

fn validate_completeness(
    facts: &DataIntegrityFacts,
    unknowns: &BTreeSet<UnknownTarget>,
) -> Result<(), String> {
    for (relation_id, relation) in &facts.relations {
        for (row_id, row) in &relation.rows {
            if row.active.is_none()
                && !unknowns.contains(&UnknownTarget::RowActive(
                    relation_id.clone(),
                    row_id.clone(),
                ))
            {
                return Err(format!(
                    "null `{relation_id}.{row_id}.active` requires a row_active unknown"
                ));
            }
            for (field_id, cell) in &row.cells {
                if cell.present.is_none()
                    && !unknowns.contains(&UnknownTarget::CellPresent(
                        relation_id.clone(),
                        row_id.clone(),
                        field_id.clone(),
                    ))
                {
                    return Err(format!(
                        "null `{relation_id}.{row_id}.{field_id}.present` requires a cell_present unknown"
                    ));
                }
                if cell.present != Some(false)
                    && cell.value.is_none()
                    && !unknowns.contains(&UnknownTarget::CellValue(
                        relation_id.clone(),
                        row_id.clone(),
                        field_id.clone(),
                    ))
                {
                    return Err(format!(
                        "null `{relation_id}.{row_id}.{field_id}.value` requires a cell_value unknown"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_definitions(facts: &DataIntegrityFacts) -> Result<(), String> {
    for (id, definition) in &facts.unique_constraints {
        require_id("unique constraint", id)?;
        let relation = require_relation(facts, &definition.relation)?;
        validate_field_list(&definition.relation, relation, &definition.fields)?;
    }
    for (id, definition) in &facts.foreign_keys {
        require_id("foreign key", id)?;
        let child = require_relation(facts, &definition.child_relation)?;
        let parent = require_relation(facts, &definition.parent_relation)?;
        validate_field_list(&definition.child_relation, child, &definition.child_fields)?;
        validate_field_list(
            &definition.parent_relation,
            parent,
            &definition.parent_fields,
        )?;
        if definition.child_fields.len() != definition.parent_fields.len() {
            return Err(format!(
                "foreign key `{id}` has mismatched composite-key arity"
            ));
        }
        for (child_field, parent_field) in definition
            .child_fields
            .iter()
            .zip(&definition.parent_fields)
        {
            if !compatible_domains(&child.fields[child_field], &parent.fields[parent_field]) {
                return Err(format!(
                    "foreign key `{id}` has incompatible field domains for `{child_field}` and `{parent_field}`"
                ));
            }
        }
    }
    for (id, definition) in &facts.cardinality_constraints {
        require_id("cardinality constraint", id)?;
        let relation = require_relation(facts, &definition.relation)?;
        if definition.minimum < 0 || definition.maximum < 0 {
            return Err(format!(
                "cardinality constraint `{id}` bounds must be nonnegative"
            ));
        }
        validate_bounds(id, definition.minimum, definition.maximum)?;
        if let Some(rows) = &definition.rows {
            validate_id_list("cardinality row", rows)?;
            for row in rows {
                if !relation.rows.contains_key(row) {
                    return Err(format!(
                        "cardinality constraint `{id}` references unknown row `{row}`"
                    ));
                }
            }
        }
    }
    for (id, definition) in &facts.value_ranges {
        require_id("value range", id)?;
        validate_bounds(id, definition.minimum, definition.maximum)?;
        let domain = require_field(facts, &definition.relation, &definition.field)?;
        if !matches!(domain, FieldDomainFacts::Integer { .. }) {
            return Err(format!("value range `{id}` requires an integer field"));
        }
    }
    for (id, definition) in &facts.conditional_requirements {
        require_id("conditional requirement", id)?;
        let trigger = require_field(facts, &definition.relation, &definition.trigger_field)?;
        require_field(facts, &definition.relation, &definition.required_field)?;
        validate_value(trigger, &definition.expected).map_err(|message| {
            format!("conditional requirement `{id}` expected value {message}")
        })?;
    }
    for (id, definition) in &facts.aggregate_balances {
        require_id("aggregate balance", id)?;
        if definition.terms.is_empty() {
            return Err(format!(
                "aggregate balance `{id}` must declare at least one term"
            ));
        }
        let _ = definition.target;
        for term in &definition.terms {
            require_row(facts, &term.relation, &term.row)?;
            let domain = require_field(facts, &term.relation, &term.field)?;
            if !matches!(domain, FieldDomainFacts::Integer { .. }) {
                return Err(format!(
                    "aggregate balance `{id}` requires integer term cells"
                ));
            }
            let _ = term.coefficient;
        }
    }
    for (id, definition) in &facts.consistency_relations {
        require_id("consistency relation", id)?;
        let relation = require_relation(facts, &definition.relation)?;
        validate_field_list(&definition.relation, relation, &definition.fields)?;
        if definition.allowed.is_empty() {
            return Err(format!(
                "consistency relation `{id}` must declare allowed tuples"
            ));
        }
        let mut tuples = BTreeSet::new();
        for tuple in &definition.allowed {
            if tuple.len() != definition.fields.len() {
                return Err(format!(
                    "consistency relation `{id}` has malformed tuple arity"
                ));
            }
            for (field, value) in definition.fields.iter().zip(tuple) {
                validate_value(&relation.fields[field], value).map_err(|message| {
                    format!("consistency relation `{id}` tuple field `{field}` {message}")
                })?;
            }
            let fingerprint = format!("{tuple:?}");
            if !tuples.insert(fingerprint) {
                return Err(format!(
                    "consistency relation `{id}` has a duplicate allowed tuple"
                ));
            }
        }
    }
    for (id, definition) in &facts.temporal_constraints {
        require_id("temporal constraint", id)?;
        if definition.start_field == definition.end_field {
            return Err(format!(
                "temporal constraint `{id}` requires distinct endpoint fields"
            ));
        }
        for field in [&definition.start_field, &definition.end_field] {
            let domain = require_field(facts, &definition.relation, field)?;
            if !matches!(domain, FieldDomainFacts::Integer { .. }) {
                return Err(format!(
                    "temporal constraint `{id}` requires integer endpoints"
                ));
            }
        }
        let mut edges = BTreeSet::new();
        for edge in &definition.predecessors {
            require_row(facts, &definition.relation, &edge.before)?;
            require_row(facts, &definition.relation, &edge.after)?;
            if edge.before == edge.after {
                return Err(format!("temporal constraint `{id}` has a self predecessor"));
            }
            if !edges.insert((&edge.before, &edge.after)) {
                return Err(format!(
                    "temporal constraint `{id}` has a duplicate predecessor"
                ));
            }
        }
    }
    Ok(())
}

fn validate_bounds(id: &str, minimum: i64, maximum: i64) -> Result<(), String> {
    if minimum > maximum {
        return Err(format!("definition `{id}` minimum exceeds maximum"));
    }
    Ok(())
}

fn validate_field_list(
    relation_id: &str,
    relation: &RelationFacts,
    fields: &[String],
) -> Result<(), String> {
    validate_id_list("field reference", fields)?;
    for field in fields {
        if !relation.fields.contains_key(field) {
            return Err(format!(
                "relation `{relation_id}` references unknown field `{field}`"
            ));
        }
    }
    Ok(())
}

fn validate_id_list(kind: &str, ids: &[String]) -> Result<(), String> {
    if ids.is_empty() {
        return Err(format!("{kind} list must not be empty"));
    }
    let mut seen = BTreeSet::new();
    for id in ids {
        require_id(kind, id)?;
        if !seen.insert(id) {
            return Err(format!("duplicate {kind} `{id}`"));
        }
    }
    Ok(())
}

fn compatible_domains(left: &FieldDomainFacts, right: &FieldDomainFacts) -> bool {
    match (left, right) {
        (FieldDomainFacts::Integer { .. }, FieldDomainFacts::Integer { .. })
        | (FieldDomainFacts::Boolean, FieldDomainFacts::Boolean) => true,
        (FieldDomainFacts::Enum { values: left }, FieldDomainFacts::Enum { values: right }) => {
            left == right
        }
        _ => false,
    }
}

fn validate_expression_budget(facts: &DataIntegrityFacts) -> Result<(), String> {
    let mut total = 0usize;
    for definition in facts.unique_constraints.values() {
        let rows = require_relation(facts, &definition.relation)?.rows.len();
        total = checked_budget_add(
            total,
            estimate_expression_nodes(rows, rows, definition.fields.len())?,
        )?;
    }
    for definition in facts.foreign_keys.values() {
        let child_rows = require_relation(facts, &definition.child_relation)?
            .rows
            .len();
        let parent_rows = require_relation(facts, &definition.parent_relation)?
            .rows
            .len();
        total = checked_budget_add(
            total,
            estimate_expression_nodes(child_rows, parent_rows, definition.child_fields.len())?,
        )?;
    }
    for definition in facts.cardinality_constraints.values() {
        let rows = match &definition.rows {
            Some(rows) => rows.len(),
            None => require_relation(facts, &definition.relation)?.rows.len(),
        };
        total = checked_budget_add(total, rows)?;
    }
    for definition in facts.value_ranges.values() {
        total = checked_budget_add(
            total,
            require_relation(facts, &definition.relation)?.rows.len(),
        )?;
    }
    for definition in facts.conditional_requirements.values() {
        total = checked_budget_add(
            total,
            require_relation(facts, &definition.relation)?.rows.len(),
        )?;
    }
    for definition in facts.aggregate_balances.values() {
        total = checked_budget_add(total, definition.terms.len())?;
    }
    for definition in facts.consistency_relations.values() {
        let rows = require_relation(facts, &definition.relation)?.rows.len();
        total = checked_budget_add(
            total,
            estimate_expression_nodes(rows, definition.allowed.len(), definition.fields.len())?,
        )?;
    }
    for definition in facts.temporal_constraints.values() {
        let rows = require_relation(facts, &definition.relation)?.rows.len();
        total = checked_budget_add(total, rows)?;
        total = checked_budget_add(total, definition.predecessors.len())?;
    }
    if total > MAX_DATA_INTEGRITY_EXPRESSION_NODES {
        return Err(format!(
            "data integrity expression estimate {total} exceeds {MAX_DATA_INTEGRITY_EXPRESSION_NODES} nodes"
        ));
    }
    Ok(())
}

fn estimate_expression_nodes(a: usize, b: usize, c: usize) -> Result<usize, String> {
    let estimate = a
        .checked_mul(b)
        .and_then(|value| value.checked_mul(c))
        .ok_or_else(|| "data integrity expression budget overflow".to_owned())?;
    if estimate > MAX_DATA_INTEGRITY_EXPRESSION_NODES {
        return Err(format!(
            "data integrity expression estimate {estimate} exceeds {MAX_DATA_INTEGRITY_EXPRESSION_NODES} nodes"
        ));
    }
    Ok(estimate)
}

fn checked_budget_add(left: usize, right: usize) -> Result<usize, String> {
    let total = left
        .checked_add(right)
        .ok_or_else(|| "data integrity expression budget overflow".to_owned())?;
    if total > MAX_DATA_INTEGRITY_EXPRESSION_NODES {
        return Err(format!(
            "data integrity expression estimate {total} exceeds {MAX_DATA_INTEGRITY_EXPRESSION_NODES} nodes"
        ));
    }
    Ok(total)
}

struct SnapshotResolver {
    variables: Vec<Variable>,
    projections: Vec<ModelProjection>,
    fixed_facts: Vec<ConstraintExpr>,
    names: BTreeMap<UnknownTarget, String>,
}

impl SnapshotResolver {
    fn new(facts: &DataIntegrityFacts, unknowns: &[DataIntegrityUnknown]) -> Result<Self, String> {
        let mut variables = Vec::new();
        let mut fixed_facts = Vec::new();
        let mut names = BTreeMap::new();
        for (relation_index, (relation_id, relation)) in facts.relations.iter().enumerate() {
            for (row_index, (row_id, row)) in relation.rows.iter().enumerate() {
                let active_name = format!("di_r{relation_index}_row{row_index}_active");
                push_variable(
                    &mut variables,
                    Variable::IntRange {
                        name: active_name.clone(),
                        min: 0,
                        max: 1,
                    },
                )?;
                names.insert(
                    UnknownTarget::RowActive(relation_id.clone(), row_id.clone()),
                    active_name.clone(),
                );
                if let Some(active) = row.active {
                    fixed_facts.push(eq(var(active_name), int(i64::from(active))));
                }
                for (field_index, (field_id, domain)) in relation.fields.iter().enumerate() {
                    let cell = &row.cells[field_id];
                    let present_name =
                        format!("di_r{relation_index}_row{row_index}_f{field_index}_present");
                    push_variable(
                        &mut variables,
                        Variable::IntRange {
                            name: present_name.clone(),
                            min: 0,
                            max: 1,
                        },
                    )?;
                    names.insert(
                        UnknownTarget::CellPresent(
                            relation_id.clone(),
                            row_id.clone(),
                            field_id.clone(),
                        ),
                        present_name.clone(),
                    );
                    if let Some(present) = cell.present {
                        fixed_facts.push(eq(var(present_name), int(i64::from(present))));
                    }

                    let value_name =
                        format!("di_r{relation_index}_row{row_index}_f{field_index}_value");
                    push_variable(
                        &mut variables,
                        variable_for_domain(value_name.clone(), domain),
                    )?;
                    names.insert(
                        UnknownTarget::CellValue(
                            relation_id.clone(),
                            row_id.clone(),
                            field_id.clone(),
                        ),
                        value_name.clone(),
                    );
                    if let Some(value) = &cell.value {
                        fixed_facts.push(eq(
                            var(value_name.clone()),
                            literal_for_value(&value_name, value),
                        ));
                    }
                }
            }
        }

        let projections = unknowns
            .iter()
            .map(|unknown| {
                let target = unknown.target();
                let variable = names
                    .get(&target)
                    .cloned()
                    .expect("validated unknown target has a variable");
                match unknown {
                    DataIntegrityUnknown::RowActive { relation, row } => ModelProjection {
                        variable,
                        subject: relation.clone(),
                        field: format!("rows.{row}.active"),
                    },
                    DataIntegrityUnknown::CellPresent {
                        relation,
                        row,
                        field,
                    } => ModelProjection {
                        variable,
                        subject: relation.clone(),
                        field: format!("rows.{row}.cells.{field}.present"),
                    },
                    DataIntegrityUnknown::CellValue {
                        relation,
                        row,
                        field,
                    } => ModelProjection {
                        variable,
                        subject: relation.clone(),
                        field: format!("rows.{row}.cells.{field}.value"),
                    },
                }
            })
            .collect();

        Ok(Self {
            variables,
            projections,
            fixed_facts,
            names,
        })
    }
}

fn push_variable(variables: &mut Vec<Variable>, variable: Variable) -> Result<(), String> {
    if variables.len() == MAX_VARIABLES {
        return Err(format!(
            "data integrity snapshot requires more than {MAX_VARIABLES} solver variables"
        ));
    }
    variables.push(variable);
    Ok(())
}

fn variable_for_domain(name: String, domain: &FieldDomainFacts) -> Variable {
    match domain {
        FieldDomainFacts::Integer { minimum, maximum } => Variable::IntRange {
            name,
            min: *minimum,
            max: *maximum,
        },
        FieldDomainFacts::Enum { values } => Variable::Enum {
            name,
            values: values.clone(),
        },
        FieldDomainFacts::Boolean => Variable::IntRange {
            name,
            min: 0,
            max: 1,
        },
    }
}

fn literal_for_value(variable: &str, value: &CellValueFacts) -> ConstraintExpr {
    match value {
        CellValueFacts::Integer(value) => int(*value),
        CellValueFacts::Boolean(value) => int(i64::from(*value)),
        CellValueFacts::Enum(label) => ConstraintExpr::EnumLabel {
            var: variable.to_owned(),
            label: label.clone(),
        },
    }
}

fn require_relation<'a>(
    facts: &'a DataIntegrityFacts,
    relation: &str,
) -> Result<&'a RelationFacts, String> {
    facts
        .relations
        .get(relation)
        .ok_or_else(|| format!("unknown relation `{relation}`"))
}

fn require_row<'a>(
    facts: &'a DataIntegrityFacts,
    relation: &str,
    row: &str,
) -> Result<&'a RowFacts, String> {
    require_relation(facts, relation)?
        .rows
        .get(row)
        .ok_or_else(|| format!("relation `{relation}` references unknown row `{row}`"))
}

fn require_field<'a>(
    facts: &'a DataIntegrityFacts,
    relation: &str,
    field: &str,
) -> Result<&'a FieldDomainFacts, String> {
    require_relation(facts, relation)?
        .fields
        .get(field)
        .ok_or_else(|| format!("relation `{relation}` references unknown field `{field}`"))
}

fn require_cell<'a>(
    facts: &'a DataIntegrityFacts,
    relation: &str,
    row: &str,
    field: &str,
) -> Result<&'a CellFacts, String> {
    require_field(facts, relation, field)?;
    require_row(facts, relation, row)?
        .cells
        .get(field)
        .ok_or_else(|| format!("relation `{relation}` row `{row}` is missing cell `{field}`"))
}

fn require_id(kind: &str, id: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err(format!("{kind} ID must not be empty"));
    }
    Ok(())
}

fn conjunction(items: Vec<ConstraintExpr>) -> ConstraintExpr {
    match items.len() {
        0 => boolean(true),
        1 => items.into_iter().next().expect("one item"),
        _ => and(items),
    }
}

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn input_schema() -> Value {
    let rule_ids = manifest_family_executable_rule_ids("data_integrity")
        .expect("data integrity manifest executable rule IDs");
    let identifier = json!({"type": "string", "minLength": 1});
    let scalar = json!({"type": ["integer", "boolean", "string"]});
    let value = json!({"type": ["integer", "boolean", "string", "null"]});
    let field_domain = json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {"kind": {"const": "integer"}, "minimum": {"type": "integer"}, "maximum": {"type": "integer"}},
                "required": ["kind", "minimum", "maximum"], "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {"kind": {"const": "enum"}, "values": {"type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS, "items": identifier}},
                "required": ["kind", "values"], "additionalProperties": false
            },
            {
                "type": "object", "properties": {"kind": {"const": "boolean"}},
                "required": ["kind"], "additionalProperties": false
            }
        ]
    });
    let unknowns = json!({
        "type": "array", "maxItems": MAX_VARIABLES, "default": [],
        "items": {"oneOf": [
            {"type": "object", "properties": {"kind": {"const": "row_active"}, "relation": identifier, "row": identifier}, "required": ["kind", "relation", "row"], "additionalProperties": false},
            {"type": "object", "properties": {"kind": {"const": "cell_present"}, "relation": identifier, "row": identifier, "field": identifier}, "required": ["kind", "relation", "row", "field"], "additionalProperties": false},
            {"type": "object", "properties": {"kind": {"const": "cell_value"}, "relation": identifier, "row": identifier, "field": identifier}, "required": ["kind", "relation", "row", "field"], "additionalProperties": false}
        ]}
    });
    let field_list = json!({
        "type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS,
        "items": identifier
    });
    let unique_definition = json!({
        "type": "object",
        "properties": {"relation": identifier, "fields": field_list},
        "required": ["relation", "fields"], "additionalProperties": false
    });
    let foreign_key_definition = json!({
        "type": "object",
        "properties": {
            "child_relation": identifier, "child_fields": field_list,
            "parent_relation": identifier, "parent_fields": field_list
        },
        "required": ["child_relation", "child_fields", "parent_relation", "parent_fields"],
        "additionalProperties": false
    });
    let cardinality_definition = json!({
        "type": "object",
        "properties": {
            "relation": identifier,
            "rows": {"type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS, "items": identifier},
            "minimum": {"type": "integer"}, "maximum": {"type": "integer"}
        },
        "required": ["relation", "minimum", "maximum"], "additionalProperties": false
    });
    let value_range_definition = json!({
        "type": "object",
        "properties": {
            "relation": identifier, "field": identifier,
            "minimum": {"type": "integer"}, "maximum": {"type": "integer"}
        },
        "required": ["relation", "field", "minimum", "maximum"],
        "additionalProperties": false
    });
    let conditional_definition = json!({
        "type": "object",
        "properties": {
            "relation": identifier, "trigger_field": identifier,
            "expected": scalar, "required_field": identifier
        },
        "required": ["relation", "trigger_field", "expected", "required_field"],
        "additionalProperties": false
    });
    let aggregate_definition = json!({
        "type": "object",
        "properties": {
            "terms": {
                "type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS,
                "items": {
                    "type": "object",
                    "properties": {
                        "relation": identifier, "row": identifier, "field": identifier,
                        "coefficient": {"type": "integer"}
                    },
                    "required": ["relation", "row", "field", "coefficient"],
                    "additionalProperties": false
                }
            },
            "target": {"type": "integer"}
        },
        "required": ["terms", "target"], "additionalProperties": false
    });
    let consistency_definition = json!({
        "type": "object",
        "properties": {
            "relation": identifier, "fields": field_list,
            "allowed": {
                "type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS,
                "items": {"type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS, "items": scalar}
            }
        },
        "required": ["relation", "fields", "allowed"], "additionalProperties": false
    });
    let temporal_definition = json!({
        "type": "object",
        "properties": {
            "relation": identifier, "start_field": identifier, "end_field": identifier,
            "predecessors": {
                "type": "array", "maxItems": MAX_CONSTRAINTS,
                "items": {
                    "type": "object",
                    "properties": {"before": identifier, "after": identifier},
                    "required": ["before", "after"], "additionalProperties": false
                }
            }
        },
        "required": ["relation", "start_field", "end_field", "predecessors"],
        "additionalProperties": false
    });
    json!({
        "type": "object",
        "properties": {
            "family": {"const": "data_integrity"},
            "mode": {"type": "string", "enum": ["verify", "synthesize"]},
            "rules": {
                "type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS,
                "items": {
                    "type": "object",
                    "properties": {
                        "rule_id": {"type": "string", "enum": rule_ids},
                        "subjects": {"type": "array", "minItems": 1, "maxItems": 1, "items": identifier},
                        "parameters": {"type": "object", "additionalProperties": false}
                    },
                    "required": ["rule_id", "subjects"], "additionalProperties": false
                }
            },
            "facts": {
                "type": "object",
                "properties": {
                    "relations": {
                        "type": "object", "maxProperties": MAX_CONSTRAINTS,
                        "additionalProperties": {
                            "type": "object",
                            "properties": {
                                "fields": {"type": "object", "minProperties": 1, "maxProperties": MAX_CONSTRAINTS, "additionalProperties": field_domain},
                                "rows": {
                                    "type": "object", "maxProperties": MAX_CONSTRAINTS,
                                    "additionalProperties": {
                                        "type": "object",
                                        "properties": {
                                            "active": {"type": ["boolean", "null"]},
                                            "cells": {
                                                "type": "object", "maxProperties": MAX_CONSTRAINTS,
                                                "additionalProperties": {
                                                    "type": "object",
                                                    "properties": {"present": {"type": ["boolean", "null"]}, "value": value},
                                                    "required": ["present", "value"], "additionalProperties": false
                                                }
                                            }
                                        },
                                        "required": ["active", "cells"], "additionalProperties": false
                                    }
                                }
                            },
                            "required": ["fields", "rows"], "additionalProperties": false
                        }
                    },
                    "unique_constraints": {"type": "object", "maxProperties": MAX_CONSTRAINTS, "additionalProperties": unique_definition},
                    "foreign_keys": {"type": "object", "maxProperties": MAX_CONSTRAINTS, "additionalProperties": foreign_key_definition},
                    "cardinality_constraints": {"type": "object", "maxProperties": MAX_CONSTRAINTS, "additionalProperties": cardinality_definition},
                    "value_ranges": {"type": "object", "maxProperties": MAX_CONSTRAINTS, "additionalProperties": value_range_definition},
                    "conditional_requirements": {"type": "object", "maxProperties": MAX_CONSTRAINTS, "additionalProperties": conditional_definition},
                    "aggregate_balances": {"type": "object", "maxProperties": MAX_CONSTRAINTS, "additionalProperties": aggregate_definition},
                    "consistency_relations": {"type": "object", "maxProperties": MAX_CONSTRAINTS, "additionalProperties": consistency_definition},
                    "temporal_constraints": {"type": "object", "maxProperties": MAX_CONSTRAINTS, "additionalProperties": temporal_definition}
                },
                "required": ["relations", "unique_constraints", "foreign_keys", "cardinality_constraints", "value_ranges", "conditional_requirements", "aggregate_balances", "consistency_relations", "temporal_constraints"],
                "additionalProperties": false
            },
            "unknowns": unknowns,
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

    use super::{compile, estimate_expression_nodes, prepare};
    use crate::{service::SolverService, types::SolveStatus};

    #[test]
    fn family_compilers_keep_owned_handlers_before_stable_foreign_fallbacks() {
        fn function_source<'a>(source: &'a str, function: &str) -> &'a str {
            let signature = format!("\nfn {function}(");
            let (_, tail) = source
                .split_once(&signature)
                .unwrap_or_else(|| panic!("family compiler must define {function}"));
            let end = tail
                .find("\nfn ")
                .unwrap_or_else(|| panic!("{function} must precede another top-level function"));
            &tail[..end]
        }

        let cases: [(&str, &str, &[&str]); 4] = [
            (
                "data integrity",
                include_str!("compile.rs"),
                &[
                    "DataIntegrityUnique",
                    "DataIntegrityForeignKey",
                    "DataIntegrityCardinality",
                    "DataIntegrityValueRange",
                    "DataIntegrityConditionalRequired",
                    "DataIntegrityAggregateBalance",
                    "DataIntegrityMutuallyConsistent",
                    "DataIntegrityTemporalConsistency",
                ],
            ),
            (
                "resource",
                include_str!("../resource/compile.rs"),
                &[
                    "ResourceRequestWithinLimit",
                    "ResourceAggregateCapacity",
                    "ResourceQuotaCapacity",
                    "PlacementTopologyMaxSkew",
                    "PlacementMinimumFailureDomains",
                    "PlacementMinimizeSkew",
                ],
            ),
            (
                "scheduling",
                include_str!("../scheduling/compile.rs"),
                &[
                    "SchedulingAssignmentExactlyOnce",
                    "SchedulingPlacementAllowed",
                    "SchedulingPrecedenceFinishStart",
                    "SchedulingCumulativeCapacity",
                    "SchedulingMinimizeMakespan",
                ],
            ),
            (
                "workflow",
                include_str!("../workflow/compile.rs"),
                &[
                    "WorkflowInitialStateAllowed",
                    "WorkflowTransitionAllowed",
                    "WorkflowSafetyInvariant",
                    "WorkflowBoundedReachability",
                ],
            ),
        ];

        for (family, source, owned_handlers) in cases {
            let binding_source = function_source(source, "compile_binding");
            for handler in owned_handlers {
                assert!(
                    binding_source.contains(&format!("NativeHandlerV1::{handler}")),
                    "{family} compiler must keep owned handler {handler} explicit"
                );
            }
            assert!(
                binding_source.contains("_ => Err(format!("),
                "{family} compiler must use one stable fallback for foreign handlers"
            );
            assert!(
                binding_source.contains(&format!("\"unsupported {family} rule")),
                "{family} compiler fallback must report the source rule id"
            );
        }

        let helper_cases: [(&str, &str, &str, &[&str], &str, &str); 2] = [
            (
                "data integrity",
                include_str!("compile.rs"),
                "validate_binding_subject",
                &[
                    "DataIntegrityUnique",
                    "DataIntegrityForeignKey",
                    "DataIntegrityCardinality",
                    "DataIntegrityValueRange",
                    "DataIntegrityConditionalRequired",
                    "DataIntegrityAggregateBalance",
                    "DataIntegrityMutuallyConsistent",
                    "DataIntegrityTemporalConsistency",
                ],
                "_ => {",
                "does not belong to data_integrity",
            ),
            (
                "resource",
                include_str!("../resource/compile.rs"),
                "validate_legacy_static_contract",
                &[
                    "ResourceRequestWithinLimit",
                    "ResourceAggregateCapacity",
                    "ResourceQuotaCapacity",
                    "PlacementTopologyMaxSkew",
                    "PlacementMinimumFailureDomains",
                    "PlacementMinimizeSkew",
                ],
                "Some(_) | None => Err(format!(",
                "unsupported resource rule",
            ),
        ];

        for (family, source, function, owned_handlers, fallback, diagnostic) in helper_cases {
            let helper_source = function_source(source, function);
            let fallback_index = helper_source
                .find(fallback)
                .unwrap_or_else(|| panic!("{function} must use one stable foreign fallback"));
            assert_eq!(
                helper_source.matches(fallback).count(),
                1,
                "{function} must use exactly one stable foreign fallback"
            );
            assert_eq!(
                helper_source.matches("NativeHandlerV1::").count(),
                owned_handlers.len(),
                "{function} must not enumerate foreign handlers"
            );
            for handler in owned_handlers {
                let owned_index = helper_source
                    .find(&format!("NativeHandlerV1::{handler}"))
                    .unwrap_or_else(|| {
                        panic!("{function} must keep owned handler {handler} explicit")
                    });
                assert!(
                    owned_index < fallback_index,
                    "{function} must keep owned handler {handler} before its fallback"
                );
            }
            assert!(
                helper_source.contains(diagnostic),
                "{family} helper fallback must preserve its diagnostic"
            );
        }
    }

    fn empty_facts() -> Value {
        json!({
            "relations": {},
            "unique_constraints": {},
            "foreign_keys": {},
            "cardinality_constraints": {},
            "value_ranges": {},
            "conditional_requirements": {},
            "aggregate_balances": {},
            "consistency_relations": {},
            "temporal_constraints": {}
        })
    }

    fn request(mode: &str, rule_id: &str, subject: &str, facts: Value) -> Value {
        json!({
            "family": "data_integrity",
            "mode": mode,
            "rules": [{"rule_id": rule_id, "subjects": [subject], "parameters": {}}],
            "facts": facts,
            "unknowns": []
        })
    }

    fn one_cell_facts(domain: Value, active: Value, present: Value, value: Value) -> Value {
        let mut facts = empty_facts();
        facts["relations"] = json!({
            "records": {
                "fields": {"key": domain},
                "rows": {"first": {"active": active, "cells": {"key": {"present": present, "value": value}}}}
            }
        });
        facts["unique_constraints"] =
            json!({"record_key": {"relation": "records", "fields": ["key"]}});
        facts
    }

    fn unique_request(duplicate_complete_key: bool) -> Value {
        let mut facts = empty_facts();
        facts["relations"] = json!({
            "records": {
                "fields": {"key": {"kind": "integer", "minimum": 0, "maximum": 9}},
                "rows": {
                    "first": {"active": true, "cells": {"key": {"present": true, "value": 1}}},
                    "second": {
                        "active": true,
                        "cells": {"key": if duplicate_complete_key {
                            json!({"present": true, "value": 1})
                        } else {
                            json!({"present": false, "value": null})
                        }}
                    }
                }
            }
        });
        facts["unique_constraints"] =
            json!({"record_key": {"relation": "records", "fields": ["key"]}});
        request("verify", "data_integrity.unique", "record_key", facts)
    }

    fn foreign_key_request(child_key_present: bool, parent_match: bool) -> Value {
        let mut facts = empty_facts();
        facts["relations"] = json!({
            "children": {
                "fields": {"parent_id": {"kind": "integer", "minimum": 0, "maximum": 9}},
                "rows": {"child": {
                    "active": true,
                    "cells": {"parent_id": if child_key_present {
                        json!({"present": true, "value": 1})
                    } else {
                        json!({"present": false, "value": null})
                    }}
                }}
            },
            "parents": {
                "fields": {"id": {"kind": "integer", "minimum": 0, "maximum": 9}},
                "rows": {"parent": {
                    "active": true,
                    "cells": {"id": {"present": true, "value": if parent_match { 1 } else { 2 }}}
                }}
            }
        });
        facts["foreign_keys"] = json!({
            "child_parent": {
                "child_relation": "children",
                "child_fields": ["parent_id"],
                "parent_relation": "parents",
                "parent_fields": ["id"]
            }
        });
        request(
            "verify",
            "data_integrity.foreign_key",
            "child_parent",
            facts,
        )
    }

    fn cardinality_request(active: &[bool], minimum: i64, maximum: i64) -> Value {
        let mut facts = empty_facts();
        let rows = active
            .iter()
            .enumerate()
            .map(|(index, active)| {
                (
                    format!("row_{index}"),
                    json!({
                        "active": active,
                        "cells": {"marker": {"present": false, "value": null}}
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        facts["relations"] = json!({
            "records": {
                "fields": {"marker": {"kind": "boolean"}},
                "rows": rows
            }
        });
        facts["cardinality_constraints"] = json!({
            "record_count": {"relation": "records", "minimum": minimum, "maximum": maximum}
        });
        request(
            "verify",
            "data_integrity.cardinality",
            "record_count",
            facts,
        )
    }

    fn value_range_request(active: bool, present: bool, value: Option<i64>) -> Value {
        let mut facts = empty_facts();
        facts["relations"] = json!({
            "measurements": {
                "fields": {"reading": {"kind": "integer", "minimum": 0, "maximum": 20}},
                "rows": {"sample": {
                    "active": active,
                    "cells": {"reading": {"present": present, "value": value}}
                }}
            }
        });
        facts["value_ranges"] = json!({
            "reading_range": {"relation": "measurements", "field": "reading", "minimum": 5, "maximum": 10}
        });
        request(
            "verify",
            "data_integrity.value_range",
            "reading_range",
            facts,
        )
    }

    fn conditional_request(
        trigger_domain: Value,
        expected: Value,
        trigger_value: Value,
        required_present: Option<bool>,
    ) -> Value {
        let mut facts = empty_facts();
        let required_value = if required_present == Some(false) {
            Value::Null
        } else {
            json!(true)
        };
        facts["relations"] = json!({
            "records": {
                "fields": {
                    "trigger": trigger_domain,
                    "required": {"kind": "boolean"}
                },
                "rows": {"first": {
                    "active": true,
                    "cells": {
                        "trigger": {"present": true, "value": trigger_value},
                        "required": {"present": required_present, "value": required_value}
                    }
                }}
            }
        });
        facts["conditional_requirements"] = json!({
            "required_when_triggered": {
                "relation": "records",
                "trigger_field": "trigger",
                "expected": expected,
                "required_field": "required"
            }
        });
        request(
            "verify",
            "data_integrity.conditional_required",
            "required_when_triggered",
            facts,
        )
    }

    fn balance_request(
        left: i64,
        delta: Option<i64>,
        target: i64,
        delta_active: bool,
        delta_present: bool,
    ) -> Value {
        let mut facts = empty_facts();
        facts["relations"] = json!({
            "ledger": {
                "fields": {"amount": {"kind": "integer", "minimum": -200, "maximum": 200}},
                "rows": {
                    "left": {
                        "active": true,
                        "cells": {"amount": {"present": true, "value": left}}
                    },
                    "delta": {
                        "active": delta_active,
                        "cells": {"amount": {
                            "present": delta_present,
                            "value": if delta_present { json!(delta) } else { Value::Null }
                        }}
                    }
                }
            }
        });
        facts["aggregate_balances"] = json!({
            "ledger_total": {
                "terms": [
                    {"relation": "ledger", "row": "left", "field": "amount", "coefficient": 1},
                    {"relation": "ledger", "row": "delta", "field": "amount", "coefficient": 1}
                ],
                "target": target
            }
        });
        request(
            "verify",
            "data_integrity.aggregate_balance",
            "ledger_total",
            facts,
        )
    }

    fn consistency_request(
        active: bool,
        state_present: bool,
        state: &str,
        enabled: Option<bool>,
    ) -> Value {
        let mut facts = empty_facts();
        facts["relations"] = json!({
            "configurations": {
                "fields": {
                    "state": {"kind": "enum", "values": ["draft", "live"]},
                    "enabled": {"kind": "boolean"}
                },
                "rows": {"config": {
                    "active": active,
                    "cells": {
                        "state": {
                            "present": state_present,
                            "value": if state_present { json!(state) } else { Value::Null }
                        },
                        "enabled": {"present": true, "value": enabled}
                    }
                }}
            }
        });
        facts["consistency_relations"] = json!({
            "state_enabled": {
                "relation": "configurations",
                "fields": ["state", "enabled"],
                "allowed": [["draft", false], ["live", true]]
            }
        });
        request(
            "verify",
            "data_integrity.mutually_consistent",
            "state_enabled",
            facts,
        )
    }

    fn temporal_request(
        before_active: bool,
        before_start: i64,
        before_end: i64,
        after_active: bool,
        after_start: Option<i64>,
        after_end: i64,
    ) -> Value {
        let mut facts = empty_facts();
        facts["relations"] = json!({
            "intervals": {
                "fields": {
                    "start": {"kind": "integer", "minimum": 0, "maximum": 20},
                    "end": {"kind": "integer", "minimum": 0, "maximum": 20}
                },
                "rows": {
                    "before": {
                        "active": before_active,
                        "cells": {
                            "start": {"present": true, "value": before_start},
                            "end": {"present": true, "value": before_end}
                        }
                    },
                    "after": {
                        "active": after_active,
                        "cells": {
                            "start": {"present": true, "value": after_start},
                            "end": {"present": true, "value": after_end}
                        }
                    }
                }
            }
        });
        facts["temporal_constraints"] = json!({
            "ordered_intervals": {
                "relation": "intervals",
                "start_field": "start",
                "end_field": "end",
                "predecessors": [{"before": "before", "after": "after"}]
            }
        });
        request(
            "verify",
            "data_integrity.temporal_consistency",
            "ordered_intervals",
            facts,
        )
    }

    async fn status(input: Value) -> SolveStatus {
        let compiled = compile(input).expect("data integrity request must compile");
        SolverService::new()
            .solve_constraints(compiled.request)
            .await
            .expect("data integrity request must solve")
            .status
    }

    #[tokio::test]
    async fn conditional_required_uses_typed_enum_and_boolean_trigger_equality() {
        let enum_domain = json!({"kind": "enum", "values": ["open", "closed"]});
        assert_eq!(
            status(conditional_request(
                enum_domain.clone(),
                json!("open"),
                json!("open"),
                Some(true),
            ))
            .await,
            SolveStatus::Sat
        );
        assert_eq!(
            status(conditional_request(
                enum_domain.clone(),
                json!("open"),
                json!("open"),
                Some(false),
            ))
            .await,
            SolveStatus::Unsat
        );
        assert_eq!(
            status(conditional_request(
                enum_domain,
                json!("open"),
                json!("closed"),
                Some(false),
            ))
            .await,
            SolveStatus::Sat
        );
        assert_eq!(
            status(conditional_request(
                json!({"kind": "boolean"}),
                json!(true),
                json!(true),
                Some(false),
            ))
            .await,
            SolveStatus::Unsat
        );
    }

    #[tokio::test]
    async fn conditional_required_synthesizes_required_presence() {
        let mut input =
            conditional_request(json!({"kind": "boolean"}), json!(true), json!(true), None);
        input["mode"] = json!("synthesize");
        input["unknowns"] = json!([
            {"kind": "cell_present", "relation": "records", "row": "first", "field": "required"}
        ]);
        let compiled = compile(input).expect("conditional synthesis must compile");
        assert_eq!(
            compiled.projections[0].field,
            "rows.first.cells.required.present"
        );
        assert_eq!(
            SolverService::new()
                .solve_constraints(compiled.request)
                .await
                .expect("conditional synthesis must solve")
                .status,
            SolveStatus::Sat
        );
    }

    #[tokio::test]
    async fn aggregate_balance_counts_inactive_explicit_terms_and_rejects_off_by_one() {
        assert_eq!(
            status(balance_request(100, Some(-30), 70, false, true)).await,
            SolveStatus::Sat
        );
        assert_eq!(
            status(balance_request(100, Some(-30), 69, false, true)).await,
            SolveStatus::Unsat
        );
    }

    #[tokio::test]
    async fn aggregate_balance_requires_every_explicit_term_present() {
        assert_eq!(
            status(balance_request(100, None, 100, false, false)).await,
            SolveStatus::Unsat
        );
    }

    #[tokio::test]
    async fn aggregate_balance_synthesizes_an_inactive_integer_term() {
        let mut input = balance_request(100, Some(-30), 70, false, true);
        input["mode"] = json!("synthesize");
        input["facts"]["relations"]["ledger"]["rows"]["delta"]["cells"]["amount"]["value"] =
            Value::Null;
        input["unknowns"] = json!([
            {"kind": "cell_value", "relation": "ledger", "row": "delta", "field": "amount"}
        ]);
        let compiled = compile(input).expect("aggregate synthesis must compile");
        assert_eq!(
            compiled.projections[0].field,
            "rows.delta.cells.amount.value"
        );
        assert_eq!(
            SolverService::new()
                .solve_constraints(compiled.request)
                .await
                .expect("aggregate synthesis must solve")
                .status,
            SolveStatus::Sat
        );
    }

    #[tokio::test]
    async fn mutually_consistent_requires_present_finite_enum_boolean_tuple() {
        assert_eq!(
            status(consistency_request(true, true, "live", Some(true))).await,
            SolveStatus::Sat
        );
        assert_eq!(
            status(consistency_request(true, true, "live", Some(false))).await,
            SolveStatus::Unsat
        );
        assert_eq!(
            status(consistency_request(true, false, "live", Some(true))).await,
            SolveStatus::Unsat
        );
        assert_eq!(
            status(consistency_request(false, true, "live", Some(false))).await,
            SolveStatus::Sat
        );
    }

    #[tokio::test]
    async fn mutually_consistent_synthesizes_a_boolean_tuple_member() {
        let mut input = consistency_request(true, true, "live", Some(true));
        input["mode"] = json!("synthesize");
        input["facts"]["relations"]["configurations"]["rows"]["config"]["cells"]["enabled"]
            ["value"] = Value::Null;
        input["unknowns"] = json!([
            {"kind": "cell_value", "relation": "configurations", "row": "config", "field": "enabled"}
        ]);
        assert_eq!(status(input).await, SolveStatus::Sat);
    }

    #[tokio::test]
    async fn temporal_consistency_rejects_interval_and_predecessor_counterexamples() {
        assert_eq!(
            status(temporal_request(true, 0, 4, true, Some(4), 6)).await,
            SolveStatus::Sat
        );
        assert_eq!(
            status(temporal_request(true, 0, 4, true, Some(5), 4)).await,
            SolveStatus::Unsat
        );
        assert_eq!(
            status(temporal_request(true, 0, 6, true, Some(5), 7)).await,
            SolveStatus::Unsat
        );
        assert_eq!(
            status(temporal_request(false, 0, 4, true, Some(4), 6)).await,
            SolveStatus::Unsat
        );
    }

    #[tokio::test]
    async fn temporal_consistency_synthesizes_a_bounded_integer_endpoint() {
        let mut input = temporal_request(true, 0, 4, true, Some(4), 6);
        input["mode"] = json!("synthesize");
        input["facts"]["relations"]["intervals"]["rows"]["after"]["cells"]["start"]["value"] =
            Value::Null;
        input["unknowns"] = json!([
            {"kind": "cell_value", "relation": "intervals", "row": "after", "field": "start"}
        ]);
        assert_eq!(status(input).await, SolveStatus::Sat);
    }

    #[tokio::test]
    async fn unique_ignores_incomplete_keys_but_rejects_equal_complete_keys() {
        assert_eq!(status(unique_request(false)).await, SolveStatus::Sat);
        assert_eq!(status(unique_request(true)).await, SolveStatus::Unsat);
        let mut inactive_duplicate = unique_request(true);
        inactive_duplicate["facts"]["relations"]["records"]["rows"]["second"]["active"] =
            json!(false);
        assert_eq!(status(inactive_duplicate).await, SolveStatus::Sat);
    }

    #[tokio::test]
    async fn unique_synthesizes_a_distinct_complete_key() {
        let mut input = unique_request(true);
        input["mode"] = json!("synthesize");
        input["facts"]["relations"]["records"]["rows"]["second"]["cells"]["key"]["value"] =
            Value::Null;
        input["unknowns"] = json!([
            {"kind": "cell_value", "relation": "records", "row": "second", "field": "key"}
        ]);
        let compiled = compile(input).expect("bounded unique synthesis must compile");
        assert_eq!(compiled.projections[0].field, "rows.second.cells.key.value");
        assert_eq!(
            SolverService::new()
                .solve_constraints(compiled.request)
                .await
                .expect("bounded unique synthesis must solve")
                .status,
            SolveStatus::Sat
        );
    }

    #[tokio::test]
    async fn foreign_key_match_simple_accepts_absent_child_key() {
        assert_eq!(
            status(foreign_key_request(false, false)).await,
            SolveStatus::Sat
        );
    }

    #[tokio::test]
    async fn foreign_key_requires_an_active_complete_matching_parent() {
        assert_eq!(
            status(foreign_key_request(true, true)).await,
            SolveStatus::Sat
        );
        assert_eq!(
            status(foreign_key_request(true, false)).await,
            SolveStatus::Unsat
        );
        let mut inactive_parent = foreign_key_request(true, true);
        inactive_parent["facts"]["relations"]["parents"]["rows"]["parent"]["active"] = json!(false);
        assert_eq!(status(inactive_parent).await, SolveStatus::Unsat);
    }

    #[tokio::test]
    async fn foreign_key_synthesizes_a_bounded_parent_match() {
        let mut input = foreign_key_request(true, true);
        input["mode"] = json!("synthesize");
        input["facts"]["relations"]["children"]["rows"]["child"]["cells"]["parent_id"]["value"] =
            Value::Null;
        input["unknowns"] = json!([
            {"kind": "cell_value", "relation": "children", "row": "child", "field": "parent_id"}
        ]);
        assert_eq!(status(input).await, SolveStatus::Sat);
    }

    #[tokio::test]
    async fn cardinality_bounds_are_inclusive() {
        assert_eq!(
            status(cardinality_request(&[true, false], 1, 2)).await,
            SolveStatus::Sat
        );
        assert_eq!(
            status(cardinality_request(&[true, true], 1, 2)).await,
            SolveStatus::Sat
        );
        assert_eq!(
            status(cardinality_request(&[false, false], 1, 2)).await,
            SolveStatus::Unsat
        );
        let mut subset = cardinality_request(&[true, true], 1, 1);
        subset["facts"]["cardinality_constraints"]["record_count"]["rows"] = json!(["row_1"]);
        assert_eq!(status(subset).await, SolveStatus::Sat);
    }

    #[tokio::test]
    async fn cardinality_synthesizes_only_within_the_declared_bounds() {
        let mut input = cardinality_request(&[false, false], 1, 1);
        input["mode"] = json!("synthesize");
        for row in ["row_0", "row_1"] {
            input["facts"]["relations"]["records"]["rows"][row]["active"] = Value::Null;
        }
        input["unknowns"] = json!([
            {"kind": "row_active", "relation": "records", "row": "row_1"},
            {"kind": "row_active", "relation": "records", "row": "row_0"}
        ]);
        let compiled = compile(input).expect("bounded cardinality synthesis must compile");
        assert_eq!(
            compiled
                .projections
                .iter()
                .map(|projection| projection.field.as_str())
                .collect::<Vec<_>>(),
            ["rows.row_1.active", "rows.row_0.active"]
        );
        assert_eq!(
            SolverService::new()
                .solve_constraints(compiled.request)
                .await
                .expect("bounded cardinality synthesis must solve")
                .status,
            SolveStatus::Sat
        );
    }

    #[tokio::test]
    async fn value_range_is_inclusive_and_guarded_by_active_presence() {
        assert_eq!(
            status(value_range_request(true, true, Some(5))).await,
            SolveStatus::Sat
        );
        assert_eq!(
            status(value_range_request(true, true, Some(10))).await,
            SolveStatus::Sat
        );
        assert_eq!(
            status(value_range_request(false, true, Some(20))).await,
            SolveStatus::Sat
        );
        assert_eq!(
            status(value_range_request(true, false, None)).await,
            SolveStatus::Sat
        );
        assert_eq!(
            status(value_range_request(true, true, Some(11))).await,
            SolveStatus::Unsat
        );
    }

    #[tokio::test]
    async fn value_range_synthesizes_a_bounded_integer_value() {
        let mut input = value_range_request(true, true, Some(5));
        input["mode"] = json!("synthesize");
        input["facts"]["relations"]["measurements"]["rows"]["sample"]["cells"]["reading"]
            ["value"] = Value::Null;
        input["unknowns"] = json!([
            {"kind": "cell_value", "relation": "measurements", "row": "sample", "field": "reading"}
        ]);
        assert_eq!(status(input).await, SolveStatus::Sat);
    }

    #[test]
    fn compiled_constraint_names_include_binding_and_definition_subject() {
        let compiled = compile(unique_request(false)).expect("unique request must compile");
        let encoded = serde_json::to_value(compiled.request).expect("request must serialize");
        let id = encoded["constraints"][0]["id"]
            .as_str()
            .expect("compiled constraint ID");
        assert!(id.contains("rule_0_data_integrity_unique"));
        assert!(id.contains("record_key"));
        assert_eq!(compiled.rules[0].rule_id, "data_integrity.unique");
        assert_eq!(compiled.rules[0].binding_index, 0);
    }

    #[test]
    fn strict_request_rejects_unknown_fields() {
        let mut input = request(
            "verify",
            "data_integrity.unique",
            "record_key",
            one_cell_facts(
                json!({"kind": "boolean"}),
                json!(true),
                json!(true),
                json!(true),
            ),
        );
        input["unexpected"] = json!(true);
        assert!(compile(input)
            .expect_err("strict request")
            .contains("unknown field"));
    }

    #[test]
    fn verification_rejects_declared_snapshot_unknowns() {
        let facts = one_cell_facts(
            json!({"kind": "integer", "minimum": 0, "maximum": 9}),
            Value::Null,
            json!(true),
            json!(1),
        );
        let mut input = request("verify", "data_integrity.unique", "record_key", facts);
        input["unknowns"] = json!([{"kind": "row_active", "relation": "records", "row": "first"}]);
        let error = compile(input).expect_err("verification facts must be complete");
        assert!(error.contains("verification requires complete data integrity facts"));
    }

    #[test]
    fn synthesis_rejects_null_fact_without_matching_declaration() {
        let input = request(
            "synthesize",
            "data_integrity.unique",
            "record_key",
            one_cell_facts(
                json!({"kind": "boolean"}),
                Value::Null,
                json!(true),
                json!(true),
            ),
        );
        assert!(compile(input)
            .expect_err("missing declaration")
            .contains("requires a row_active unknown"));
    }

    #[test]
    fn enum_values_must_belong_to_the_declared_domain() {
        let input = request(
            "verify",
            "data_integrity.unique",
            "record_key",
            one_cell_facts(
                json!({"kind": "enum", "values": ["a", "b"]}),
                json!(true),
                json!(true),
                json!("c"),
            ),
        );
        assert!(compile(input)
            .expect_err("domain mismatch")
            .contains("outside enum domain"));
    }

    #[test]
    fn duplicate_unknown_targets_are_rejected() {
        let facts = one_cell_facts(
            json!({"kind": "boolean"}),
            Value::Null,
            json!(true),
            json!(true),
        );
        let mut input = request("synthesize", "data_integrity.unique", "record_key", facts);
        input["unknowns"] = json!([
            {"kind": "row_active", "relation": "records", "row": "first"},
            {"kind": "row_active", "relation": "records", "row": "first"}
        ]);
        assert!(compile(input)
            .expect_err("duplicate target")
            .contains("duplicate data integrity unknown"));
    }

    #[test]
    fn composite_foreign_keys_require_matching_field_domains() {
        let mut facts = empty_facts();
        facts["relations"] = json!({
            "children": {"fields": {"key": {"kind": "integer", "minimum": 0, "maximum": 9}}, "rows": {}},
            "parents": {"fields": {"key": {"kind": "boolean"}}, "rows": {}}
        });
        facts["foreign_keys"] = json!({
            "child_parent": {"child_relation": "children", "child_fields": ["key"], "parent_relation": "parents", "parent_fields": ["key"]}
        });
        let input = request(
            "verify",
            "data_integrity.foreign_key",
            "child_parent",
            facts,
        );
        assert!(compile(input)
            .expect_err("type mismatch")
            .contains("incompatible field domains"));
    }

    #[test]
    fn projections_are_unknown_only_and_caller_ordered() {
        let facts = one_cell_facts(
            json!({"kind": "integer", "minimum": 0, "maximum": 9}),
            Value::Null,
            Value::Null,
            Value::Null,
        );
        let mut input = request("synthesize", "data_integrity.unique", "record_key", facts);
        input["unknowns"] = json!([
            {"kind": "cell_value", "relation": "records", "row": "first", "field": "key"},
            {"kind": "row_active", "relation": "records", "row": "first"},
            {"kind": "cell_present", "relation": "records", "row": "first", "field": "key"}
        ]);
        let prepared = prepare(input).expect("valid synthesis snapshot");
        let fields = prepared
            .projections
            .iter()
            .map(|item| item.field.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            fields,
            [
                "rows.first.cells.key.value",
                "rows.first.active",
                "rows.first.cells.key.present"
            ]
        );
        assert_eq!(prepared.variables.len(), 3);
    }

    #[test]
    fn expression_budget_uses_checked_arithmetic() {
        let error = estimate_expression_nodes(usize::MAX, 2, 2)
            .expect_err("overflow must fail before lowering");
        assert!(error.contains("expression budget overflow"));
    }

    #[test]
    fn expression_budget_accepts_limit_and_rejects_first_excess() {
        assert_eq!(
            estimate_expression_nodes(256, 64, 1).expect("exact budget limit"),
            16_384
        );
        assert!(estimate_expression_nodes(257, 64, 1)
            .expect_err("first excess must fail")
            .contains("exceeds 16384"));
    }

    #[test]
    fn inverted_integer_domains_are_rejected() {
        let input = request(
            "verify",
            "data_integrity.unique",
            "record_key",
            one_cell_facts(
                json!({"kind": "integer", "minimum": 9, "maximum": 0}),
                json!(true),
                json!(true),
                json!(1),
            ),
        );
        assert!(compile(input)
            .expect_err("inverted bounds")
            .contains("minimum exceeds maximum"));
    }

    #[test]
    fn every_row_must_supply_exactly_the_declared_cells() {
        let mut facts = one_cell_facts(
            json!({"kind": "boolean"}),
            json!(true),
            json!(true),
            json!(true),
        );
        facts["relations"]["records"]["fields"]["other"] = json!({"kind": "boolean"});
        let input = request("verify", "data_integrity.unique", "record_key", facts);
        assert!(compile(input)
            .expect_err("missing cell")
            .contains("missing cell `other`"));
    }

    #[test]
    fn cardinality_bounds_must_be_nonnegative() {
        let mut facts = one_cell_facts(
            json!({"kind": "boolean"}),
            json!(true),
            json!(true),
            json!(true),
        );
        facts["cardinality_constraints"] = json!({
            "record_count": {"relation": "records", "minimum": -1, "maximum": 1}
        });
        let input = request(
            "verify",
            "data_integrity.cardinality",
            "record_count",
            facts,
        );
        assert!(compile(input)
            .expect_err("negative cardinality")
            .contains("must be nonnegative"));
    }
}

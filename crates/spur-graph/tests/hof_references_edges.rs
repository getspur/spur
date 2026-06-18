use spur_graph::{build_facts, GraphEdgeKind, RelationKind};

fn build_fixture(file_name: &str, src: &str) -> spur_graph::extract::GraphFacts {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(file_name), src).expect("write fixture");
    build_facts(dir.path(), None).expect("build facts").0
}

fn assert_hof_reference(facts: &spur_graph::extract::GraphFacts, target: &str) {
    assert!(
        hof_reference_labels(facts)
            .iter()
            .any(|label| label == target),
        "missing HOF reference to {target}; reference edges: {:?}",
        hof_reference_labels(facts)
    );
}

fn assert_no_hof_reference(facts: &spur_graph::extract::GraphFacts, target: &str) {
    assert!(
        !hof_reference_labels(facts)
            .iter()
            .any(|label| label == target),
        "unexpected HOF reference to {target}"
    );
}

fn hof_reference_labels(facts: &spur_graph::extract::GraphFacts) -> Vec<String> {
    facts
        .edges
        .iter()
        .filter(|edge| {
            edge.relation == RelationKind::References
                && edge.edge_kind == Some(GraphEdgeKind::ReferencesHof)
        })
        .filter_map(|edge| edge.target_label.clone())
        .collect()
}

#[test]
fn python_hof_calls_reference_bare_function_arguments() {
    let facts = build_fixture(
        "hof.py",
        r#"
from functools import reduce

def transform(value):
    return value + 1

def predicate(value):
    return value > 0

def combine(left, right):
    return left + right

def rank(value):
    return value.score

def factory():
    return predicate

def inline(value):
    return transform(value)

def run(values):
    list(map(transform, values))
    list(filter(predicate, values))
    reduce(combine, values)
    sorted(values, key=rank)
    min(values, key=rank)
    max(values, key=rank)
    values.sort(key=rank)
    list(map(lambda value: inline(value), values))
    list(map(factory(), values))
"#,
    );

    for target in ["transform", "predicate", "combine", "rank"] {
        assert_hof_reference(&facts, target);
    }
    assert_eq!(hof_reference_labels(&facts).len(), 4);
    assert_no_hof_reference(&facts, "inline");
    assert_no_hof_reference(&facts, "factory");
}

#[test]
fn cpp_hof_calls_reference_bare_function_arguments() {
    let facts = build_fixture(
        "hof.cpp",
        r#"
#include <algorithm>
#include <numeric>
#include <vector>

bool keep(int value) {
    return value > 0;
}

int bump(int value) {
    return value + 1;
}

int combine(int left, int right) {
    return left + right;
}

bool make_predicate() {
    return true;
}

bool lambda_only(int value) {
    return value > 1;
}

bool outside_std(int value) {
    return value > 2;
}

namespace predicates {
bool accept(int value) {
    return value > 0;
}
}

namespace custom {
template <typename It, typename Fn>
void for_each(It first, It last, Fn fn) {}
}

void run(std::vector<int>& values, std::vector<int>& out) {
    std::for_each(values.begin(), values.end(), keep);
    std::transform(values.begin(), values.end(), out.begin(), bump);
    std::sort(values.begin(), values.end(), predicates::accept);
    std::find_if(values.begin(), values.end(), keep);
    std::remove_if(values.begin(), values.end(), keep);
    std::count_if(values.begin(), values.end(), keep);
    std::accumulate(values.begin(), values.end(), 0, combine);
    std::for_each(values.begin(), values.end(), [](int value) { return lambda_only(value); });
    std::for_each(values.begin(), values.end(), make_predicate());
    custom::for_each(values.begin(), values.end(), outside_std);
}
"#,
    );

    for target in ["keep", "bump", "accept", "combine"] {
        assert_hof_reference(&facts, target);
    }
    assert_eq!(hof_reference_labels(&facts).len(), 4);
    assert_no_hof_reference(&facts, "lambda_only");
    assert_no_hof_reference(&facts, "make_predicate");
    assert_no_hof_reference(&facts, "outside_std");
}

#[test]
fn typescript_hof_calls_reference_bare_function_arguments() {
    let facts = build_fixture(
        "hof.ts",
        r#"
function renderItem(value: number): string {
    return String(value);
}

function isReady(value: number): boolean {
    return value > 0;
}

function track(value: number): void {
    void value;
}

function combine(left: number, right: number): number {
    return left + right;
}

function handleSuccess(value: number): number {
    return value;
}

function handleError(error: Error): void {
    void error;
}

function cleanup(): void {}

function callback(value: number): number {
    return value;
}

function render(value: number): string {
    return String(value);
}

function factory(): (value: number) => string {
    return renderItem;
}

function run(items: number[], promise: Promise<number>): void {
    items.map(renderItem);
    items.filter(isReady);
    items.forEach(track);
    items.reduce(combine, 0);
    promise.then(handleSuccess);
    promise.catch(handleError);
    promise.finally(cleanup);
    items.custom(callback);
    items.map((value) => render(value));
    items.map(factory());
}
"#,
    );

    for target in [
        "renderItem",
        "isReady",
        "track",
        "combine",
        "handleSuccess",
        "handleError",
        "cleanup",
    ] {
        assert_hof_reference(&facts, target);
    }
    assert_eq!(hof_reference_labels(&facts).len(), 7);
    assert_no_hof_reference(&facts, "callback");
    assert_no_hof_reference(&facts, "render");
    assert_no_hof_reference(&facts, "factory");
}

#[test]
fn tsx_hof_calls_reference_bare_function_arguments() {
    let facts = build_fixture(
        "hof.tsx",
        r#"
function renderItem(value: string) {
    return <span>{value}</span>;
}

function isReady(value: string): boolean {
    return value.length > 0;
}

export function List({ items }: { items: string[] }) {
    const rows = items.map(renderItem).filter(isReady);
    return <div>{rows}</div>;
}
"#,
    );

    assert_hof_reference(&facts, "renderItem");
    assert_hof_reference(&facts, "isReady");
    assert_eq!(hof_reference_labels(&facts).len(), 2);
}

#[test]
fn javascript_hof_calls_reference_bare_function_arguments() {
    let facts = build_fixture(
        "hof.js",
        r#"
function renderItem(value) {
    return String(value);
}

function isReady(value) {
    return value > 0;
}

function combine(left, right) {
    return left + right;
}

function handleSuccess(value) {
    return value;
}

function handleError(error) {
    return error;
}

function callback(value) {
    return value;
}

function render(value) {
    return String(value);
}

function run(items, promise) {
    items.map(renderItem);
    items.filter(isReady);
    items.reduce(combine, 0);
    promise.then(handleSuccess);
    promise.catch(handleError);
    items.custom(callback);
    items.map((value) => render(value));
}
"#,
    );

    for target in [
        "renderItem",
        "isReady",
        "combine",
        "handleSuccess",
        "handleError",
    ] {
        assert_hof_reference(&facts, target);
    }
    assert_eq!(hof_reference_labels(&facts).len(), 5);
    assert_no_hof_reference(&facts, "callback");
    assert_no_hof_reference(&facts, "render");
}

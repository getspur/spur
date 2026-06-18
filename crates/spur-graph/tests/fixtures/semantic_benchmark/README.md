# Semantic Benchmark Corpus

This corpus is a compact query-quality benchmark for `spur-graph`. Each
subdirectory is an intentionally crafted fixture for one language family. The
fixtures are small on purpose: they should make relation drift visible without
depending on compilers, package managers, network access, or large real-world
projects.

The integration test in `tests/semantic_benchmark.rs` records:

- exact node counts by `NodeKind`
- exact edge counts by `RelationKind`
- exact non-default edge counts by `GraphEdgeKind`
- representative must-have edges
- representative must-not-have edges

When query behavior intentionally changes, update this corpus in the same
change as the extractor/query update:

1. Keep snippets minimal and language-focused. Prefer one clear construct over
   broad real-world examples.
2. Run `scripts/spur-cargo test -p spur-graph semantic_benchmark_dump_current_counts -- --ignored --nocapture`
   to print current counts and edge summaries for all cases.
3. Update only the count arrays or edge assertions whose changed behavior is
   intentional.
4. Run `scripts/spur-cargo test -p spur-graph semantic_benchmark -- --nocapture`.
5. Run the relevant query or contract tests named in the task or PR.

Do not add performance thresholds here. Semantic correctness and performance
gates are deliberately separate.

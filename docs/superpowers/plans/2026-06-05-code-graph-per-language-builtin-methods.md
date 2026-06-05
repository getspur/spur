# Relocate builtin-method denylist to per-language registry (language-agnostic) Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source:** architectural review of the v8 `method_crate_singleton` recall + v9 language gate. The
precision guard `STD_PRELUDE_METHOD_NAMES` is a **Rust vocabulary hardcoded in the generic resolver**
(`extract/tree_sitter.rs`). After v9 it only guards Rust↔Rust binds, so a TypeScript `.forEach()`,
Python `.append()`, or C++ `.size()` calling a lone same-named workspace method **binds unprotected**.
SPUR's graph is language-agnostic S-P-O; language facts belong in the per-language registry, not the
engine — exactly as the `.scm` queries already do (generic matcher, per-language query data).

**Magnitude is small today** (live graph: 11 TS + 1 Python same-crate method-recall candidates, mostly
genuine domain methods; the few builtin-named ones overlap the Rust list by luck). **This is a
design-integrity + future-proofing change** (the TS notebook frontend is actively growing), not a
firefight. It does not chase exhaustiveness — it puts the heuristic in the right place and gives each
language its own list.

**The fix:** move builtin-method knowledge into `extract/languages.rs` as per-language `const`s,
exposed by a lightweight `Language::builtin_method_names(self) -> &'static [&'static str]`. The
resolver resolves the source file's `Language` via the existing `Language::from_path` and consults
that language's list. The generic resolver keeps **zero** hardcoded language vocabulary.

> **Why a method on `Language`, not a `LanguageConfig` field:** `Language::config()` constructs a
> fresh `LanguageConfig` (incl. tree-sitter `Language` handles) on every call — too heavy for the
> per-edge resolver path. A direct `match self { … }` returning a `&'static` slice is cheap.

**This stays a heuristic.** Even per-language denylists are a crutch for the closed-world assumption
(the graph can't see stdlib symbols). The end-state that retires the list — receiver-type resolution
(Tier-1 T1.d.2; Rust seed machinery `typed_bindings_by_scope`/`receiver_type_scope_text` already
exists) and external-symbol modeling (Tier-2) — is explicitly **out of scope** here.

📌 **Golden artifacts: expect ZERO change** (audited). The only `method_crate_singleton` binds in the
non-rust corpora are `normalized` (python), `boot` (ts), `Initialize` (cpp) — none are builtins in
their language, so all stay bound. The one name that would otherwise flip is python `send` (in the
Rust list, called as `client.send()` in python_corpus) — **kept excluded by adding `send`/`recv` to
the Python list**, so it stays rebind-resolved exactly as today. With those two names present, the
re-bless produces **no fixture diff at all**. If ANY golden changes, STOP and emit `risk` — it means
a language list is still missing a name in the Rust set (re-run the cross-list audit). (Rust corpus
unaffected — same list as v8.)

> **Why the first attempt escalated (read this):** the prior worker correctly self-escalated a
> `risk` signal because, without `send` in the Python list, the python_corpus `send` edge gained a
> `method_crate_singleton` stamp — a non-drops-only golden delta. That was the plan's gap, not the
> worker's error. The fix is the `send`/`recv` additions above; everything else in the prior diff
> (the 4 consts, the `Language::builtin_method_names` accessor, the resolver swap, the two tests,
> the v10 bump) was correct and should be reproduced.

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.

---

### Task per-lang-builtins: move the builtin-method denylist into the language registry

**Task ID:** `task-per-lang-builtins`

**Files (all in scope):**
- Modify: `crates/spur-graph/src/extract/languages.rs` (add per-language builtin `const`s + a
  `Language::builtin_method_names` accessor + a sorted-invariant unit test)
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` (delete `STD_PRELUDE_METHOD_NAMES`; the
  method arm consults `Language::builtin_method_names` via `Language::from_path` on the source file;
  add a behavioral TS test)
- Modify: `crates/spur-graph/src/store/build.rs` (`RESOLVER_VERSION` bump → v10)
- Regenerate (bless, only if changed): `crates/spur-graph/tests/fixtures/{python_corpus,typescript_corpus,cpp_corpus}/expected_graph_index.json` (rust unaffected)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `STD_PRELUDE_METHOD_NAMES` is **removed** from `tree_sitter.rs`. Its contents move verbatim to a
      `RUST_BUILTIN_METHODS: &[&str]` in `languages.rs` (must include `clone`, which the existing v8
      test relies on).
- [ ] New per-language `const`s in `languages.rs`, each **sorted ascending** (so `binary_search` is
      valid) with a doc-comment stating it is a precision heuristic, not an exhaustive builtin list:
  - `RUST_BUILTIN_METHODS` (the moved list)
  - `TS_BUILTIN_METHODS` — JS/TS Array/Object/String/Promise/Map/Set core, e.g. `forEach, map,
    filter, reduce, some, every, find, findIndex, includes, indexOf, slice, splice, concat, join,
    push, pop, shift, unshift, reverse, sort, flat, flatMap, keys, values, entries, has, get, set,
    add, delete, clear, then, catch, finally, toString, valueOf, hasOwnProperty, bind, call, apply,
    replace, replaceAll, split, trim, trimStart, trimEnd, padStart, padEnd, startsWith, endsWith,
    substring, charAt, toLowerCase, toUpperCase, match, repeat` (finalize + sort)
  - `PYTHON_BUILTIN_METHODS` — e.g. `append, extend, insert, remove, pop, index, count, sort,
    reverse, copy, clear, keys, values, items, get, update, setdefault, popitem, add, discard,
    union, intersection, join, split, rsplit, strip, lstrip, rstrip, replace, format, encode,
    decode, startswith, endswith, find, lower, upper, read, write, close, send, recv` (finalize +
    sort). **`send`/`recv` are required** — they are in the Rust list and are genuine Python
    generator/coroutine/socket methods; omitting them un-protects the python_corpus `client.send()`
    call (a known builtin name) and breaks the zero-golden-change expectation below.
  - `CPP_BUILTIN_METHODS` — e.g. `size, length, begin, end, cbegin, cend, rbegin, rend, push_back,
    pop_back, emplace_back, front, back, at, data, c_str, empty, clear, insert, erase, find, count,
    contains, reserve, resize, capacity, assign, swap, first, second, str, substr, append, compare`
    (finalize + sort)
  - markdown → empty slice.
- [ ] `Language::builtin_method_names(self) -> &'static [&'static str]` returns the per-language const
      (`TypeScript | Tsx => TS_BUILTIN_METHODS`; `Markdown => &[]`). It must NOT call `self.config()`.
- [ ] In `resolve_singleton_bare_target`'s Method arm, the builtin check becomes language-driven:
      resolve the **source** file's language with `Language::from_path(std::path::Path::new(src_file))`
      and test `lang.builtin_method_names().binary_search(&edge.target_name.as_str()).is_ok()`. When
      the source language is unknown (`from_path` → None), treat as **not** a builtin (the
      `function_singleton_safe` same-crate+language gate already governs safety). Keep the rest of the
      arm (the `same_crate_safe` gate, the `method_crate_singleton` stamp, the drop on failure)
      unchanged. (Post-v9 src and tgt share a language, so the source file's language is correct.)
- [ ] A `languages.rs` unit test asserts every builtin const is sorted and de-duplicated (guards the
      `binary_search` contract): for each list, `is_sorted()` and `windows(2).all(|w| w[0] != w[1])`.
- [ ] A behavioral `build_facts` test (in `tree_sitter.rs`, beside the v8 test) on a same-crate **TS**
      fixture proves per-language coverage: a `.ts` file calls a TS-builtin-named method (e.g.
      `x.forEach()`) whose only workspace definition is a TS method named `forEach` in the same crate
      → assert the `calls` edge is **unresolved**; and a sibling TS domain method (e.g. `setCellType`)
      → still resolves with `bind_method="method_crate_singleton"`. (Use `crates/foo/web/*.ts` paths.)
- [ ] The existing v8 test `method_crate_singleton_recovers_cross_module_same_crate` (Rust, `clone`
      negative) still passes unchanged.
- [ ] Goldens: **expect zero fixture diff** after re-bless (with `send`/`recv` in the Python list, per
      the audit above). If ANY golden changes → STOP and emit `risk` (a language list is missing a
      Rust-set name; re-run the cross-list audit before proceeding).
- [ ] `RESOLVER_VERSION` bumped to `"2026-06-05-per-language-builtins-v10"`.
- [ ] Full `-p spur-graph` suite green except flaky `incremental_ingest`; clippy `-D warnings` clean.
- [ ] **Report:** which goldens changed (and that each change is a drops-only builtin exclusion), the
      new tests pass, the v8 Rust test still passes, and v10 confirmation.

**Suggested Worker:** codex.

**Scope Boundary:** IN: relocating + generalizing the builtin denylist (the 4 consts + the
`Language::builtin_method_names` accessor + the resolver call-site swap + `RESOLVER_VERSION` + the two
tests + drops-only re-bless). OUT: receiver-type resolution, external-symbol modeling, the
`function_singleton_safe` language gate (v9, done), the rebind logic, the Function/References arms,
`path_scope`/`path_crate`, `LanguageConfig`'s existing fields and the `config()`/query machinery,
import/qualified/dyn paths, other relations, other crates, `schema.rs`. Do NOT expand a language's
list to chase exhaustiveness — a representative core is the goal.

**Implementation:**

- [ ] **Step 1: Failing test.** Add the behavioral TS test described above. Run
      `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib method_crate_singleton` → the
      `forEach`-exclusion assertion FAILS today (current Rust-only list doesn't contain `forEach`, so
      it binds).

- [ ] **Step 2: Add the per-language consts + accessor in `languages.rs`.** Move the Rust list, add
      TS/Python/C++ lists (sorted), add:

```rust
impl Language {
    /// Common builtin/stdlib method names per language family — a precision heuristic that keeps a
    /// same-crate singleton method bind from capturing a call whose real receiver is an external
    /// (stdlib/runtime) type. NOT exhaustive.
    pub(crate) fn builtin_method_names(self) -> &'static [&'static str] {
        match self {
            Self::Rust => RUST_BUILTIN_METHODS,
            Self::Python => PYTHON_BUILTIN_METHODS,
            Self::TypeScript | Self::Tsx => TS_BUILTIN_METHODS,
            Self::Cpp => CPP_BUILTIN_METHODS,
            Self::Markdown => &[],
        }
    }
}
```

- [ ] **Step 3: Swap the resolver call site in `tree_sitter.rs`.** Delete `STD_PRELUDE_METHOD_NAMES`;
      replace the `std_prelude_method` computation in the Method arm with:

```rust
let builtin_method = file_for_node(edge.source)
    .and_then(|src| crate::extract::languages::Language::from_path(std::path::Path::new(src)))
    .is_some_and(|lang| {
        lang.builtin_method_names()
            .binary_search(&edge.target_name.as_str())
            .is_ok()
    });
if same_crate_safe && !builtin_method {
    builder.add_pending_edge_with_bind_method(edge, Some(target), Some("method_crate_singleton"));
} else {
    builder.add_pending_edge(edge, None);
}
```

  Run Step 1 → expect PASS; confirm the v8 Rust `clone` test still passes.

- [ ] **Step 4: Bump `RESOLVER_VERSION`** (`build.rs:29`) → `"2026-06-05-per-language-builtins-v10"`.

- [ ] **Step 5: Re-bless goldens (drops-only on py/ts/cpp).**

```bash
SPUR_GRAPH_BLESS=1 SPUR_REMOTE=0 scripts/spur-cargo test -p spur-graph --test extractor
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor   # confirm green
```

  Verify the diff is only `calls` edges losing a `method_crate_singleton` target (→ null, bind_method
  removed) in python/typescript/cpp; rust unchanged. Anything else → STOP and emit `risk`.

- [ ] **Step 6: Broad gate + commit** (green except flaky `incremental_ingest`):

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor --test resolver \
  --test artifact_range_invariants --test calls_range_resolution_edges
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/extract/languages.rs crates/spur-graph/src/extract/tree_sitter.rs \
        crates/spur-graph/src/store/build.rs crates/spur-graph/tests/fixtures/
git commit -m "refactor(spur-graph): per-language builtin-method denylist in language registry"
```

  Report: goldens changed (drops-only confirmation), tests, v10.

## Self-Review
- **Coverage:** removes all hardcoded language vocabulary from the generic resolver; each language
  owns its builtin list in the registry that already hosts its `.scm` queries and kind maps —
  language-agnostic engine, per-language data.
- **Placeholder scan:** concrete consts + concrete accessor + concrete resolver swap + sorted-invariant
  test + behavioral TS test; bless conditional/drops-only.
- **Type consistency:** `Language`/`Language::from_path` are `pub`; `LanguageConfig` untouched; the new
  accessor returns `&'static [&'static str]`; `binary_search` requires the sorted-invariant test.
- **DAG:** single task.
- **Risk:** behavior-preserving for Rust (same list); adds honest precision for TS/Python/C++ (drops a
  handful of builtin over-binds — a correctness gain), drops-only golden churn on non-rust corpora with
  a `risk` off-ramp. The deeper receiver-type / external-symbol end-state that retires the heuristic is
  explicitly deferred.

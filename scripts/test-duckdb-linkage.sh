#!/usr/bin/env bash
# Cargo's resolved features, including dev dependencies, are the contract.
set -euo pipefail
cd "$(dirname "$0")/.."

probe_spur_context_duckdb() {
    local output status=0
    output=$(scripts/spur-cargo tree --locked --offline -p spur-context "$@" \
        -e features -i libduckdb-sys 2>&1) || status=$?
    if [[ $status -ne 0 ]]; then
        if [[ "$output" == *'package ID specification `libduckdb-sys` did not match any packages'* ]]; then
            SPUR_CONTEXT_DUCKDB_STATE=absent
            SPUR_CONTEXT_DUCKDB_FEATURES=
            return 0
        fi
        echo "$output" >&2
        return "$status"
    fi
    if [[ "$output" == *'libduckdb-sys v'* ]]; then
        SPUR_CONTEXT_DUCKDB_STATE=present
        SPUR_CONTEXT_DUCKDB_FEATURES="$output"
    else
        SPUR_CONTEXT_DUCKDB_STATE=absent
        SPUR_CONTEXT_DUCKDB_FEATURES=
    fi
}

assert_spur_context_duckdb_absent() {
    local route=$1
    shift
    probe_spur_context_duckdb "$@"
    if [[ "$SPUR_CONTEXT_DUCKDB_STATE" != absent ]]; then
        echo "FAIL: spur-context $route unexpectedly activates DuckDB" >&2
        exit 1
    fi
    echo "PASS: spur-context $route does not activate DuckDB"
}

assert_spur_context_duckdb_present() {
    local route=$1 expected_bundled=$2
    shift 2
    probe_spur_context_duckdb "$@"
    if [[ "$SPUR_CONTEXT_DUCKDB_STATE" != present ]]; then
        echo "FAIL: spur-context $route did not activate DuckDB" >&2
        exit 1
    fi
    local bundled=false
    if [[ "$SPUR_CONTEXT_DUCKDB_FEATURES" == *'libduckdb-sys feature "bundled"'* ]]; then
        bundled=true
    fi
    if [[ "$bundled" != "$expected_bundled" ]]; then
        echo "FAIL: spur-context $route bundled=$bundled, expected $expected_bundled" >&2
        exit 1
    fi
    echo "PASS: spur-context $route activates DuckDB with bundled=$bundled"
}

assert_spur_context_duckdb_absent "default features"
assert_spur_context_duckdb_absent "no default features" --no-default-features
assert_spur_context_duckdb_present "no default features plus duckdb" false \
    --no-default-features --features duckdb
assert_spur_context_duckdb_present "default features plus duckdb" true --features duckdb

for package in spur-analyst spur-core spur-cli spur-code-eval; do
    features=$(scripts/spur-cargo tree --locked --offline -p "$package" \
        --no-default-features -e features -i libduckdb-sys)
    if [[ "$features" == *'libduckdb-sys feature "bundled"'* ]]; then
        echo "FAIL: $package --no-default-features still compiles DuckDB" >&2
        exit 1
    fi
    features=$(scripts/spur-cargo tree --locked --offline -p "$package" \
        -e features -i libduckdb-sys)
    if [[ "$features" != *'libduckdb-sys feature "bundled"'* ]]; then
        echo "FAIL: $package default build lost bundled DuckDB" >&2
        exit 1
    fi
    echo "PASS: $package supports prebuilt and bundled linkage"
done

# The full CLI UI/analytics feature set must also work without source linking.
features=$(scripts/spur-cargo tree --locked --offline -p spur-cli \
    --no-default-features --features embed,tui-default,interactive-default,duckdb \
    -e features -i libduckdb-sys)
if [[ "$features" == *'libduckdb-sys feature "bundled"'* ]]; then
    echo "FAIL: CLI runtime features re-enable bundled DuckDB" >&2
    exit 1
fi
echo "PASS: full CLI runtime features support prebuilt linkage"

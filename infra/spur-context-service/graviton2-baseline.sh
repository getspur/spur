#!/usr/bin/env bash
# Shared CPU baseline for deployable spur-context-service arm64 artifacts.
# shellcheck shell=bash

SPUR_CONTEXT_GRAVITON2_RUST_TARGET_CPU="${SPUR_CONTEXT_GRAVITON2_RUST_TARGET_CPU:-neoverse-n1}"

if [[ "$SPUR_CONTEXT_GRAVITON2_RUST_TARGET_CPU" == "generic" ]]; then
    _SPUR_CONTEXT_GRAVITON2_DEFAULT_CFLAGS="-march=armv8-a+lse -O2"
else
    _SPUR_CONTEXT_GRAVITON2_DEFAULT_CFLAGS="-mcpu=neoverse-n1 -O2"
fi

# RUSTFLAGS overrides .cargo/config.toml, so keep the workspace frame-pointer
# policy explicit in the deploy baseline.
SPUR_CONTEXT_GRAVITON2_RUSTFLAGS="${SPUR_CONTEXT_GRAVITON2_RUSTFLAGS:--Ctarget-cpu=${SPUR_CONTEXT_GRAVITON2_RUST_TARGET_CPU} -Ctarget-feature=+lse -Cforce-frame-pointers=yes -Clinker=clang -Clink-arg=-fuse-ld=/mnt/cargo/rust-lld-driver/ld.lld}"
SPUR_CONTEXT_GRAVITON2_CFLAGS="${SPUR_CONTEXT_GRAVITON2_CFLAGS:-$_SPUR_CONTEXT_GRAVITON2_DEFAULT_CFLAGS}"
SPUR_CONTEXT_GRAVITON2_CXXFLAGS="${SPUR_CONTEXT_GRAVITON2_CXXFLAGS:-$SPUR_CONTEXT_GRAVITON2_CFLAGS}"

graviton2_baseline_log() {
    if declare -F log >/dev/null 2>&1; then
        log "$*"
    else
        echo "[graviton2-baseline] $*" >&2
    fi
}

graviton2_baseline_fail() {
    echo "[graviton2-baseline] $*" >&2
    return 1
}

rust_target_cpus_are_graviton2_safe() {
    local flags="$1"
    local found_target_cpu=0
    local next_is_codegen_option=0
    local word target_cpu

    for word in $flags; do
        if [[ "$next_is_codegen_option" -eq 1 ]]; then
            next_is_codegen_option=0
            if [[ "$word" == target-cpu=* ]]; then
                found_target_cpu=1
                target_cpu="${word#target-cpu=}"
                case "$target_cpu" in
                    neoverse-n1 | generic) ;;
                    *) return 1 ;;
                esac
            fi
            continue
        fi

        case "$word" in
            -C)
                next_is_codegen_option=1
                ;;
            -Ctarget-cpu=*)
                found_target_cpu=1
                target_cpu="${word#-Ctarget-cpu=}"
                case "$target_cpu" in
                    neoverse-n1 | generic) ;;
                    *) return 1 ;;
                esac
                ;;
        esac
    done

    [[ "$found_target_cpu" -eq 1 ]]
}

cflags_are_graviton2_safe() {
    local flags="$1"
    local found_cpu_or_arch=0
    local word

    for word in $flags; do
        case "$word" in
            -mcpu=* | -march=*)
                found_cpu_or_arch=1
                case "$word" in
                    -mcpu=neoverse-n1 | -march=armv8-a*) ;;
                    *) return 1 ;;
                esac
                ;;
        esac
    done

    [[ "$found_cpu_or_arch" -eq 1 ]]
}

assert_graviton2_safe_flags() {
    local artifact="$1"
    local rustflags="$2"
    local cflags="$3"
    local cxxflags="$4"
    local forbidden_cpu="neoverse-"
    forbidden_cpu+="v2"

    for flagset in "$rustflags" "$cflags" "$cxxflags"; do
        if [[ "$flagset" == *"$forbidden_cpu"* ]]; then
            graviton2_baseline_fail "$artifact uses a non-baseline CPU target"
            return 1
        fi
    done

    if ! rust_target_cpus_are_graviton2_safe "$rustflags"; then
        graviton2_baseline_fail "$artifact Rust flags must use an explicit Graviton2-safe target-cpu"
        return 1
    fi

    if ! cflags_are_graviton2_safe "$cflags"; then
        graviton2_baseline_fail "$artifact CFLAGS must use only neoverse-n1 or generic armv8-a"
        return 1
    fi
    if ! cflags_are_graviton2_safe "$cxxflags"; then
        graviton2_baseline_fail "$artifact CXXFLAGS must use only neoverse-n1 or generic armv8-a"
        return 1
    fi
}

run_graviton2_safe_cargo() {
    local artifact="$1"
    shift

    assert_graviton2_safe_flags \
        "$artifact" \
        "$SPUR_CONTEXT_GRAVITON2_RUSTFLAGS" \
        "$SPUR_CONTEXT_GRAVITON2_CFLAGS" \
        "$SPUR_CONTEXT_GRAVITON2_CXXFLAGS"

    graviton2_baseline_log "verified Graviton2-safe build flags for $artifact"
    AWS_RUSTFLAGS_DEFAULT="$SPUR_CONTEXT_GRAVITON2_RUSTFLAGS" \
    RUSTFLAGS="$SPUR_CONTEXT_GRAVITON2_RUSTFLAGS" \
    CFLAGS="$SPUR_CONTEXT_GRAVITON2_CFLAGS" \
    CXXFLAGS="$SPUR_CONTEXT_GRAVITON2_CXXFLAGS" \
        scripts/spur-cargo "$@"
}

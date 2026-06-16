#!/usr/bin/env bash
# INV-SDK-F1: sdk/fixtures/port-store must be byte-for-byte identical to the
# standalone notebook repo's fixtures/port-store Rust golden fixtures.
#
# The Rust PortStore writer and the SDK language readers pin the same golden
# fixtures so that a wire-format change forces an explicit, atomic update on
# both sides. If you changed the Rust side, regenerate the SDK copy:
#
#   SPUR_NOTEBOOK_REPO=/path/to/spur-notebook scripts/check-sdk-fixture-lockstep.sh
#   cp -R /path/to/spur-notebook/fixtures/port-store/. sdk/fixtures/port-store/
#
# Then update any SDK reader tests that reference the affected fields.
# If you changed the SDK copy directly, sync back to the Rust side and update
# the Rust round-trip test in getspur/spur-notebook.
set -euo pipefail

if [[ -z "${SPUR_NOTEBOOK_REPO:-}" ]]; then
  echo "INV-SDK-F1 ERROR: SPUR_NOTEBOOK_REPO is required after the notebook repo split."
  echo "Set it to a local getspur/spur-notebook checkout."
  exit 2
fi

RUST_DIR="$SPUR_NOTEBOOK_REPO/fixtures/port-store"
SDK_DIR="sdk/fixtures/port-store"

if [[ ! -d "$RUST_DIR" ]]; then
  echo "INV-SDK-F1 ERROR: Rust fixture directory not found: $RUST_DIR"
  echo "SPUR_NOTEBOOK_REPO must point to a getspur/spur-notebook checkout."
  exit 1
fi

if [[ ! -d "$SDK_DIR" ]]; then
  echo "INV-SDK-F1 ERROR: SDK fixture directory not found: $SDK_DIR"
  echo "This script must be run from the spur repo root."
  echo "Run: cp -R \"$RUST_DIR/.\" \"$SDK_DIR/\""
  exit 1
fi

DIFF_OUTPUT=$(diff -r "$RUST_DIR" "$SDK_DIR" 2>&1) && DIFF_EXIT=0 || DIFF_EXIT=$?

if [[ $DIFF_EXIT -eq 2 ]]; then
  echo "INV-SDK-F1 ERROR: diff failed unexpectedly"
  echo "$DIFF_OUTPUT"
  exit 1
fi

if [[ $DIFF_EXIT -ne 0 ]]; then
  echo "INV-SDK-F1 FAIL: sdk/fixtures/port-store is out of sync with $RUST_DIR"
  echo ""
  echo "Diff:"
  echo "$DIFF_OUTPUT"
  echo ""
  echo "Fix: cp -R \"$RUST_DIR/.\" \"$SDK_DIR/\""
  echo "Then update SDK reader tests and commit both sides together."
  exit 1
fi

echo "INV-SDK-F1: OK"

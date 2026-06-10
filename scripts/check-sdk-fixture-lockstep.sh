#!/usr/bin/env bash
# INV-SDK-F1: sdk/fixtures/port-store must be byte-for-byte identical to
# crates/spur-notebook/fixtures/port-store (the Rust PortStore golden fixtures).
#
# The Rust PortStore writer and the SDK language readers pin the same golden
# fixtures so that a wire-format change forces an explicit, atomic update on
# both sides.  If you changed the Rust side, regenerate the SDK copy:
#
#   cp -R crates/spur-notebook/fixtures/port-store/. sdk/fixtures/port-store/
#
# Then update any SDK reader tests that reference the affected fields.
# If you changed the SDK copy directly, sync back to the Rust side and update
# the Rust round-trip test in crates/spur-notebook/tests/.
set -euo pipefail

RUST_DIR="crates/spur-notebook/fixtures/port-store"
SDK_DIR="sdk/fixtures/port-store"

if [[ ! -d "$RUST_DIR" ]]; then
  echo "INV-SDK-F1 ERROR: Rust fixture directory not found: $RUST_DIR"
  echo "This script must be run from the spur repo root."
  exit 1
fi

if [[ ! -d "$SDK_DIR" ]]; then
  echo "INV-SDK-F1 ERROR: SDK fixture directory not found: $SDK_DIR"
  echo "This script must be run from the spur repo root."
  echo "Run: cp -R $RUST_DIR/. $SDK_DIR/"
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
  echo "Fix: cp -R $RUST_DIR/. $SDK_DIR/"
  echo "Then update SDK reader tests and commit both sides together."
  exit 1
fi

echo "INV-SDK-F1: OK"

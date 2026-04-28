#!/usr/bin/env bash
# Sign crates/spur-license/resources/default_policy.json with the Ed25519
# private key at $SPUR_POLICY_SIGNING_KEY (PEM).
#
# Output: rewrites default_policy.json with a SignedPolicy wrapper containing
# the canonical-JSON payload, base64 Ed25519 signature, and key_id.

set -euo pipefail

if [[ -z "${SPUR_POLICY_SIGNING_KEY:-}" ]]; then
  echo "error: SPUR_POLICY_SIGNING_KEY env var must point to the Ed25519 private key (PEM)" >&2
  exit 1
fi

KEY_ID="${SPUR_POLICY_KEY_ID:-spur-policy-2026-04}"
RESOURCES_DIR="$(cd "$(dirname "$0")/.." && pwd)/resources"
POLICY_FILE="$RESOURCES_DIR/default_policy.json"
TMP_PAYLOAD="$(mktemp)"
TMP_SIG="$(mktemp)"
trap 'rm -f "$TMP_PAYLOAD" "$TMP_SIG"' EXIT

# Detect whether the file already has a SignedPolicy wrapper or is a raw
# PolicyDocument. If wrapped, extract .payload; otherwise treat the whole
# file as the payload.
#
# IMPORTANT: jq always emits a trailing newline. The Rust verifier
# (build.rs + at runtime) calls `signed.payload.as_bytes()` which does
# NOT include any trailing newline, so we strip it here. Without this
# strip the signed bytes would differ by one byte from the verified
# bytes and ed25519-dalek would reject the signature.
if jq -e '.payload and .signature and .key_id' "$POLICY_FILE" >/dev/null 2>&1; then
  jq -r '.payload' "$POLICY_FILE" | tr -d '\n' > "$TMP_PAYLOAD"
else
  jq -c . "$POLICY_FILE" | tr -d '\n' > "$TMP_PAYLOAD"
fi

# Sign the canonical payload bytes.
openssl pkeyutl -sign -inkey "$SPUR_POLICY_SIGNING_KEY" -rawin -in "$TMP_PAYLOAD" -out "$TMP_SIG"
SIG_B64="$(base64 < "$TMP_SIG" | tr -d '\n')"
PAYLOAD_STR="$(cat "$TMP_PAYLOAD")"

jq -n \
  --arg payload "$PAYLOAD_STR" \
  --arg signature "$SIG_B64" \
  --arg key_id "$KEY_ID" \
  '{payload: $payload, signature: $signature, key_id: $key_id}' \
  > "$POLICY_FILE"

echo "Signed $POLICY_FILE with key_id=$KEY_ID"

#!/usr/bin/env bash
# Fails if vendor-specific tokens appear in spur-tui.
# Normalization happens in spur-acp adapters; spur-tui must consume only
# normalized types. See docs/spur/acp-meta-conventions.md.
set -euo pipefail

# Pipe-separated alternations; tokens matched with grep -E.
VENDOR_TOKENS='"_meta"|claudeCode|parentToolUseId|toolResponse|terminal_info'
TARGET='crates/spur-tui/src/'
ALLOWLIST_MARKER='allow-vendor-read'

if [[ ! -d "$TARGET" ]]; then
    echo "ERROR: target directory '$TARGET' not found." >&2
    echo "This script must be run from the spur repo root." >&2
    exit 1
fi

# Find matches, then drop lines carrying the allowlist marker.
MATCHES=$(grep -rnE "$VENDOR_TOKENS" "$TARGET" || true)
MATCHES=$(printf '%s\n' "$MATCHES" | grep -v "$ALLOWLIST_MARKER" || true)

if [ -n "$MATCHES" ]; then
    echo "ERROR: vendor-specific tokens found in $TARGET:" >&2
    echo "$MATCHES" >&2
    echo "" >&2
    echo "Vendor _meta access must go through spur_acp::adapter." >&2
    echo "If this read is intentional, add '// allow-vendor-read' on the line." >&2
    echo "See docs/spur/acp-meta-conventions.md." >&2
    exit 1
fi

echo "OK: no vendor-specific tokens in $TARGET"

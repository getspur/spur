# Port-Store Wire Contract

The port store is the data bus between a Spur notebook's compute graph and
its app plugins. The host writes versioned port files; the app plugin (Python
server) and frontend cells (TypeScript) read them.

## Directory layout

```
~/.spur/notebooks/<blake3-id>/
  ports/
    manifest.json           # authoritative index; atomically rewritten on update
    <port-name>@v<N>.arrow  # Arrow IPC file
    <port-name>@v<N>.media  # binary media file (WebM, MP4, etc.)
```

The root is resolved as `notebook_port_root(entry_notebook_path)/ports`.
`notebook_port_root` maps to `~/.spur/notebooks/<blake3-id>` (verified at
`jute-notebook/src-tauri/src/ports.rs:18`).

## manifest.json shape

```json
{
  "ports": {
    "<name>": {
      "path": "<port>@v<N>.<ext>",
      "version": <int>,
      "kind": "arrow" | "media",
      "mime": "<str>",          // optional; set for media ports
      "size": <int>,            // optional; byte count
      "schema": { ... },        // optional; Arrow schema for arrow ports
      "duration_sec": <float>   // optional; duration for media ports
    }
  }
}
```

All fields except `path`, `version`, and `kind` are optional. Old manifests
without `duration_sec` default to `null`/`None` in both SDKs — this is an
additive, backward-compatible extension.

### Critical consumer rule

**Consumers MUST read `entry["path"]` and basename-join it under the root
directory. Never derive `root/<port-name>` yourself.**

The versioned filename is `<port>@v<N>.<ext>`. If you derive the path from the
port name alone (e.g. `root/spur-ad-capture`) you miss the `@vN` suffix and get
a file-not-found error. This was the root cause of `render.py:369` in the
html_video Phase-1 review (2026-06-10).

Python implementation (correct):
```python
file_path = root / Path(entry["path"]).name   # basename join
```

TypeScript implementation (correct):
```ts
const file_path = join(root, basename(entry.path));
```

## Injected env vars

| Env var | Provisioned when | Value |
|---------|-----------------|-------|
| `SPUR_PORTS_ROOT` | `capabilities.ports` declared | `~/.spur/notebooks/<id>/ports` |
| `SPUR_NOTEBOOK_PORT_ROOT` | Always (kernel env) | `~/.spur/notebooks/<id>` |
| `SPUR_ARTIFACTS_DIR` | `capabilities.artifacts_dir` declared | `~/.spur/notebooks/<id>/artifacts` |

Note: `SPUR_PORTS_ROOT` (Python server) already includes the `ports/` suffix.
`SPUR_NOTEBOOK_PORT_ROOT` (kernel env, used by the TypeScript SDK) does not —
the TypeScript SDK appends `ports/manifest.json` itself.

## Golden fixtures

The golden fixtures pin the wire format that the Rust `PortStore` writer produces.

Location in this repo: `sdk/fixtures/port-store/`
Rust source: `crates/spur-notebook/fixtures/port-store/`

The two directories are byte-for-byte identical (enforced by
`scripts/check-sdk-fixture-lockstep.sh`, CI invariant `INV-SDK-F1`).

Contents:
- `manifest.json` — a manifest with an arrow port (`sales`) and a media port
  (`spur-ad-capture`, WebM, 60s, 10 bytes)
- `spur-ad-capture@v1.media` — 10 bytes of placeholder media data

```json
{
  "ports": {
    "sales": {
      "path": "sales@v1.arrow",
      "version": 1,
      "kind": "arrow",
      "schema": { "fields": [...], "metadata": {} }
    },
    "spur-ad-capture": {
      "path": "spur-ad-capture@v1.media",
      "version": 1,
      "kind": "media",
      "mime": "video/webm",
      "size": 10,
      "duration_sec": 60.0
    }
  }
}
```

Note: the `sales` entry has no data file in the fixtures (it's manifest-only),
so `PortStore.read("sales")` will raise `PortFileNotFoundError`. The fixture
tests parse-only for that entry.

## Lockstep invariant (INV-SDK-F1)

A wire-format change in the Rust `PortStore` writer that does not also update
`sdk/fixtures/port-store/` fails CI. The enforcement is:

```sh
scripts/check-sdk-fixture-lockstep.sh
```

**If you change the Rust port-store format:**
1. Run `cp -R crates/spur-notebook/fixtures/port-store/. sdk/fixtures/port-store/`
2. Update any SDK reader tests that reference the changed fields.
3. Commit both sides together.

**If you change the SDK fixture copy directly:**
1. Sync back: `cp -R sdk/fixtures/port-store/. crates/spur-notebook/fixtures/port-store/`
2. Update the Rust round-trip test in `crates/spur-notebook/tests/`.

## duration_sec

`duration_sec` is an optional float on media entries. It represents the capture
duration in seconds. The Rust writer populates it from `push_capture_port_for_state`
(which receives the already-validated duration from the Tauri command). SDK
readers expose it as:
- Python: `PortRead.duration_sec: float | None`
- TypeScript: `PortData.durationSec?: number`

When absent (older manifests), both SDKs default to `None`/`undefined`.

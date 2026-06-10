# Python SDK — spur_app

Source: `sdk/python/src/spur_app/`
Published: PyPI `spur-app` (post-U7; not yet released). Until then, install from the repo with `uv add --dev sdk/python/` or `pip install -e sdk/python/`.

Dependencies: `mcp` (for `app.run()`); stdlib-only for `ports`, `artifacts`, `env`, `testing`.

## App

`from spur_app import App`

```python
app = App("html-video")   # name must match spur-app.json "name"

@app.tool()
def my_tool(x: int) -> str:
    return str(x)

if __name__ == "__main__":
    app.run()
```

`App.__init__(name: str)` configures stderr logging and lazily initialises
capability properties. The `mcp` package is only imported on first call to
`app.tool()` or `app.run()`, so the rest of the SDK works in test environments
where `mcp` is not installed.

### App.tool()

```python
@app.tool()
def my_tool(arg: str) -> dict:
    ...
```

Decorator. Delegates to `FastMCP.tool()`. The decorated function is registered
as an MCP tool with its parameter names and type annotations as the schema.

### App.run()

```python
app.run()
```

Starts the MCP server on the stdio transport. Call from `__main__`. All
`spur_app` log output goes to stderr (never stdout, which owns the JSON-RPC
wire).

### App.ports

```python
frame = app.ports.read("spur-ad-capture")
# frame: PortRead
```

Lazy + cached `PortStore`. Raises `MissingCapabilityError` on first access if
`SPUR_PORTS_ROOT` is not set. Requires `capabilities.ports` declared in manifest.

### App.artifacts

```python
out_path = app.artifacts.path("renders/output.mp4")
# Returns a Path; parent dirs are created
```

Lazy + cached `ArtifactStore`. Raises `MissingCapabilityError` on first access
if `SPUR_ARTIFACTS_DIR` is not set. Requires `capabilities.artifacts_dir`.

### App.env

```python
tmpl_dir = app.env.path("TEMPLATES_DIR")    # Path; raises if absent
raw = app.env.get("OPTIONAL_VAR", "default") # str | None
val = app.env.require("REQUIRED_VAR")        # str; raises if absent
```

Typed accessors for manifest-declared env vars (the `mcp_server.env` block).

## PortStore

`from spur_app.ports import PortStore, PortRead`

```python
store = PortStore()                        # reads SPUR_PORTS_ROOT
store = PortStore(root="/abs/path")        # explicit root (for tests)

frame: PortRead = store.read("port-name")
names: list[str] = store.list()
```

`PortStore.read(name: str) -> PortRead` re-parses `manifest.json` on every call
(the host atomically rewrites it on each port update; caching would return stale
data).

### PortRead

```python
@dataclass(frozen=True)
class PortRead:
    bytes: bytes            # raw file contents
    mime: str | None        # MIME type (None for arrow ports)
    version: int            # version counter
    kind: str               # "arrow" or "media"
    duration_sec: float | None  # seconds for media ports; None otherwise
    path: Path              # resolved filesystem path that was read
```

**Never derive the path yourself.** `PortStore.read` always basename-joins
`entry["path"]` under the root directory. This is the only correct implementation
of the port-store contract (the `@vN` absolute-path bug is impossible-by-construction).

## ArtifactStore

`from spur_app.artifacts import ArtifactStore`

```python
store = ArtifactStore()                  # reads SPUR_ARTIFACTS_DIR
store = ArtifactStore(root="/abs/path")  # explicit root (for tests)

p: Path = store.path("renders/out.mp4")  # parent dirs created
root: Path = store.root                  # the artifacts directory
```

`ArtifactStore.path(relative: str) -> Path` resolves relative paths under the
artifacts root and creates parent directories. Raises `ArtifactPathError` for
absolute paths or `..` escapes.

## EnvAccessor

`from spur_app.env import EnvAccessor`

```python
env = EnvAccessor()
val: str | None = env.get("VAR", "default")
val: str        = env.require("VAR")   # raises EnvVarRequiredError if absent
p:   Path       = env.path("VAR")      # require + Path(...)
```

## Error types

`from spur_app.errors import ...`

All errors derive from `SpurAppError`.

| Class | Raised when |
|-------|-------------|
| `MissingCapabilityError(capability, message)` | `SPUR_PORTS_ROOT` or `SPUR_ARTIFACTS_DIR` not set |
| `PortNotFoundError(port, available)` | Named port absent from manifest |
| `PortFileNotFoundError(port, path)` | Manifest entry exists but file is missing |
| `PortManifestError(manifest_path, reason)` | `manifest.json` missing or invalid JSON |
| `EnvVarRequiredError(name)` | `env.require(name)` called when var is absent |
| `ArtifactPathError(message)` | Absolute path or `..` escape in `artifacts.path()` |

## Testing — FakePortStore

`from spur_app.testing import FakePortStore, fake_port_store`

```python
# Programmatic store
with FakePortStore() as store:
    store.add_media("clip", b"fake", mime="video/mp4", duration_sec=5.0)
    store.add_arrow("data", b"ipc-bytes")
    frame = store.port_store.read("clip")
    assert frame.duration_sec == 5.0
# SPUR_PORTS_ROOT is restored after the context

# Load from golden fixtures — depth depends on the test file's location.
# sdk/python/tests/test_*.py uses parents[3] (test → python → sdk → repo root).
# An in-app test at my-app/tests/test_*.py inside the monorepo uses parents[2].
from pathlib import Path
FIXTURES = Path(__file__).resolve().parents[3] / "sdk" / "fixtures" / "port-store"

with FakePortStore.from_fixtures(FIXTURES) as store:
    frame = store.port_store.read("spur-ad-capture")
    assert frame.mime == "video/webm"
    assert frame.duration_sec == 60.0

# pytest fixture (re-export in conftest.py)
from spur_app.testing import fake_port_store  # noqa: F401

def test_my_tool(fake_port_store):
    fake_port_store.add_media("clip", b"data", mime="video/mp4")
    result = fake_port_store.port_store.read("clip")
    assert result.mime == "video/mp4"
```

`FakePortStore` sets `SPUR_PORTS_ROOT` to a temp directory for the duration of
the context and restores it on exit. `fake_port_store` is a `@pytest.fixture`
that wraps an empty `FakePortStore` per test.

### FakePortStore methods

| Method | Description |
|--------|-------------|
| `add_media(name, data, *, mime, duration_sec, version)` | Add a media port entry |
| `add_arrow(name, ipc_bytes, *, schema, version)` | Add an arrow port entry |
| `FakePortStore.from_fixtures(dir)` | Factory: mirror a fixture directory |
| `.port_store` (property) | `PortStore` rooted at the temp dir (only inside context) |
| `.root` (property) | The temp directory Path (only inside context) |
| `.env()` | Returns `{"SPUR_PORTS_ROOT": ...}` for subprocess injection |

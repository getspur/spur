# Notebook daemon control protocol

`spur-notebook` runs as a foreground Tauri app and keeps one Unix socket alive
for each brain session while the notebook app is running:

```text
~/.spur/notebooks/sessions/<nonce>.sock
```

The nonce is minted before the brain process is spawned, so the MCP stdio config
can point at a socket before the ACP session id exists. The socket is shared by
daemon control messages and notebook MCP traffic. Every socket frame is:

```text
u32_be_json_length || json_bytes
```

Frames larger than 16 MiB are rejected.

## Installation on macOS

Use the workspace install task:

```text
cargo xtask install
```

On macOS this installs the CLI binary to `$CARGO_HOME/bin/spur` and builds the
Tauri app bundle at `~/Applications/Jute.app`. The MCP proxy lazily launches the
bundled binary directly when its per-session socket is not already accepting
connections:

```text
~/Applications/Jute.app/Contents/MacOS/Jute --socket ~/.spur/notebooks/sessions/<nonce>.sock
```

Launching from inside the `.app` gives AppKit and WKWebView the bundle context
and `CFBundleIdentifier` needed for the webview to render. Raw
`spur-notebook` binaries remain a fallback for old installs and development
workflows, but the macOS install path is the app bundle.

## Multiplexing

The daemon reads the first frame on each connection.

If the first frame has `"daemon": "notebook.v1"`, it is handled as a daemon
control request and the daemon writes one control response before closing the
connection.

Any other first frame is treated as the first MCP JSON-RPC message for the
notebook MCP server. Subsequent frames on that connection remain MCP frames.

## Control requests

```json
{
  "daemon": "notebook.v1",
  "command": "open",
  "path": "/absolute/or/relative/file.ipynb"
}
```

Commands:

- `open`: save the current notebook if one is loaded, then load `path` and show
  a Tauri window.
- `new`: create a scratch notebook under `~/.spur/scratch/<uuid>.ipynb`, load it,
  and show a Tauri window.
- `reopen`: show the current notebook window. If no notebook is loaded, the
  response is an error.
- `close`: save the current notebook if one is loaded, close the window, and
  clear daemon notebook state.
- `shutdown`: save the current notebook if one is loaded, then exit the daemon.

`open` requires `path`; the other commands ignore `path`.

The daemon records the last successfully loaded notebook at:

```text
~/.spur/notebooks/last.json
```

On daemon startup, that file is read and the notebook is restored if the path
still exists. `close` clears the record. This is intentionally a single
notebook pointer; multi-window restore is deferred.

## Control responses

Success:

```json
{
  "ok": true,
  "path": "/path/to/current.ipynb"
}
```

Failure:

```json
{
  "ok": false,
  "error": {
    "code": "notebook_not_open",
    "message": "No notebook is loaded"
  }
}
```

The response `path` is present when a notebook remains loaded after the command.

## MCP proxy

Brain sessions preconfigure the notebook MCP server as a stdio process:

```text
~/Applications/Jute.app/Contents/MacOS/Jute --mcp-proxy ~/.spur/notebooks/sessions/<nonce>.sock
```

The proxy launches `spur-notebook --socket <sock>` on first use when the socket
is missing or refusing connections, waits for it to become connectable, then
translates newline-delimited stdio JSON-RPC messages into the daemon's
length-prefixed socket frames and writes daemon responses back to stdout as
newline-delimited JSON.

Claude Code and other MCP-stdio clients use the same proxy path with their own
socket path argument:

```text
~/Applications/Jute.app/Contents/MacOS/Jute --mcp-proxy <sock>
```

The brain MCP config is stable for the whole session. Opening, creating, closing,
or reopening notebooks changes daemon state only; it does not restart the brain
or rewrite the session MCP config.

When no notebook is loaded, `notebook.*` tools return an MCP error with
`data.code = "notebook_not_open"`.

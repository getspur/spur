# Notebook daemon control protocol

`spur-notebook --headless` runs the notebook daemon without creating an initial
Tauri window. It keeps one stable Unix socket alive:

```text
~/.spur/notebooks/control.sock
```

The socket is shared by daemon control messages and notebook MCP traffic. Every
socket frame is:

```text
u32_be_json_length || json_bytes
```

Frames larger than 16 MiB are rejected.

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
spur-notebook --mcp-proxy ~/.spur/notebooks/control.sock
```

The proxy translates newline-delimited stdio JSON-RPC messages into the daemon's
length-prefixed socket frames and writes daemon responses back to stdout as
newline-delimited JSON.

The brain MCP config is stable for the whole session. Opening, creating, closing,
or reopening notebooks changes daemon state only; it does not restart the brain
or rewrite the session MCP config.

When no notebook is loaded, `notebook.*` tools return an MCP error with
`data.code = "notebook_not_open"`.

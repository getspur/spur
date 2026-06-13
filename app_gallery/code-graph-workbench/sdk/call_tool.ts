/**
 * `callTool` — connects to the notebook MCP server via a Unix socket,
 * performs the MCP initialize handshake, calls `tools/call`, and returns
 * the unwrapped result.
 *
 * Wire contract:
 *   1. Connect to the Unix socket at `SPUR_NOTEBOOK_MCP_SOCKET`.
 *   2. Send `initialize` request (id=1).
 *   3. Read the `initialize` response; throw if it carries a JSON-RPC error.
 *   4. Send `notifications/initialized` notification (no id).
 *   5. Send `tools/call` request (id=2).
 *   6. Read the `tools/call` response and unwrap structured content.
 *
 * Structured-content unwrapping priority:
 *   - `result.structuredContent`  (camelCase MCP 2025-11-25 field)
 *   - `result.structured_content` (snake_case alias used by some hosts)
 *   - `result.content[].text` parsed as JSON, or `{ text }` if not JSON
 *   - `result` as-is if none of the above match
 */

import { readFrame, writeFrame } from "./wire.ts";

/** Options for `callTool` and `callToolWithSocket`. */
export interface CallToolOptions {
  /**
   * Override for the `clientInfo` sent in the `initialize` handshake.
   * Defaults to `{ name: "spur-app", version: "0.1.0" }`.
   */
  clientInfo?: { name: string; version: string };
}

/**
 * Minimal seam used by tests to inject a fake connection factory instead of
 * opening a real Unix socket. The factory receives the socket path and must
 * return an object compatible with `Deno.Conn`.
 *
 * @internal — only accepted by `callToolWithSocket`, not by `callTool`.
 */
export type ConnectFn = (socketPath: string) => Promise<Deno.Conn>;

/** Default connect implementation — opens a real Unix socket. */
async function defaultConnect(socketPath: string): Promise<Deno.Conn> {
  return await Deno.connect({ transport: "unix", path: socketPath });
}

/** Unwrap a JSON-RPC error field into a thrown Error. */
function throwIfRpcError(response: Record<string, unknown>, context: string) {
  if (response.error !== undefined) {
    const err = response.error as Record<string, unknown>;
    throw new Error(
      typeof err.message === "string"
        ? err.message
        : `${context} error: ${JSON.stringify(response.error)}`,
    );
  }
}

/**
 * Call a notebook MCP tool by name with the given arguments.
 * Reads `SPUR_NOTEBOOK_MCP_SOCKET` from the environment.
 *
 * @param name    MCP tool name (e.g. `"html_video_render"`)
 * @param args    Tool arguments object
 * @param options Optional overrides (clientInfo)
 */
export async function callTool(
  name: string,
  args: Record<string, unknown>,
  options?: CallToolOptions,
): Promise<unknown> {
  const socketPath = Deno.env.get("SPUR_NOTEBOOK_MCP_SOCKET");
  if (!socketPath) {
    throw new Error("SPUR_NOTEBOOK_MCP_SOCKET is not set");
  }
  return await callToolWithSocket(socketPath, name, args, options);
}

/**
 * Like `callTool` but accepts an explicit socket path instead of reading the
 * environment variable. Used internally and in tests.
 *
 * @param socketPath Unix socket path to connect to
 * @param name       MCP tool name
 * @param args       Tool arguments object
 * @param options    Optional overrides; accepts `_connectFn` for test injection
 *                   (@internal — not part of the public API surface)
 */
export async function callToolWithSocket(
  socketPath: string,
  name: string,
  args: Record<string, unknown>,
  options?: CallToolOptions & {
    /** @internal Test seam — inject a fake connect function. */
    _connectFn?: ConnectFn;
  },
): Promise<unknown> {
  const clientInfo = options?.clientInfo ?? {
    name: "spur-app",
    version: "0.1.0",
  };
  const connect: ConnectFn = options?._connectFn ?? defaultConnect;
  const conn = await connect(socketPath);
  let id = 1;
  try {
    // Step 1: send initialize request
    await writeFrame(conn, {
      jsonrpc: "2.0",
      id: id++,
      method: "initialize",
      params: {
        protocolVersion: "2025-11-25",
        capabilities: {},
        clientInfo,
      },
    });

    // Step 2: read initialize response — throw if the server reports an error
    const initResponse = await readFrame(conn) as Record<string, unknown>;
    throwIfRpcError(initResponse, "initialize");

    // Step 3: send initialized notification (no id — it's a notification)
    await writeFrame(conn, {
      jsonrpc: "2.0",
      method: "notifications/initialized",
      params: {},
    });

    // Step 4: send tools/call request (id=2)
    await writeFrame(conn, {
      jsonrpc: "2.0",
      id: id++,
      method: "tools/call",
      params: { name, arguments: args },
    });

    // Step 5: read tools/call response
    const response = await readFrame(conn) as Record<string, unknown>;
    throwIfRpcError(response, "tools/call");

    const result = (response.result ?? {}) as Record<string, unknown>;

    // Unwrap structured content — three branches in priority order:
    // 1. camelCase field (MCP 2025-11-25 spec)
    if (result.structuredContent !== undefined) return result.structuredContent;
    // 2. snake_case alias used by some hosts
    if (result.structured_content !== undefined) {
      return result.structured_content;
    }
    // 3. text content item — try JSON parse, fall back to { text }
    const contentItems = result.content as
      | Array<Record<string, unknown>>
      | undefined;
    const text = contentItems?.find?.((item) => item.type === "text")?.text;
    if (typeof text === "string") {
      try {
        return JSON.parse(text);
      } catch (_) {
        return { text };
      }
    }

    return result;
  } finally {
    try {
      conn.close();
    } catch (_) {
      // ignore close errors
    }
  }
}

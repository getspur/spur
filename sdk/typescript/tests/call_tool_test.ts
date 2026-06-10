/**
 * Tests for callTool / callToolWithSocket in src/call_tool.ts.
 *
 * All tests spin up a fake MCP server over a real Unix socket in a temp dir
 * so they exercise the actual framing code without needing a live notebook.
 */

import {
  assertEquals,
  assertRejects,
} from "https://deno.land/std@0.224.0/assert/mod.ts";
import { callToolWithSocket } from "../src/call_tool.ts";
import { readFrame, writeFrame } from "../src/wire.ts";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Start a fake MCP server at a temp Unix socket path. */
async function makeFakeServer(
  socketPath: string,
  handler: (conn: Deno.Conn) => Promise<void>,
): Promise<{ done: Promise<void>; close: () => void }> {
  const listener = Deno.listen({ transport: "unix", path: socketPath });
  const done = (async () => {
    try {
      const conn = await listener.accept();
      await handler(conn);
      conn.close();
    } finally {
      listener.close();
    }
  })();
  return { done, close: () => listener.close() };
}

/**
 * Canonical fake MCP server: assert handshake order, then respond with
 * the provided `toolsCallResponse` to the tools/call request.
 */
async function canonicalServer(
  conn: Deno.Conn,
  toolsCallResponse: Record<string, unknown>,
  receivedMessages: unknown[],
) {
  // 1. Read initialize request
  const initReq = await readFrame(conn);
  receivedMessages.push(initReq);

  // 2. Send initialize response
  const r = initReq as Record<string, unknown>;
  await writeFrame(conn, {
    jsonrpc: "2.0",
    id: r.id,
    result: {
      protocolVersion: "2025-11-25",
      capabilities: {},
      serverInfo: { name: "fake-mcp", version: "0" },
    },
  });

  // 3. Read notifications/initialized (no id — it's a notification)
  const notif = await readFrame(conn);
  receivedMessages.push(notif);

  // 4. Read tools/call request
  const toolsCall = await readFrame(conn);
  receivedMessages.push(toolsCall);

  // 5. Send tools/call response
  const tc = toolsCall as Record<string, unknown>;
  await writeFrame(conn, {
    jsonrpc: "2.0",
    id: tc.id,
    ...toolsCallResponse,
  });
}

async function tempSocketPath(): Promise<string> {
  const f = await Deno.makeTempFile({ suffix: ".sock" });
  await Deno.remove(f);
  return f;
}

// ---------------------------------------------------------------------------
// Handshake order
// ---------------------------------------------------------------------------

Deno.test(
  "callTool sends initialize → notifications/initialized → tools/call in order",
  { permissions: { net: true, read: true, write: true, env: true } },
  async () => {
    const socketPath = await tempSocketPath();
    const received: unknown[] = [];

    const { done } = await makeFakeServer(socketPath, (conn) =>
      canonicalServer(
        conn,
        {
          result: {
            structuredContent: { rendered: true },
          },
        },
        received,
      ));

    await callToolWithSocket(socketPath, "test_tool", { arg: "val" });
    await done;

    assertEquals(received.length, 3);
    const [initReq, notif, toolsCall] = received as Array<
      Record<string, unknown>
    >;

    // initialize request
    assertEquals(initReq.method, "initialize");
    assertEquals(typeof initReq.id, "number");

    // notifications/initialized — must NOT have an id
    assertEquals(notif.method, "notifications/initialized");
    assertEquals(notif.id, undefined);

    // tools/call
    assertEquals(toolsCall.method, "tools/call");
    assertEquals(typeof toolsCall.id, "number");
    const params = toolsCall.params as Record<string, unknown>;
    assertEquals(params.name, "test_tool");
    assertEquals(params.arguments, { arg: "val" });
  },
);

// ---------------------------------------------------------------------------
// clientInfo override
// ---------------------------------------------------------------------------

Deno.test(
  "callTool sends default clientInfo when none provided",
  { permissions: { net: true, read: true, write: true, env: true } },
  async () => {
    const socketPath = await tempSocketPath();
    const received: unknown[] = [];

    const { done } = await makeFakeServer(socketPath, (conn) =>
      canonicalServer(
        conn,
        { result: { structuredContent: {} } },
        received,
      ));

    await callToolWithSocket(socketPath, "t", {});
    await done;

    const initReq = received[0] as Record<string, unknown>;
    const params = initReq.params as Record<string, unknown>;
    const clientInfo = params.clientInfo as Record<string, unknown>;
    assertEquals(clientInfo.name, "spur-app");
    assertEquals(clientInfo.version, "0.1.0");
  },
);

Deno.test(
  "callTool uses provided clientInfo override",
  { permissions: { net: true, read: true, write: true, env: true } },
  async () => {
    const socketPath = await tempSocketPath();
    const received: unknown[] = [];

    const { done } = await makeFakeServer(socketPath, (conn) =>
      canonicalServer(
        conn,
        { result: { structuredContent: {} } },
        received,
      ));

    await callToolWithSocket(socketPath, "t", {}, {
      clientInfo: { name: "my-app", version: "9.9" },
    });
    await done;

    const initReq = received[0] as Record<string, unknown>;
    const params = initReq.params as Record<string, unknown>;
    const clientInfo = params.clientInfo as Record<string, unknown>;
    assertEquals(clientInfo.name, "my-app");
    assertEquals(clientInfo.version, "9.9");
  },
);

// ---------------------------------------------------------------------------
// Structured-content unwrapping — all three branches
// ---------------------------------------------------------------------------

Deno.test(
  "callTool unwraps structuredContent (camelCase)",
  { permissions: { net: true, read: true, write: true, env: true } },
  async () => {
    const socketPath = await tempSocketPath();

    const { done } = await makeFakeServer(socketPath, (conn) =>
      canonicalServer(
        conn,
        {
          result: {
            structuredContent: { answer: 42 },
          },
        },
        [],
      ));

    const result = await callToolWithSocket(socketPath, "t", {});
    await done;
    assertEquals(result, { answer: 42 });
  },
);

Deno.test(
  "callTool unwraps structured_content (snake_case alias)",
  { permissions: { net: true, read: true, write: true, env: true } },
  async () => {
    const socketPath = await tempSocketPath();

    const { done } = await makeFakeServer(socketPath, (conn) =>
      canonicalServer(
        conn,
        {
          result: {
            structured_content: { answer: 99 },
          },
        },
        [],
      ));

    const result = await callToolWithSocket(socketPath, "t", {});
    await done;
    assertEquals(result, { answer: 99 });
  },
);

Deno.test(
  "callTool unwraps text-content JSON parse branch",
  { permissions: { net: true, read: true, write: true, env: true } },
  async () => {
    const socketPath = await tempSocketPath();

    const { done } = await makeFakeServer(socketPath, (conn) =>
      canonicalServer(
        conn,
        {
          result: {
            content: [
              { type: "text", text: '{"parsed":true,"x":7}' },
            ],
          },
        },
        [],
      ));

    const result = await callToolWithSocket(socketPath, "t", {});
    await done;
    assertEquals(result, { parsed: true, x: 7 });
  },
);

Deno.test(
  "callTool wraps non-JSON text in { text } object",
  { permissions: { net: true, read: true, write: true, env: true } },
  async () => {
    const socketPath = await tempSocketPath();

    const { done } = await makeFakeServer(socketPath, (conn) =>
      canonicalServer(
        conn,
        {
          result: {
            content: [
              { type: "text", text: "plain text not json" },
            ],
          },
        },
        [],
      ));

    const result = await callToolWithSocket(socketPath, "t", {});
    await done;
    assertEquals(result, { text: "plain text not json" });
  },
);

// ---------------------------------------------------------------------------
// Error propagation — tools/call errors
// ---------------------------------------------------------------------------

Deno.test(
  "callTool throws on JSON-RPC error response",
  { permissions: { net: true, read: true, write: true, env: true } },
  async () => {
    const socketPath = await tempSocketPath();

    const { done } = await makeFakeServer(socketPath, (conn) =>
      canonicalServer(
        conn,
        {
          error: { code: -32602, message: "Invalid params: missing arg" },
        },
        [],
      ));

    await assertRejects(
      () => callToolWithSocket(socketPath, "t", {}),
      Error,
      "Invalid params: missing arg",
    );
    await done;
  },
);

Deno.test(
  "callTool throws on error without message field",
  { permissions: { net: true, read: true, write: true, env: true } },
  async () => {
    const socketPath = await tempSocketPath();

    const { done } = await makeFakeServer(socketPath, (conn) =>
      canonicalServer(
        conn,
        { error: { code: -32603 } },
        [],
      ));

    await assertRejects(
      () => callToolWithSocket(socketPath, "t", {}),
      Error,
    );
    await done;
  },
);

// ---------------------------------------------------------------------------
// Error propagation — initialize errors
// ---------------------------------------------------------------------------

Deno.test(
  "callTool throws when initialize response contains a JSON-RPC error",
  { permissions: { net: true, read: true, write: true, env: true } },
  async () => {
    const socketPath = await tempSocketPath();

    // Server that replies to initialize with an error and then hangs up
    const { done } = await makeFakeServer(socketPath, async (conn) => {
      const initReq = await readFrame(conn) as Record<string, unknown>;
      await writeFrame(conn, {
        jsonrpc: "2.0",
        id: initReq.id,
        error: { code: -32600, message: "protocol version not supported" },
      });
      // Close immediately — no further messages expected
    });

    await assertRejects(
      () => callToolWithSocket(socketPath, "t", {}),
      Error,
      "protocol version not supported",
    );
    await done;
  },
);

// ---------------------------------------------------------------------------
// Missing environment variable
// ---------------------------------------------------------------------------

Deno.test(
  "callTool throws when SPUR_NOTEBOOK_MCP_SOCKET is not set",
  { permissions: { env: true } },
  async () => {
    // Save and remove env var
    const saved = Deno.env.get("SPUR_NOTEBOOK_MCP_SOCKET");
    Deno.env.delete("SPUR_NOTEBOOK_MCP_SOCKET");
    try {
      const { callTool } = await import("../src/call_tool.ts");
      await assertRejects(
        () => callTool("t", {}),
        Error,
        "SPUR_NOTEBOOK_MCP_SOCKET is not set",
      );
    } finally {
      if (saved !== undefined) Deno.env.set("SPUR_NOTEBOOK_MCP_SOCKET", saved);
    }
  },
);

// ---------------------------------------------------------------------------
// _connectFn seam on callToolWithSocket (internal test injection)
// ---------------------------------------------------------------------------

Deno.test(
  "callToolWithSocket _connectFn seam receives the socket path",
  { permissions: { net: true, read: true, write: true, env: true } },
  async () => {
    const socketPath = await tempSocketPath();
    const received: unknown[] = [];
    const { done } = await makeFakeServer(socketPath, (conn) =>
      canonicalServer(
        conn,
        { result: { structuredContent: { ok: true } } },
        received,
      ));

    let capturedPath: string | undefined;
    const { callToolWithSocket: cwsLocal } = await import(
      "../src/call_tool.ts"
    );
    await cwsLocal(socketPath, "t", {}, {
      _connectFn: async (path: string) => {
        capturedPath = path;
        return await Deno.connect({ transport: "unix", path });
      },
    });
    await done;
    assertEquals(capturedPath, socketPath);
  },
);

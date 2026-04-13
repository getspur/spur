#!/usr/bin/env node
// Stub ACP agent that errors on session/load — used by
// load_session_error_propagation regression test.

process.stdin.setEncoding("utf8");
let buffer = "";

process.stdin.on("data", (chunk) => {
  buffer += chunk;
  let nl;
  while ((nl = buffer.indexOf("\n")) >= 0) {
    const line = buffer.slice(0, nl).trim();
    buffer = buffer.slice(nl + 1);
    if (!line) continue;
    let req;
    try { req = JSON.parse(line); } catch { continue; }
    if (req.method === "initialize") {
      process.stdout.write(JSON.stringify({
        jsonrpc: "2.0",
        id: req.id,
        result: {
          protocolVersion: 1,
          agentCapabilities: { loadSession: true, promptCapabilities: {} },
          authMethods: [],
        },
      }) + "\n");
    } else if (req.method === "session/load") {
      process.stdout.write(JSON.stringify({
        jsonrpc: "2.0",
        id: req.id,
        error: {
          code: -32002,
          message: "Resource not found: " + (req.params?.sessionId ?? ""),
          data: { uri: req.params?.sessionId ?? "" },
        },
      }) + "\n");
    }
    // Ignore everything else.
  }
});
process.stdin.on("end", () => process.exit(0));

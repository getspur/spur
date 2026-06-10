/**
 * Tests for the low-level framing primitives in src/wire.ts.
 */

import { assertEquals, assertRejects } from "@std/assert";
import { readExactly, readFrame, writeFrame } from "../src/wire.ts";

// ---------------------------------------------------------------------------
// readExactly
// ---------------------------------------------------------------------------

Deno.test("readExactly reads the exact number of bytes in one shot", async () => {
  const data = new Uint8Array([1, 2, 3, 4, 5]);
  let offset = 0;
  const fakeReader = {
    read(buf: Uint8Array): Promise<number | null> {
      if (offset >= data.length) return Promise.resolve(null);
      const n = Math.min(buf.length, data.length - offset);
      buf.set(data.subarray(offset, offset + n));
      offset += n;
      return Promise.resolve(n);
    },
  };
  const result = await readExactly(fakeReader, 5);
  assertEquals(result, data);
});

Deno.test("readExactly handles chunked reads", async () => {
  const data = new Uint8Array([10, 20, 30, 40]);
  let offset = 0;
  const fakeReader = {
    read(buf: Uint8Array): Promise<number | null> {
      if (offset >= data.length) return Promise.resolve(null);
      // deliver one byte at a time to stress the loop
      buf[0] = data[offset++];
      return Promise.resolve(1);
    },
  };
  const result = await readExactly(fakeReader, 4);
  assertEquals(result, data);
});

Deno.test("readExactly throws when connection closes early", async () => {
  const fakeReader = {
    read(_buf: Uint8Array): Promise<number | null> {
      return Promise.resolve(null);
    },
  };
  await assertRejects(
    () => readExactly(fakeReader, 4),
    Error,
    "notebook MCP socket closed",
  );
});

// ---------------------------------------------------------------------------
// round-trip: writeFrame → readFrame via a Unix socket pair
// ---------------------------------------------------------------------------

Deno.test(
  "writeFrame / readFrame round-trip via Unix socket",
  { permissions: { net: true, read: true, write: true } },
  async () => {
    const socketPath = await Deno.makeTempFile({ suffix: ".sock" });
    await Deno.remove(socketPath);

    const listener = Deno.listen({ transport: "unix", path: socketPath });

    // Server: accept one connection, send a frame, close
    const serverDone = (async () => {
      const conn = await listener.accept();
      await writeFrame(conn, { hello: "world", n: 42 });
      conn.close();
      listener.close();
    })();

    // Client: connect, read the frame
    const clientConn = await Deno.connect({
      transport: "unix",
      path: socketPath,
    });
    const received = await readFrame(clientConn);
    clientConn.close();

    await serverDone;
    assertEquals(received, { hello: "world", n: 42 });
  },
);

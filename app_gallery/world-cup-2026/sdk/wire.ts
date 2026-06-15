/**
 * Low-level framing primitives for the 4-byte big-endian length-prefixed
 * JSON-RPC protocol spoken over Unix sockets.
 */

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/**
 * Read exactly `size` bytes from `reader`, blocking until all bytes arrive.
 * Throws if the connection closes before `size` bytes are available.
 */
export async function readExactly(
  reader: { read(p: Uint8Array): Promise<number | null> },
  size: number,
): Promise<Uint8Array> {
  const buffer = new Uint8Array(size);
  let offset = 0;
  while (offset < size) {
    const n = await reader.read(buffer.subarray(offset));
    if (n === null) throw new Error("notebook MCP socket closed");
    offset += n;
  }
  return buffer;
}

/**
 * Write all bytes of `data` to `writer`, looping until every byte is sent.
 * `Deno.Conn.write` may return before all bytes are written (partial write);
 * this function retries until the full buffer is flushed.
 */
async function writeAll(
  writer: { write(p: Uint8Array): Promise<number> },
  data: Uint8Array,
): Promise<void> {
  let offset = 0;
  while (offset < data.length) {
    const n = await writer.write(data.subarray(offset));
    offset += n;
  }
}

/**
 * Read one length-framed JSON frame from `conn`.
 * Frame layout: 4-byte big-endian uint32 length followed by that many UTF-8 bytes.
 */
export async function readFrame(conn: Deno.Conn): Promise<unknown> {
  const header = await readExactly(conn, 4);
  const length = new DataView(
    header.buffer,
    header.byteOffset,
    header.byteLength,
  ).getUint32(0, false);
  return JSON.parse(decoder.decode(await readExactly(conn, length)));
}

/**
 * Write one length-framed JSON frame to `conn`.
 * Frame layout: 4-byte big-endian uint32 length followed by the JSON bytes.
 * Loops internally to guard against partial writes.
 */
export async function writeFrame(
  conn: Deno.Conn,
  value: unknown,
): Promise<void> {
  const payload = encoder.encode(JSON.stringify(value));
  const frame = new Uint8Array(4 + payload.length);
  new DataView(frame.buffer).setUint32(0, payload.length, false);
  frame.set(payload, 4);
  await writeAll(conn, frame);
}

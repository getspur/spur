/**
 * Tests for ports helpers in src/ports.ts.
 *
 * All tests write a temporary port-store directory (manifest + sample files)
 * and pass the root path explicitly to avoid depending on the env var during
 * testing (though env-var injection is also tested).
 */

import {
  assertEquals,
  assertExists,
  assertRejects,
  assertStringIncludes,
} from "https://deno.land/std@0.224.0/assert/mod.ts";
import { join } from "https://deno.land/std@0.224.0/path/mod.ts";
import { ports } from "../src/ports.ts";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/** Create a minimal port-store fixture in a temp directory. */
async function createFixture(options?: {
  extraPorts?: Record<
    string,
    {
      kind?: "arrow" | "media";
      mime?: string;
      content?: Uint8Array;
      duration_sec?: number;
    }
  >;
}): Promise<{ root: string; cleanup: () => Promise<void> }> {
  const root = await Deno.makeTempDir({ prefix: "spur_ports_test_" });
  const portsDir = join(root, "ports");
  await Deno.mkdir(portsDir, { recursive: true });

  // Write sample media file
  const sampleBytes = new Uint8Array([0xde, 0xad, 0xbe, 0xef, 0x01, 0x02]);
  await Deno.writeFile(join(portsDir, "sample@v1.media"), sampleBytes);

  // Build manifest
  const manifestPorts: Record<string, unknown> = {
    "my-port": {
      path: join(portsDir, "sample@v1.media"),
      version: 1,
      kind: "media",
      mime: "video/webm",
      size: sampleBytes.length,
      duration_sec: 3.5,
    },
  };

  if (options?.extraPorts) {
    for (const [name, cfg] of Object.entries(options.extraPorts)) {
      const filename = `${name}@v1.${cfg.kind ?? "media"}`;
      const content = cfg.content ?? new Uint8Array([0xaa, 0xbb]);
      await Deno.writeFile(join(portsDir, filename), content);
      manifestPorts[name] = {
        path: join(portsDir, filename),
        version: 1,
        kind: cfg.kind ?? "media",
        mime: cfg.mime,
        duration_sec: cfg.duration_sec,
      };
    }
  }

  await Deno.writeTextFile(
    join(portsDir, "manifest.json"),
    JSON.stringify({ ports: manifestPorts }, null, 2),
  );

  return {
    root,
    cleanup: () => Deno.remove(root, { recursive: true }),
  };
}

// ---------------------------------------------------------------------------
// ports.manifest
// ---------------------------------------------------------------------------

Deno.test(
  "ports.manifest parses the manifest file",
  { permissions: { read: true, write: true, env: true } },
  async () => {
    const { root, cleanup } = await createFixture();
    try {
      const m = await ports.manifest(root);
      assertExists(m.ports);
      assertExists(m.ports["my-port"]);
      assertEquals(m.ports["my-port"].version, 1);
      assertEquals(m.ports["my-port"].kind, "media");
      assertEquals(m.ports["my-port"].mime, "video/webm");
    } finally {
      await cleanup();
    }
  },
);

Deno.test(
  "ports.manifest uses SPUR_NOTEBOOK_PORT_ROOT env var when no root given",
  { permissions: { read: true, write: true, env: true } },
  async () => {
    const { root, cleanup } = await createFixture();
    Deno.env.set("SPUR_NOTEBOOK_PORT_ROOT", root);
    try {
      const m = await ports.manifest();
      assertExists(m.ports["my-port"]);
    } finally {
      Deno.env.delete("SPUR_NOTEBOOK_PORT_ROOT");
      await cleanup();
    }
  },
);

Deno.test(
  "ports.manifest throws when neither root nor env var is set",
  { permissions: { env: true } },
  async () => {
    Deno.env.delete("SPUR_NOTEBOOK_PORT_ROOT");
    await assertRejects(
      () => ports.manifest(),
      Error,
      "SPUR_NOTEBOOK_PORT_ROOT",
    );
  },
);

// ---------------------------------------------------------------------------
// ports.read
// ---------------------------------------------------------------------------

Deno.test(
  "ports.read returns bytes for a known port",
  { permissions: { read: true, write: true, env: true } },
  async () => {
    const { root, cleanup } = await createFixture();
    try {
      const data = await ports.read("my-port", root);
      assertEquals(
        data.bytes,
        new Uint8Array([0xde, 0xad, 0xbe, 0xef, 0x01, 0x02]),
      );
    } finally {
      await cleanup();
    }
  },
);

Deno.test(
  "ports.read returns correct metadata",
  { permissions: { read: true, write: true, env: true } },
  async () => {
    const { root, cleanup } = await createFixture();
    try {
      const data = await ports.read("my-port", root);
      assertEquals(data.version, 1);
      assertEquals(data.kind, "media");
      assertEquals(data.mime, "video/webm");
      assertEquals(data.durationSec, 3.5);
    } finally {
      await cleanup();
    }
  },
);

Deno.test(
  "ports.read throws a clear error naming the missing port",
  { permissions: { read: true, write: true, env: true } },
  async () => {
    const { root, cleanup } = await createFixture();
    try {
      await assertRejects(
        () => ports.read("nonexistent-port", root),
        Error,
        "nonexistent-port",
      );
    } finally {
      await cleanup();
    }
  },
);

Deno.test(
  "ports.read uses SPUR_NOTEBOOK_PORT_ROOT env var when no root given",
  { permissions: { read: true, write: true, env: true } },
  async () => {
    const { root, cleanup } = await createFixture();
    Deno.env.set("SPUR_NOTEBOOK_PORT_ROOT", root);
    try {
      const data = await ports.read("my-port");
      assertExists(data.bytes);
      assertEquals(data.kind, "media");
    } finally {
      Deno.env.delete("SPUR_NOTEBOOK_PORT_ROOT");
      await cleanup();
    }
  },
);

Deno.test(
  "ports.read performs basename-join safety (ignores directory prefix in path)",
  { permissions: { read: true, write: true, env: true } },
  async () => {
    // Create a fixture where path field points to a deeply nested fake dir,
    // but the actual file exists at the basename under portsDir.
    const { root, cleanup } = await createFixture({
      extraPorts: {
        "safe-port": {
          kind: "media",
          mime: "application/octet-stream",
          content: new Uint8Array([0xff, 0xfe]),
        },
      },
    });
    try {
      // Tamper the manifest to have a tricky path prefix
      const portsDir = join(root, "ports");
      const manifestPath = join(portsDir, "manifest.json");
      const raw = JSON.parse(await Deno.readTextFile(manifestPath));
      // Replace the path with one that has a different directory prefix
      raw.ports["safe-port"].path = "/tmp/somewhere/else/safe-port@v1.media";
      await Deno.writeTextFile(manifestPath, JSON.stringify(raw));

      // The file is still at the correct basename location under portsDir
      // ports.read should still find it via basename-join
      const data = await ports.read("safe-port", root);
      assertEquals(data.bytes, new Uint8Array([0xff, 0xfe]));
    } finally {
      await cleanup();
    }
  },
);

Deno.test(
  "ports.read handles absent mime and durationSec gracefully",
  { permissions: { read: true, write: true, env: true } },
  async () => {
    const { root, cleanup } = await createFixture({
      extraPorts: {
        "bare-port": { kind: "arrow" },
      },
    });
    try {
      const data = await ports.read("bare-port", root);
      assertEquals(data.mime, undefined);
      assertEquals(data.durationSec, undefined);
      assertEquals(data.kind, "arrow");
    } finally {
      await cleanup();
    }
  },
);

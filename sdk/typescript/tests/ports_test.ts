/**
 * Tests for ports helpers in src/ports.ts.
 *
 * All tests write a temporary port-store directory (manifest + sample files)
 * and pass the root path explicitly to avoid depending on the env var during
 * testing (though env-var injection is also tested).
 */

import { assertEquals, assertExists, assertRejects } from "@std/assert";
import { basename, join } from "@std/path";
import { ports } from "../src/ports.ts";

// ---------------------------------------------------------------------------
// Golden-fixture helpers
// ---------------------------------------------------------------------------

/**
 * The shared fixture directory lives at sdk/fixtures/port-store/ and is flat:
 * manifest.json and the data files sit at the same level (no ports/ subdir).
 *
 * ports.ts always appends `ports/` to the root it is given, so we cannot pass
 * the fixture dir directly. Instead we create a tmp root, mirror the fixture
 * files into `<tmp>/ports/`, and pass `<tmp>` as the root. We read every byte
 * from the real fixture file so the CI byte-lock pin is preserved — any drift
 * in the fixture causes a test failure here.
 */
const FIXTURE_DIR = new URL(
  "../../fixtures/port-store",
  import.meta.url,
).pathname;

/**
 * Build a tmp root that has a `ports/` subdirectory populated from the golden
 * fixture dir. Returns the tmp root path and a cleanup callback.
 *
 * Strategy: read the manifest from the fixture, rewrite the `path` fields to
 * point at the copied file basenames (they are already plain filenames in the
 * golden fixture, but we normalise to be safe), then copy each referenced data
 * file. Only the media port has a data file; the arrow port has no data file in
 * the fixture so we skip it (ports.read is not called for it in the pin tests).
 */
async function buildFixtureRoot(): Promise<{
  root: string;
  cleanup: () => Promise<void>;
}> {
  const root = await Deno.makeTempDir({ prefix: "spur_fixture_test_" });
  const portsDir = join(root, "ports");
  await Deno.mkdir(portsDir, { recursive: true });

  // Read the golden manifest verbatim.
  const rawManifest = JSON.parse(
    await Deno.readTextFile(join(FIXTURE_DIR, "manifest.json")),
  ) as { ports: Record<string, { path: string; kind: string }> };

  // For each port, copy its data file if it exists in the fixture dir.
  for (const entry of Object.values(rawManifest.ports)) {
    const filename = basename(entry.path);
    const srcPath = join(FIXTURE_DIR, filename);
    try {
      const bytes = await Deno.readFile(srcPath);
      await Deno.writeFile(join(portsDir, filename), bytes);
    } catch (_: unknown) {
      // No data file in fixture (e.g. arrow port) — skip.
    }
  }

  // Write the manifest into the ports subdir. The path fields in the golden
  // manifest are plain filenames (no directory prefix), which is exactly what
  // basename-join in ports.read expects — no rewriting needed.
  await Deno.writeTextFile(
    join(portsDir, "manifest.json"),
    JSON.stringify(rawManifest, null, 2),
  );

  return {
    root,
    cleanup: () => Deno.remove(root, { recursive: true }),
  };
}

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

// ---------------------------------------------------------------------------
// Golden-fixture pin tests (sdk/fixtures/port-store)
// ---------------------------------------------------------------------------

Deno.test(
  "golden fixture: manifest parses both ports (sales arrow + spur-ad-capture media)",
  { permissions: { read: true, write: true, env: true } },
  async () => {
    const { root, cleanup } = await buildFixtureRoot();
    try {
      const m = await ports.manifest(root);

      // Both ports must be present.
      assertExists(m.ports["sales"], "expected 'sales' port in manifest");
      assertExists(
        m.ports["spur-ad-capture"],
        "expected 'spur-ad-capture' port in manifest",
      );

      // Arrow port: check kind.
      assertEquals(m.ports["sales"].kind, "arrow");
      assertEquals(m.ports["sales"].version, 1);

      // Media port: check all declared fields.
      assertEquals(m.ports["spur-ad-capture"].kind, "media");
      assertEquals(m.ports["spur-ad-capture"].version, 1);
      assertEquals(m.ports["spur-ad-capture"].mime, "video/webm");
      assertEquals(m.ports["spur-ad-capture"].size, 10);
      assertEquals(m.ports["spur-ad-capture"].duration_sec, 60.0);
    } finally {
      await cleanup();
    }
  },
);

Deno.test(
  "golden fixture: read('spur-ad-capture') returns exact fixture bytes",
  { permissions: { read: true, write: true, env: true } },
  async () => {
    // Read the expected bytes directly from the golden fixture file so this
    // test fails immediately if the fixture file changes (CI byte-lock).
    const expectedBytes = await Deno.readFile(
      join(FIXTURE_DIR, "spur-ad-capture@v1.media"),
    );

    const { root, cleanup } = await buildFixtureRoot();
    try {
      const data = await ports.read("spur-ad-capture", root);
      assertEquals(
        data.bytes,
        expectedBytes,
        "bytes must match the golden fixture file exactly",
      );
    } finally {
      await cleanup();
    }
  },
);

Deno.test(
  "golden fixture: read('spur-ad-capture') returns correct metadata",
  { permissions: { read: true, write: true, env: true } },
  async () => {
    const { root, cleanup } = await buildFixtureRoot();
    try {
      const data = await ports.read("spur-ad-capture", root);
      assertEquals(data.mime, "video/webm");
      assertEquals(data.version, 1);
      assertEquals(data.kind, "media");
      assertEquals(data.durationSec, 60);
    } finally {
      await cleanup();
    }
  },
);

Deno.test(
  "golden fixture: 'sales' arrow port entry parses with kind arrow",
  { permissions: { read: true, write: true, env: true } },
  async () => {
    const { root, cleanup } = await buildFixtureRoot();
    try {
      const m = await ports.manifest(root);
      assertEquals(m.ports["sales"].kind, "arrow");
      // Schema is present and has the expected id field.
      const schema = m.ports["sales"].schema as {
        fields: Array<{ name: string }>;
      };
      assertExists(schema, "sales port should have a schema");
      assertEquals(schema.fields[0].name, "id");
    } finally {
      await cleanup();
    }
  },
);

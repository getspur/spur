/**
 * `ports` — typed helpers for reading the SPUR port store from frontend cells.
 *
 * Port-store wire contract:
 *   - Root env var: `SPUR_NOTEBOOK_PORT_ROOT`
 *   - Manifest path: `${root}/ports/manifest.json`
 *   - Manifest shape:
 *     ```json
 *     {
 *       "ports": {
 *         "<name>": {
 *           "path": "<absolute path to versioned file>",
 *           "version": <N>,
 *           "kind": "arrow" | "media",
 *           "mime": "<optional mime type>",
 *           "size": <optional byte count>,
 *           "schema": <optional schema>,
 *           "duration_sec": <optional duration for media>
 *         }
 *       }
 *     }
 *     ```
 *   - Consumers MUST read `entry.path` — never derive `root/<name>`.
 *   - For safety, basename-join: use `basename(entry.path)` joined under
 *     the ports directory.
 */

import { basename, join } from "https://deno.land/std@0.224.0/path/mod.ts";

/** A single port entry from the manifest. */
export interface PortEntry {
  /** Absolute path to the versioned file (e.g. `name@v2.media`). */
  path: string;
  /** Monotonically increasing version counter. */
  version: number;
  /** Data kind. */
  kind: "arrow" | "media";
  /** Optional MIME type (typically set for `kind: "media"`). */
  mime?: string;
  /** Optional byte size. */
  size?: number;
  /** Optional schema (for `kind: "arrow"`). */
  schema?: unknown;
  /** Optional duration in seconds (for `kind: "media"`). */
  duration_sec?: number;
}

/** Manifest object at `${root}/ports/manifest.json`. */
export interface PortManifest {
  ports: Record<string, PortEntry>;
}

/** Result returned by `ports.read`. */
export interface PortData {
  bytes: Uint8Array;
  mime?: string;
  version: number;
  kind: "arrow" | "media";
  durationSec?: number;
}

/**
 * Return the port store root directory.
 * Accepts an explicit `root` override (used in tests and for dependency
 * injection); falls back to `SPUR_NOTEBOOK_PORT_ROOT`.
 * Throws if neither is set.
 */
function resolveRoot(override?: string): string {
  const root = override ?? Deno.env.get("SPUR_NOTEBOOK_PORT_ROOT");
  if (!root) {
    throw new Error(
      "SPUR_NOTEBOOK_PORT_ROOT is not set and no root override was provided",
    );
  }
  return root;
}

/**
 * Parse and return the port manifest.
 *
 * @param root Optional root directory override (default: `SPUR_NOTEBOOK_PORT_ROOT`).
 */
export async function manifest(root?: string): Promise<PortManifest> {
  const r = resolveRoot(root);
  const manifestPath = join(r, "ports", "manifest.json");
  const text = await Deno.readTextFile(manifestPath);
  return JSON.parse(text) as PortManifest;
}

/**
 * Read a port's bytes and metadata.
 *
 * Consumers MUST use `entry.path` for the file path. For safety this function
 * basename-joins: it takes `basename(entry.path)` and resolves it under the
 * `ports` sub-directory of the root, so a malicious or stale path segment
 * cannot escape the store.
 *
 * @param name Port name (key in `manifest.ports`).
 * @param root Optional root directory override (default: `SPUR_NOTEBOOK_PORT_ROOT`).
 * @throws If the port is not present in the manifest.
 */
export async function read(name: string, root?: string): Promise<PortData> {
  const r = resolveRoot(root);
  const m = await manifest(r);
  const entry = m.ports[name];
  if (!entry) {
    throw new Error(
      `Port "${name}" not found in manifest at ${
        join(r, "ports", "manifest.json")
      }`,
    );
  }
  // Basename-join for safety: never trust arbitrary paths from the manifest.
  const safePath = join(r, "ports", basename(entry.path));
  const bytes = await Deno.readFile(safePath);
  return {
    bytes,
    mime: entry.mime,
    version: entry.version,
    kind: entry.kind,
    durationSec: entry.duration_sec,
  };
}

export const ports = { manifest, read };

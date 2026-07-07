"use strict";
// Downloads the platform binary for this package's version from
// getspur/spur-releases, verifies it against the release SHA256SUMS, and
// installs it as ./bin/spur[.exe]. Runs at postinstall; run-spur.js also
// invokes it lazily when the binary is missing (--ignore-scripts installs).
const fs = require("fs");
const path = require("path");
const crypto = require("crypto");
const { assetFor, binaryPath } = require("./platform");

const RELEASES = "https://github.com/getspur/spur-releases/releases/download";

async function fetchBytes(url) {
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok) {
    throw new Error(`download failed (HTTP ${res.status}) for ${url}`);
  }
  return Buffer.from(await res.arrayBuffer());
}

function expectedSha256(sumsText, asset) {
  for (const line of sumsText.split("\n")) {
    const fields = line.trim().split(/\s+/);
    if (fields.length === 2 && fields[1] === asset) {
      return fields[0].toLowerCase();
    }
  }
  return null;
}

async function main() {
  const version = require("./package.json").version;
  const { asset, exe } = assetFor(version);
  const base = `${RELEASES}/v${version}`;

  const bytes = await fetchBytes(`${base}/${asset}`);

  const sums = (await fetchBytes(`${base}/SHA256SUMS`)).toString("utf8");
  const expected = expectedSha256(sums, asset);
  if (!expected) {
    throw new Error(`SHA256SUMS for v${version} has no entry for ${asset}`);
  }
  const actual = crypto.createHash("sha256").update(bytes).digest("hex");
  if (actual !== expected) {
    throw new Error(
      `checksum mismatch for ${asset}: expected ${expected}, got ${actual}`
    );
  }

  const dest = binaryPath(exe);
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.writeFileSync(dest, bytes, { mode: 0o755 });
  console.log(`spur ${version} installed: ${dest}`);
}

main().catch((err) => {
  console.error(`@getspur/spur-cli install failed: ${err && err.message ? err.message : err}`);
  process.exit(1);
});

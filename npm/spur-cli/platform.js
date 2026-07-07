"use strict";
// Maps the npm process platform/arch onto a spur release asset name, as
// published by `cargo xtask dist` to getspur/spur-releases. The macOS asset
// is a universal2 fat Mach-O, so a single asset serves both mac arches.
const path = require("path");

function assetFor(version, platform = process.platform, arch = process.arch) {
  if (platform === "darwin" && (arch === "arm64" || arch === "x64")) {
    return { asset: `spur-${version}-universal2-apple-darwin`, exe: false };
  }
  if (platform === "linux" && arch === "arm64") {
    return { asset: `spur-${version}-aarch64-unknown-linux-gnu`, exe: false };
  }
  if (platform === "linux" && arch === "x64") {
    return { asset: `spur-${version}-x86_64-unknown-linux-gnu`, exe: false };
  }
  if (platform === "win32" && arch === "x64") {
    return { asset: `spur-${version}-x86_64-pc-windows-msvc.exe`, exe: true };
  }
  throw new Error(
    `@getspur/spur-cli: no prebuilt spur binary for ${platform}/${arch}. ` +
      "See https://github.com/getspur/spur-releases/releases for available artifacts."
  );
}

function binaryPath(exe = process.platform === "win32") {
  return path.join(__dirname, "bin", exe ? "spur.exe" : "spur");
}

module.exports = { assetFor, binaryPath };

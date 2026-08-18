"use strict";

const assert = require("node:assert/strict");
const test = require("node:test");
const { assetFor } = require("./platform");

test("macOS downloads the release artifact for its process architecture", () => {
  assert.deepEqual(assetFor("1.20.0", "darwin", "arm64"), {
    asset: "spur-1.20.0-aarch64-apple-darwin",
    exe: false,
  });
  assert.deepEqual(assetFor("1.20.0", "darwin", "x64"), {
    asset: "spur-1.20.0-x86_64-apple-darwin",
    exe: false,
  });
});

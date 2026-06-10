/**
 * Tests for capture.canvas in src/capture.ts.
 */

import { assertEquals, assertStringIncludes } from "@std/assert";
import { capture } from "../src/capture.ts";

Deno.test("capture.canvas emits data-capture=true", () => {
  const html = capture.canvas({ port: "my-cell" });
  assertStringIncludes(html, 'data-capture="true"');
});

Deno.test("capture.canvas emits data-capture-cell-id with port value", () => {
  const html = capture.canvas({ port: "spur-ad-capture" });
  assertStringIncludes(html, 'data-capture-cell-id="spur-ad-capture"');
});

Deno.test("capture.canvas emits data-capture-fps with provided fps", () => {
  const html = capture.canvas({ port: "x", fps: 24 });
  assertStringIncludes(html, 'data-capture-fps="24"');
});

Deno.test("capture.canvas defaults fps to 30", () => {
  const html = capture.canvas({ port: "x" });
  assertStringIncludes(html, 'data-capture-fps="30"');
});

Deno.test("capture.canvas emits data-capture-duration-sec with provided durationSec", () => {
  const html = capture.canvas({ port: "x", durationSec: 60 });
  assertStringIncludes(html, 'data-capture-duration-sec="60"');
});

Deno.test("capture.canvas defaults durationSec to 3", () => {
  const html = capture.canvas({ port: "x" });
  assertStringIncludes(html, 'data-capture-duration-sec="3"');
});

Deno.test("capture.canvas emits width and height when provided", () => {
  const html = capture.canvas({ port: "x", width: 1280, height: 720 });
  assertStringIncludes(html, 'width="1280"');
  assertStringIncludes(html, 'height="720"');
});

Deno.test("capture.canvas omits width/height when not provided", () => {
  const html = capture.canvas({ port: "x" });
  assertEquals(html.includes("width="), false);
  assertEquals(html.includes("height="), false);
});

Deno.test("capture.canvas escapes special characters in port attribute", () => {
  const html = capture.canvas({ port: 'a"b&c<d>e' });
  assertStringIncludes(html, 'data-capture-cell-id="a&quot;b&amp;c&lt;d&gt;e"');
});

Deno.test("capture.canvas returns a valid canvas element string", () => {
  const html = capture.canvas({
    port: "p",
    fps: 30,
    durationSec: 5,
    width: 640,
    height: 480,
  });
  assertStringIncludes(html, "<canvas");
  assertStringIncludes(html, "></canvas>");
});

Deno.test("capture.canvas matches app.ipynb spur-ad-capture cell attributes", () => {
  // Verify faithfulness to the existing producer cell in app_gallery/html_video/app.ipynb
  const html = capture.canvas({
    port: "spur-ad-capture",
    fps: 30,
    durationSec: 60,
    width: 1280,
    height: 720,
  });
  assertStringIncludes(html, 'data-capture="true"');
  assertStringIncludes(html, 'data-capture-cell-id="spur-ad-capture"');
  assertStringIncludes(html, 'data-capture-fps="30"');
  assertStringIncludes(html, 'data-capture-duration-sec="60"');
  assertStringIncludes(html, 'width="1280"');
  assertStringIncludes(html, 'height="720"');
});

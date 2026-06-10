/**
 * Tests for display helpers in src/display.ts.
 */

import {
  assertEquals,
  assertExists,
} from "https://deno.land/std@0.224.0/assert/mod.ts";
import { display } from "../src/display.ts";

const JUPYTER = Symbol.for("Jupyter.display");

// ---------------------------------------------------------------------------
// Symbol protocol — all helpers must expose [Symbol.for("Jupyter.display")]
// ---------------------------------------------------------------------------

Deno.test("display.html exposes Symbol.for('Jupyter.display')", () => {
  const obj = display.html("<b>hi</b>");
  assertExists((obj as Record<symbol, unknown>)[JUPYTER]);
});

Deno.test("display.markdown exposes Symbol.for('Jupyter.display')", () => {
  const obj = display.markdown("# title");
  assertExists((obj as Record<symbol, unknown>)[JUPYTER]);
});

Deno.test("display.json exposes Symbol.for('Jupyter.display')", () => {
  const obj = display.json({ x: 1 });
  assertExists((obj as Record<symbol, unknown>)[JUPYTER]);
});

// ---------------------------------------------------------------------------
// MIME bundle contents
// ---------------------------------------------------------------------------

Deno.test("display.html returns text/html MIME bundle", () => {
  const content = "<b>Hello World</b>";
  const obj = display.html(content);
  const bundle = (obj as Record<symbol, () => Record<string, unknown>>)
    [JUPYTER]();
  assertEquals(bundle["text/html"], content);
});

Deno.test("display.markdown returns text/markdown MIME bundle", () => {
  const content = "# Title\n\nParagraph.";
  const obj = display.markdown(content);
  const bundle = (obj as Record<symbol, () => Record<string, unknown>>)
    [JUPYTER]();
  assertEquals(bundle["text/markdown"], content);
});

Deno.test("display.json returns application/json MIME bundle with original value", () => {
  const value = { answer: 42, nested: { ok: true } };
  const obj = display.json(value);
  const bundle = (obj as Record<symbol, () => Record<string, unknown>>)
    [JUPYTER]();
  assertEquals(bundle["application/json"], value);
});

Deno.test("display.json passes through non-object values", () => {
  const obj = display.json([1, 2, 3]);
  const bundle = (obj as Record<symbol, () => Record<string, unknown>>)
    [JUPYTER]();
  assertEquals(bundle["application/json"], [1, 2, 3]);
});

Deno.test("display.json bundle contains exactly the application/json key", () => {
  const obj = display.json(null);
  const bundle = (obj as Record<symbol, () => Record<string, unknown>>)
    [JUPYTER]();
  assertEquals(Object.keys(bundle), ["application/json"]);
});

Deno.test("display.html bundle contains exactly the text/html key", () => {
  const obj = display.html("");
  const bundle = (obj as Record<symbol, () => Record<string, unknown>>)
    [JUPYTER]();
  assertEquals(Object.keys(bundle), ["text/html"]);
});

Deno.test("display.markdown bundle contains exactly the text/markdown key", () => {
  const obj = display.markdown("");
  const bundle = (obj as Record<symbol, () => Record<string, unknown>>)
    [JUPYTER]();
  assertEquals(Object.keys(bundle), ["text/markdown"]);
});

import { expect, test } from "vitest";

import { extensionForLanguage } from "./CellInput";

test("sql code cells get a non-empty SQL extension", () => {
  const ext = extensionForLanguage("code", "sql");
  expect(ext).toBeTruthy();
});

test("markdown still wraps regardless of codeType", () => {
  const ext = extensionForLanguage("markdown");
  expect(ext).toBeTruthy();
});

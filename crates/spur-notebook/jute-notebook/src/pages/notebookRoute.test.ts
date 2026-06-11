import { describe, expect, it } from "vitest";

import {
  activeTabIdFromSearch,
  notebookRouteForPaths,
  pinnedPathsFromSearch,
} from "@/pages/notebookRoute";

describe("notebook route pinned paths", () => {
  it("serializes pinned paths as repeated params", () => {
    const route = notebookRouteForPaths(["/a.ipynb", "/b.ipynb"], "/b.ipynb", [
      "/a.ipynb",
    ]);
    const search = route.slice(route.indexOf("?"));

    expect(pinnedPathsFromSearch(search)).toEqual(["/a.ipynb"]);
    expect(activeTabIdFromSearch(search)).toBe("/b.ipynb");
  });

  it("returns an empty list when nothing is pinned", () => {
    const route = notebookRouteForPaths(["/a.ipynb"], "/a.ipynb");

    expect(pinnedPathsFromSearch(route.slice(route.indexOf("?")))).toEqual([]);
  });
});

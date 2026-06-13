import { describe, expect, test } from "vitest";

import {
  EMPTY_STATE_COPY,
  composerLensLabel,
  defaultLensFor,
  mapViewMode,
} from "./lens";

describe("sidebar lens model", () => {
  test("maps store view modes to backend view modes", () => {
    expect(mapViewMode("cells")).toBe("notebook");
    expect(mapViewMode("dag")).toBe("dag");
    expect(mapViewMode("app")).toBe("app");
  });

  test("defaults the lens for every view mode and app-open state", () => {
    expect(defaultLensFor("cells")).toBe("notebook_builder");
    expect(defaultLensFor("dag")).toBe("dag_ops");
    expect(
      defaultLensFor("app", {
        app_name: "Revenue App",
        app_root: "/tmp/revenue-app",
        capabilities: {
          active_output_scripts: true,
          artifacts_dir: true,
          canvas_capture: true,
        },
        open_mode: "app",
        skill: "revenue",
      }),
    ).toBe("app_product");
    expect(defaultLensFor("app", undefined)).toBe("notebook_deep_dive");
  });

  test("exposes copy for each lens", () => {
    expect(EMPTY_STATE_COPY.notebook_builder.heading).toBe(
      "Build on this notebook",
    );
    expect(EMPTY_STATE_COPY.notebook_deep_dive.heading).toBe(
      "Understand this notebook",
    );
    expect(EMPTY_STATE_COPY.dag_ops.heading).toBe("Operate this graph");
    expect(EMPTY_STATE_COPY.app_product.heading).toBe("Improve this app");
    expect(composerLensLabel("notebook_builder")).toBe("Builder lens");
    expect(composerLensLabel("notebook_deep_dive")).toBe("Deep dive lens");
    expect(composerLensLabel("dag_ops")).toBe("Operations lens");
    expect(composerLensLabel("app_product")).toBe("Product lens");
  });
});

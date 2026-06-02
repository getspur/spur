import { describe, expect, test } from "vitest";

import {
  compilePhasePresentation,
  compileProgressMessage,
  formatCompileElapsed,
} from "../compileProgress";

describe("compile progress helpers", () => {
  test("returns phase-specific presentation tokens", () => {
    const compiling = compilePhasePresentation("compiling");
    const running = compilePhasePresentation("running");

    expect(compiling.label).toBe("Compiling");
    expect(compiling.gutterBadgeClassName).toContain("amber");
    expect(compiling.dotClassName).toContain("amber");
    expect(compiling.railClassName).toContain("amber");

    expect(running.label).toBe("Running");
    expect(running.gutterBadgeClassName).toContain("emerald");
    expect(running.dotClassName).toContain("emerald");
    expect(running.railClassName).toContain("emerald");
  });

  test("formats elapsed time as floored seconds", () => {
    expect(formatCompileElapsed(1_000, 4_900)).toBe("3s");
    expect(formatCompileElapsed(5_000, 4_000)).toBe("0s");
  });

  test("formats progress messages without fake totals", () => {
    expect(compileProgressMessage("compiling", " crate-a ")).toBe(
      "Compiling crate-a",
    );
    expect(compileProgressMessage("compiling", "")).toBe("Compiling");
    expect(compileProgressMessage("compiling", null)).toBe("Compiling");
    expect(compileProgressMessage("running", "ignored")).toBe("Running");
  });
});

import { describe, expect, it } from "vitest";

import { cellToSlide } from "./cellToSlide";

const md = (source: string, id = "c1") => ({
  id,
  type: "markdown" as const,
  source,
  metadata: { spur: { version: 1 } },
  outputs: [],
});

const code = (source: string, outputs: any[] = [], id = "c1") => ({
  id,
  type: "code" as const,
  source,
  metadata: { spur: { version: 1 } },
  outputs,
});

const deck = { theme: "minimal-light" };

describe("cellToSlide layout inference", () => {
  it("infers 'title' from a single H1", () => {
    const s = cellToSlide(md("# Hello\nSubtitle"), deck);
    expect(s?.layout).toBe("title");
  });

  it("infers 'section' from a lone H2", () => {
    const s = cellToSlide(md("## Section break"), deck);
    expect(s?.layout).toBe("section");
  });

  it("infers 'bullets' from a markdown list", () => {
    const s = cellToSlide(md("### Three things\n- one\n- two\n- three"), deck);
    expect(s?.layout).toBe("bullets");
  });

  it("infers 'content' from generic markdown", () => {
    const s = cellToSlide(md("Just a paragraph of prose."), deck);
    expect(s?.layout).toBe("content");
  });

  it("infers 'output' for code cells by default", () => {
    const s = cellToSlide(code("print('x')"), deck);
    expect(s?.layout).toBe("output");
  });

  it("returns null when hidden=true", () => {
    const cell = md("# x");
    cell.metadata = { ...cell.metadata, jute_deck: { hidden: true } } as any;
    expect(cellToSlide(cell, deck)).toBeNull();
  });

  it("explicit layout overrides inference", () => {
    const cell = md("# x");
    cell.metadata = { ...cell.metadata, jute_deck: { layout: "two-col" } } as any;
    const s = cellToSlide(cell, deck);
    expect(s?.layout).toBe("two-col");
  });

  it("first-matching-rule-wins on mixed markdown (H1 wins over bullets)", () => {
    const s = cellToSlide(md("# Title\n\n- bullet"), deck);
    expect(s?.layout).toBe("title");
  });

  it("carries speaker_notes through to the slide", () => {
    const cell = md("# x");
    cell.metadata = { ...cell.metadata, jute_deck: { speaker_notes: "n" } } as any;
    expect(cellToSlide(cell, deck)?.speakerNotes).toBe("n");
  });

  it("resolves theme_override over notebook theme", () => {
    const cell = md("# x");
    cell.metadata = { ...cell.metadata, jute_deck: { theme_override: "spur-brand" } } as any;
    expect(cellToSlide(cell, deck)?.theme).toBe("spur-brand");
  });
});

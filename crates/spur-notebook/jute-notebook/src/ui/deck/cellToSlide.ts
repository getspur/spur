import type { Cell } from "@/bindings/Cell";
import type { CellMetadata } from "@/bindings/CellMetadata";
import type { JuteDeckNotebookMetadata } from "@/bindings/JuteDeckNotebookMetadata";
import type { Output } from "@/bindings/Output";

import type { Block, ResolvedLayout, SlideSpec } from "./types";

const H1_ONLY = /^\s*#\s+[^\n]+(\n[^\n#]+)?\s*$/;
const H2_ONLY = /^\s*##\s+[^\n]+\s*$/;
const HAS_BULLETS = /(^|\n)\s*[-*]\s+/;
const HAS_H1 = /(^|\n)\s*#\s+/;

type CellKind = "markdown" | "code" | "raw" | "html";

type FixtureCompatibleCell = {
  id?: string;
  type?: CellKind;
  cell_type?: CellKind;
  source?: string | string[];
  metadata?: CellMetadata & Record<string, unknown>;
  outputs?: Output[];
};

type CellToSlideInput = Cell | FixtureCompatibleCell;

export function cellToSlide(
  cell: CellToSlideInput,
  deck: JuteDeckNotebookMetadata | undefined,
): SlideSpec | null {
  const meta: CellMetadata = cell.metadata ?? {};
  const jd = meta.jute_deck;

  if (jd?.hidden === true) return null;

  const explicit = jd?.layout && jd.layout !== "auto" ? jd.layout : undefined;
  const layout: ResolvedLayout = explicit ?? inferLayout(cell);

  const source = normalizeSource(cell.source);
  const blocks = buildBlocks(cell, source, layout);

  const theme = jd?.theme_override ?? deck?.theme ?? "minimal-light";

  return {
    id: cell.id ?? "",
    layout,
    blocks,
    speakerNotes: jd?.speaker_notes,
    theme,
    background: jd?.background,
    fragments: jd?.fragments ?? false,
  };
}

function inferLayout(cell: CellToSlideInput): ResolvedLayout {
  const kind = cellKind(cell);
  if (kind === "code") return "output";
  if (kind === "raw" || kind === "html") return "blank";

  const source = normalizeSource(cell.source);

  // First-matching-rule-wins; order is intentional.
  if (H1_ONLY.test(source.trim())) return "title";
  if (H2_ONLY.test(source.trim())) return "section";
  // H1 + anything else still wins as "title" before bullets get checked,
  // because a deck-author who started with # almost certainly meant a title.
  if (HAS_H1.test(source)) return "title";
  if (HAS_BULLETS.test(source)) return "bullets";
  return "content";
}

function buildBlocks(
  cell: CellToSlideInput,
  source: string,
  layout: ResolvedLayout,
): Block[] {
  const kind = cellKind(cell);
  if (kind === "code") {
    if (layout === "code") return [{ kind: "code", lang: detectLang(cell), source }];
    if (layout === "code-output")
      return [
        { kind: "code", lang: detectLang(cell), source },
        { kind: "output", outputs: cellOutputs(cell) },
      ];
    return [{ kind: "output", outputs: cellOutputs(cell) }];
  }
  if (kind === "html") return [{ kind: "html", html: source }];

  // markdown / raw
  return [{ kind: "markdown", md: source }];
}

function detectLang(cell: CellToSlideInput): string {
  // Try metadata first, fall back to "python".
  const lang = (cell.metadata as Record<string, unknown> | undefined)?.kernel_language;
  return typeof lang === "string" ? lang : "python";
}

function cellKind(cell: CellToSlideInput): CellKind {
  return (cell as FixtureCompatibleCell).type ?? cell.cell_type ?? "markdown";
}

function cellOutputs(cell: CellToSlideInput): Output[] {
  return "outputs" in cell ? (cell.outputs ?? []) : [];
}

function normalizeSource(source: string | string[] | undefined): string {
  return typeof source === "string" ? source : (source ?? []).join("");
}

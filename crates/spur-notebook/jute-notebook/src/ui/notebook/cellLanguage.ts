import type { CodeType } from "@/bindings/CodeType";

export type CellLanguageId = CodeType | "spur";

export interface CellLanguageToken {
  id: CellLanguageId;
  label: string;
  glyph: string;
  kernelspec: string;
  accent: string;     // left accent bar + tinted gutter
  chipText: string;
  chipBg: string;
  chipBorder: string;
  glyphBg: string;
}

export const CELL_LANGUAGE_TOKENS: Record<CellLanguageId, CellLanguageToken> = {
  python:     { id: "python",     label: "Python",     glyph: "Py", kernelspec: "python3", accent: "#3776AB", chipText: "#2c5e8a", chipBg: "#ffffff", chipBorder: "#cfe0f1", glyphBg: "#eaf2fb" },
  javascript: { id: "javascript", label: "JavaScript", glyph: "JS", kernelspec: "deno",    accent: "#8A6D00", chipText: "#8a6d00", chipBg: "#ffffff", chipBorder: "#ead9a0", glyphBg: "#fcf7e8" },
  rust:       { id: "rust",       label: "Rust",       glyph: "Rs", kernelspec: "evcxr",   accent: "#CE422B", chipText: "#b23a22", chipBg: "#ffffff", chipBorder: "#e8b9ac", glyphBg: "#fbe9e4" },
  go:         { id: "go",         label: "Go",         glyph: "Go", kernelspec: "gonb",    accent: "#00ADD8", chipText: "#0a7e9e", chipBg: "#ffffff", chipBorder: "#a8deec", glyphBg: "#e5f6fb" },
  spur:       { id: "spur",       label: "AI Agent",   glyph: "✦",  kernelspec: "spur",    accent: "#7C3AED", chipText: "#6d28d9", chipBg: "#f5f3ff", chipBorder: "#ddd6fe", glyphBg: "#ffffff" },
};

export const CODE_LANGUAGE_ORDER: CellLanguageId[] = ["python", "javascript", "rust", "go", "spur"];

interface CellLike {
  codeType?: string;
  cellMetadataOther?: Record<string, unknown>;
}

export function cellLanguageId(cell: CellLike): CellLanguageId {
  const ks = (cell.cellMetadataOther?.kernelspec as { name?: string } | undefined)?.name;
  if (ks === "spur") return "spur";
  const id = cell.codeType ?? "python";
  return (id in CELL_LANGUAGE_TOKENS ? id : "python") as CellLanguageId;
}

export function cellLanguageToken(cell: CellLike): CellLanguageToken {
  return CELL_LANGUAGE_TOKENS[cellLanguageId(cell)];
}

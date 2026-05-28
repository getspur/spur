import type { JuteDeckLayout } from "@/bindings/JuteDeckLayout";
import type { Output } from "@/bindings/Output";

export type ResolvedLayout = Exclude<JuteDeckLayout, "auto">;

export type Block =
  | { kind: "heading"; level: 1 | 2 | 3; text: string }
  | { kind: "markdown"; md: string }
  | { kind: "bullets"; items: string[]; fragments: boolean }
  | { kind: "code"; lang: string; source: string }
  | { kind: "output"; outputs: Output[] }
  | { kind: "html"; html: string }
  | { kind: "image"; src: string; alt?: string };

export type SlideSpec = {
  id: string;
  layout: ResolvedLayout;
  blocks: Block[];
  speakerNotes?: string;
  theme: string;
  background?: string;
  fragments: boolean;
};

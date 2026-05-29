import { useEffect, useMemo, useState } from "react";
import { useLocation, useSearch } from "wouter";
import { useStore } from "zustand";

import { Notebook, NotebookContext } from "@/stores/notebook";
import PresentChrome from "@/ui/deck/PresentChrome";
import SlideFrame from "@/ui/deck/SlideFrame";
import SpeakerNotes from "@/ui/deck/SpeakerNotes";
import { cellToSlide } from "@/ui/deck/cellToSlide";
import { LayoutFor } from "@/ui/deck/layouts";
import type { Block, SlideSpec } from "@/ui/deck/types";

function countFragments(slide: SlideSpec | undefined): number {
  if (!slide?.fragments || slide.layout !== "bullets") return 1;

  const bullets = slide.blocks.find(
    (block): block is Extract<Block, { kind: "bullets" }> =>
      block.kind === "bullets",
  );
  if (bullets) return Math.max(bullets.items.length, 1);

  const markdown = slide.blocks.find(
    (block): block is Extract<Block, { kind: "markdown" }> =>
      block.kind === "markdown",
  );
  if (!markdown) return 1;

  const bulletCount = markdown.md
    .split("\n")
    .filter((line) => /^\s*[-*]\s+/.test(line)).length;
  return Math.max(bulletCount, 1);
}

export default function PresentPage() {
  const [, setLocation] = useLocation();
  const { path } = Object.fromEntries(new URLSearchParams(useSearch()));
  const notebook = useMemo(() => new Notebook(), []);

  useEffect(() => {
    if (path) void notebook.loadNotebookFromPath(path);
  }, [notebook, path]);

  const cellIds = useStore(
    notebook.store,
    (state) => state.serverState.cellIds,
  );
  const storeCells = useStore(
    notebook.store,
    (state) => state.serverState.cells,
  );
  const notebookDeckMetadata = useStore(
    notebook.store,
    (state) => state.serverState.notebookMetadata.jute_deck,
  );
  const notebookLanguage = useStore(
    notebook.store,
    (state) => state.serverState.notebookMetadata.language_info?.name,
  );
  const cellSourceDrafts = useStore(
    notebook.store,
    (state) => state.editBuffer.cellSources,
  );

  const cells = useMemo(
    () =>
      cellIds.map((id) => {
        const cell = storeCells[id];
        const sourceDraft = cellSourceDrafts[id];
        return {
          id,
          type: cell.type,
          source: sourceDraft?.source ?? cell.source,
          metadata: {
            jute_deck: cell.juteDeckMetadata,
            kernel_language: notebookLanguage,
          },
          outputs: cell.result?.outputs ?? [],
        };
      }),
    [cellIds, storeCells, cellSourceDrafts, notebookLanguage],
  );

  const slides: SlideSpec[] = useMemo(
    () =>
      cells
        .map((cell) => cellToSlide(cell, notebookDeckMetadata))
        .filter((slide): slide is SlideSpec => slide !== null),
    [cells, notebookDeckMetadata],
  );

  const [idx, setIdx] = useState(0);
  const [fragmentIndex, setFragmentIndex] = useState(0);
  const [notesOpen, setNotesOpen] = useState(false);
  const [blackout, setBlackout] = useState(false);

  useEffect(() => setFragmentIndex(0), [idx]);

  useEffect(() => {
    if (idx >= slides.length) {
      setIdx(Math.max(slides.length - 1, 0));
    }
  }, [idx, slides.length]);

  useEffect(() => {
    function onKey(event: KeyboardEvent) {
      const slide = slides[idx];
      const maxFragments = countFragments(slide);

      switch (event.key) {
        case "ArrowRight":
        case " ":
        case "PageDown":
          event.preventDefault();
          if (slide?.fragments && fragmentIndex < maxFragments - 1) {
            setFragmentIndex((current) => current + 1);
          } else if (idx < slides.length - 1) {
            setIdx((current) => current + 1);
          }
          break;
        case "ArrowLeft":
        case "PageUp":
          event.preventDefault();
          if (fragmentIndex > 0) {
            setFragmentIndex((current) => current - 1);
          } else if (idx > 0) {
            setIdx((current) => current - 1);
          }
          break;
        case "Home":
          event.preventDefault();
          setIdx(0);
          break;
        case "End":
          event.preventDefault();
          setIdx(Math.max(slides.length - 1, 0));
          break;
        case "s":
        case "S":
          event.preventDefault();
          setNotesOpen((current) => !current);
          break;
        case "b":
        case "B":
          event.preventDefault();
          setBlackout((current) => !current);
          break;
        case "Escape":
          event.preventDefault();
          setLocation(
            path ? `/notebook?path=${encodeURIComponent(path)}` : "/",
          );
          break;
      }
    }

    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [slides, idx, fragmentIndex, path, setLocation]);

  const slide = slides[idx];
  const effectiveFragmentIndex = slide?.fragments
    ? fragmentIndex
    : Number.MAX_SAFE_INTEGER;

  return (
    <NotebookContext.Provider value={notebook}>
      <main className="h-screen w-screen overflow-hidden bg-black">
        {blackout || !slide ? (
          <div className="h-full w-full bg-black" />
        ) : (
          <SlideFrame themeId={slide.theme} background={slide.background}>
            <LayoutFor slide={slide} fragmentIndex={effectiveFragmentIndex} />
          </SlideFrame>
        )}
        <PresentChrome current={idx} total={slides.length} />
        {notesOpen && <SpeakerNotes notes={slide?.speakerNotes} />}
      </main>
    </NotebookContext.Provider>
  );
}

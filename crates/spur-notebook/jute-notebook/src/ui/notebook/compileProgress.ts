import type { CompilePhase } from "@/bindings";

export type CompilePhasePresentation = {
  label: string;
  gutterBadgeClassName: string;
  dotClassName: string;
  railClassName: string;
  chipClassName: string;
  trackClassName: string;
  sweepClassName: string;
  textClassName: string;
};

const PHASE_PRESENTATION: Record<CompilePhase, CompilePhasePresentation> = {
  compiling: {
    label: "Compiling",
    gutterBadgeClassName: "text-amber-700",
    dotClassName: "bg-amber-500",
    railClassName: "border-amber-200 bg-amber-50/80",
    chipClassName: "border-amber-200 bg-amber-100 text-amber-700",
    trackClassName: "bg-amber-200/70",
    sweepClassName: "bg-amber-500/80",
    textClassName: "text-amber-800",
  },
  running: {
    label: "Running",
    gutterBadgeClassName: "text-emerald-700",
    dotClassName: "bg-emerald-500",
    railClassName: "border-emerald-200 bg-emerald-50/80",
    chipClassName: "border-emerald-200 bg-emerald-100 text-emerald-700",
    trackClassName: "bg-emerald-200/70",
    sweepClassName: "bg-emerald-500/80",
    textClassName: "text-emerald-800",
  },
};

export function compilePhasePresentation(
  phase: CompilePhase,
): CompilePhasePresentation {
  return PHASE_PRESENTATION[phase];
}

export function formatCompileElapsed(startedAt: number, now: number): string {
  const seconds = Math.max(0, Math.floor((now - startedAt) / 1000));
  return `${seconds}s`;
}

export function compileProgressMessage(
  phase: CompilePhase,
  current: string | null,
): string {
  const { label } = compilePhasePresentation(phase);
  if (phase === "running") {
    return label;
  }

  const trimmedCurrent = current?.trim();
  return trimmedCurrent ? `${label} ${trimmedCurrent}` : label;
}

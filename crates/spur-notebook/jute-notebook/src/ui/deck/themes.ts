export type ThemeId = "minimal-light" | "minimal-dark" | "spur-brand";

export type Theme = {
  id: ThemeId;
  // Tailwind utility classes applied to the SlideFrame root.
  frame: string;
  // Heading / body / muted text classes for layout components to consume.
  heading: string;
  body: string;
  muted: string;
  accent: string;
};

export const THEMES: Record<ThemeId, Theme> = {
  "minimal-light": {
    id: "minimal-light",
    frame: "bg-white text-slate-900",
    heading: "text-slate-900",
    body: "text-slate-800",
    muted: "text-slate-500",
    accent: "text-blue-600",
  },
  "minimal-dark": {
    id: "minimal-dark",
    frame: "bg-slate-900 text-slate-50",
    heading: "text-slate-50",
    body: "text-slate-100",
    muted: "text-slate-400",
    accent: "text-blue-400",
  },
  "spur-brand": {
    id: "spur-brand",
    frame: "bg-gradient-to-br from-indigo-900 to-violet-800 text-slate-50",
    heading: "text-white",
    body: "text-violet-50",
    muted: "text-violet-200",
    accent: "text-amber-300",
  },
};

export function resolveTheme(id: string | undefined): Theme {
  if (id && id in THEMES) return THEMES[id as ThemeId];
  return THEMES["minimal-light"];
}

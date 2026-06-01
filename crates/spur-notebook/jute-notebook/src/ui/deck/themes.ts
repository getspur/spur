export type ThemeId =
  | "minimal-light"
  | "minimal-dark"
  | "spur-brand"
  | "editorial-monocle"
  | "modern-minimal"
  | "warm-soft"
  | "tech-utility"
  | "brutalist";

export type Theme = {
  id: ThemeId;
  // Tailwind utility classes applied to the SlideFrame root.
  frame: string;
  // Heading / body / muted text classes for layout components to consume.
  heading: string;
  body: string;
  muted: string;
  accent: string;
  // Optional CSS custom properties for token-backed themes.
  vars?: Record<string, string>;
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
  "editorial-monocle": {
    id: "editorial-monocle",
    frame: "[background:var(--deck-bg)] [color:var(--deck-fg)]",
    heading: "[color:var(--deck-fg)] [font-family:var(--deck-font-display)]",
    body: "[color:var(--deck-fg)] [font-family:var(--deck-font-body)]",
    muted: "[color:var(--deck-muted)] [font-family:var(--deck-font-body)]",
    accent: "[color:var(--deck-accent)]",
    vars: {
      "--deck-bg": "oklch(97% 0.012 80)",
      "--deck-surface": "oklch(99% 0.005 80)",
      "--deck-fg": "oklch(20% 0.02 60)",
      "--deck-muted": "oklch(48% 0.015 60)",
      "--deck-border": "oklch(89% 0.012 80)",
      "--deck-accent": "oklch(58% 0.16 35)",
      "--deck-font-display": "'Iowan Old Style', 'Charter', Georgia, serif",
      "--deck-font-body":
        "-apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif",
    },
  },
  "modern-minimal": {
    id: "modern-minimal",
    frame: "[background:var(--deck-bg)] [color:var(--deck-fg)]",
    heading: "[color:var(--deck-fg)] [font-family:var(--deck-font-display)]",
    body: "[color:var(--deck-fg)] [font-family:var(--deck-font-body)]",
    muted: "[color:var(--deck-muted)] [font-family:var(--deck-font-body)]",
    accent: "[color:var(--deck-accent)]",
    vars: {
      "--deck-bg": "oklch(99% 0.002 240)",
      "--deck-surface": "oklch(100% 0 0)",
      "--deck-fg": "oklch(18% 0.012 250)",
      "--deck-muted": "oklch(54% 0.012 250)",
      "--deck-border": "oklch(92% 0.005 250)",
      "--deck-accent": "oklch(58% 0.18 255)",
      "--deck-font-display":
        "-apple-system, BlinkMacSystemFont, 'SF Pro Display', system-ui, sans-serif",
      "--deck-font-body":
        "-apple-system, BlinkMacSystemFont, 'SF Pro Text', system-ui, sans-serif",
    },
  },
  "warm-soft": {
    id: "warm-soft",
    frame: "[background:var(--deck-bg)] [color:var(--deck-fg)]",
    heading: "[color:var(--deck-fg)] [font-family:var(--deck-font-display)]",
    body: "[color:var(--deck-fg)] [font-family:var(--deck-font-body)]",
    muted: "[color:var(--deck-muted)] [font-family:var(--deck-font-body)]",
    accent: "[color:var(--deck-accent)]",
    vars: {
      "--deck-bg": "oklch(97% 0.018 70)",
      "--deck-surface": "oklch(99% 0.008 70)",
      "--deck-fg": "oklch(22% 0.02 50)",
      "--deck-muted": "oklch(50% 0.018 50)",
      "--deck-border": "oklch(90% 0.014 70)",
      "--deck-accent": "oklch(64% 0.13 28)",
      "--deck-font-display":
        "'Tiempos Headline', 'Newsreader', 'Iowan Old Style', Georgia, serif",
      "--deck-font-body":
        "'Söhne', -apple-system, BlinkMacSystemFont, system-ui, sans-serif",
    },
  },
  "tech-utility": {
    id: "tech-utility",
    frame: "[background:var(--deck-bg)] [color:var(--deck-fg)]",
    heading: "[color:var(--deck-fg)] [font-family:var(--deck-font-display)]",
    body: "[color:var(--deck-fg)] [font-family:var(--deck-font-body)]",
    muted: "[color:var(--deck-muted)] [font-family:var(--deck-font-body)]",
    accent: "[color:var(--deck-accent)]",
    vars: {
      "--deck-bg": "oklch(98% 0.005 250)",
      "--deck-surface": "oklch(100% 0 0)",
      "--deck-fg": "oklch(22% 0.02 240)",
      "--deck-muted": "oklch(50% 0.018 240)",
      "--deck-border": "oklch(90% 0.008 240)",
      "--deck-accent": "oklch(58% 0.16 145)",
      "--deck-font-display":
        "-apple-system, BlinkMacSystemFont, 'Inter', 'Segoe UI', system-ui, sans-serif",
      "--deck-font-body":
        "-apple-system, BlinkMacSystemFont, 'Inter', 'Segoe UI', system-ui, sans-serif",
      "--deck-font-mono":
        "'JetBrains Mono', 'IBM Plex Mono', ui-monospace, Menlo, monospace",
    },
  },
  "brutalist": {
    id: "brutalist",
    frame: "[background:var(--deck-bg)] [color:var(--deck-fg)]",
    heading: "[color:var(--deck-fg)] [font-family:var(--deck-font-display)]",
    body: "[color:var(--deck-fg)] [font-family:var(--deck-font-body)]",
    muted: "[color:var(--deck-muted)] [font-family:var(--deck-font-body)]",
    accent: "[color:var(--deck-accent)]",
    vars: {
      "--deck-bg": "oklch(96% 0.004 100)",
      "--deck-surface": "oklch(100% 0 0)",
      "--deck-fg": "oklch(15% 0.02 100)",
      "--deck-muted": "oklch(40% 0.02 100)",
      "--deck-border": "oklch(15% 0.02 100)",
      "--deck-accent": "oklch(60% 0.22 25)",
      "--deck-font-display": "'Times New Roman', 'Iowan Old Style', Georgia, serif",
      "--deck-font-body":
        "ui-monospace, 'IBM Plex Mono', 'JetBrains Mono', Menlo, monospace",
      "--deck-font-mono":
        "'JetBrains Mono', 'IBM Plex Mono', ui-monospace, Menlo, monospace",
    },
  },
};

export function resolveTheme(id: string | undefined): Theme {
  if (id && id in THEMES) return THEMES[id as ThemeId];
  return THEMES["minimal-light"];
}

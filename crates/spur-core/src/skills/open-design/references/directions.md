# Open Design — Visual Directions

When the user has no brand, offer these 5 directions, then bind the chosen one's
OKLch palette + font stack to the artifact's CSS `:root`. Deterministic — never
improvise colors. Keep the directions visually distinct.

## Editorial Monocle
- **Label:** Editorial — Monocle / FT magazine
- **Mood:** Print-magazine feel. Generous whitespace, large serif headlines, restrained palette of off-white paper + ink + a single warm accent. Confident, quietly intelligent.
- **References:** Monocle · The Financial Times Weekend · NYT Magazine · It's Nice That
- **Display font:** `'Iowan Old Style', 'Charter', Georgia, serif`
- **Body font:** `-apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif`
- **Palette (OKLch):** bg `oklch(97% 0.012 80)` · surface `oklch(99% 0.005 80)` · fg `oklch(20% 0.02 60)` · muted `oklch(48% 0.015 60)` · border `oklch(89% 0.012 80)` · accent `oklch(58% 0.16 35)`
- **Posture:** serif display, sans body, mono for metadata only; no shadows, no rounded cards — borders + whitespace do the work; one decisive image cropped at the bottom; kicker in mono uppercase; one accent color used at most twice.

## Modern Minimal
- **Label:** Modern minimal — Linear / Vercel
- **Mood:** Quiet, precise, software-native. System fonts, near-greyscale palette, a single saturated accent. The chrome disappears so content is the only thing that registers.
- **References:** Linear · Vercel · Notion 2024 · Stripe docs
- **Display font:** `-apple-system, BlinkMacSystemFont, 'SF Pro Display', system-ui, sans-serif`
- **Body font:** `-apple-system, BlinkMacSystemFont, 'SF Pro Text', system-ui, sans-serif`
- **Palette (OKLch):** bg `oklch(99% 0.002 240)` · surface `oklch(100% 0 0)` · fg `oklch(18% 0.012 250)` · muted `oklch(54% 0.012 250)` · border `oklch(92% 0.005 250)` · accent `oklch(58% 0.18 255)`
- **Posture:** tight letter-spacing on display (-0.02em); hairline borders only, no shadows except dropdowns/modals; tabular-nums; sticky frosted nav, content-led layouts; one accent for links + primary CTA only.

## Warm Soft
- **Label:** Warm & soft — Stripe pre-2020 / Headspace
- **Mood:** Cream backgrounds, soft accent, gentle radii. Reads like a thoughtful product magazine — friendly without being cute. Good for fintech, wellness, indie SaaS.
- **References:** Stripe pre-2020 · Headspace · Substack · Mercury
- **Display font:** `'Tiempos Headline', 'Newsreader', 'Iowan Old Style', Georgia, serif`
- **Body font:** `'Söhne', -apple-system, BlinkMacSystemFont, system-ui, sans-serif`
- **Palette (OKLch):** bg `oklch(97% 0.018 70)` · surface `oklch(99% 0.008 70)` · fg `oklch(22% 0.02 50)` · muted `oklch(50% 0.018 50)` · border `oklch(90% 0.014 70)` · accent `oklch(64% 0.13 28)`
- **Posture:** serif display, soft sans body; gentle radii (12–16px), no hard 0px corners; single accent for CTA + one editorial flourish; soft inner glow rather than drop shadows; real screenshots/photos over icons.

## Tech Utility
- **Label:** Tech / utility — Datadog / GitHub
- **Mood:** Data-dense, monospace-friendly, light + grid. Made for engineers and operators who want information per square inch, not vibes.
- **References:** Datadog · GitHub · Cloudflare dashboard · Sentry
- **Display font:** `-apple-system, BlinkMacSystemFont, 'Inter', 'Segoe UI', system-ui, sans-serif`
- **Body font:** `-apple-system, BlinkMacSystemFont, 'Inter', 'Segoe UI', system-ui, sans-serif`
- **Mono font:** `'JetBrains Mono', 'IBM Plex Mono', ui-monospace, Menlo, monospace`
- **Palette (OKLch):** bg `oklch(98% 0.005 250)` · surface `oklch(100% 0 0)` · fg `oklch(22% 0.02 240)` · muted `oklch(50% 0.018 240)` · border `oklch(90% 0.008 240)` · accent `oklch(58% 0.16 145)`
- **Posture:** one sans family OK — utility trumps editorial; tabular numerics everywhere, mono for code/IDs/hashes; dense tables with hairline borders, no striping; inline status pills with restrained tinted backgrounds; show the product, not hero images.

## Brutalist Experimental
- **Label:** Brutalist / experimental — Are.na / Yale
- **Mood:** Loud type. Visible grid. System sans + a single oversized serif. Deliberate ugliness as confidence. Great for art, indie, agency, manifesto pages.
- **References:** Are.na · Yale Center for British Art · mschf · Read.cv
- **Display font:** `'Times New Roman', 'Iowan Old Style', Georgia, serif`
- **Body font:** `ui-monospace, 'IBM Plex Mono', 'JetBrains Mono', Menlo, monospace`
- **Palette (OKLch):** bg `oklch(96% 0.004 100)` · surface `oklch(100% 0 0)` · fg `oklch(15% 0.02 100)` · muted `oklch(40% 0.02 100)` · border `oklch(15% 0.02 100)` · accent `oklch(60% 0.22 25)`
- **Posture:** serif display at extreme sizes (clamp(80px, 12vw, 200px)); monospace body, deliberately; full-strength fg borders (1.5–2px); asymmetric 70/30 columns; near-zero radius, no shadows, no gradients; underline links, no hover decoration.

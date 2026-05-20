# SPUR Logo — Spec for Designer

Status: **no artwork yet**. This file is the brief a designer can work against. When files exist, drop them in `marketing/launch/press-kit/logo/` alongside this spec.

## Wordmark

- The wordmark is the primary mark. There is no separate icon at launch — if a square avatar is needed, use the first letter of the wordmark, set in the wordmark's typeface, on the brand background.
- Set in a monospaced or mono-adjacent typeface. SPUR is a terminal product; the wordmark should look like something that would render correctly inside a terminal.
- Letterforms: all caps, evenly spaced. No ligatures, no custom letter joins, no italic.
- Provide as SVG (primary), PNG @ 1x/2x/3x, and a single-color PDF.

## Variants required

| Variant | Use |
|---|---|
| Wordmark — dark on light | Default. Light backgrounds, print, light-themed web. |
| Wordmark — light on dark | Dark UI, terminal screenshots, dark social cards. |
| Wordmark — single-color black | One-color print, partner co-brand lockups. |
| Wordmark — single-color white | One-color print on dark, embroidery, etched. |

## Clear-space

Minimum clear-space around the wordmark on all sides: **the cap-height of the wordmark's first letter**. Nothing — no text, no edge, no other logo — inside that margin.

## Minimum size

- Digital: 80px wide minimum.
- Print: 20mm wide minimum.

Below those sizes, the wordmark loses legibility. Use the first-letter avatar instead if you need to go smaller.

## Do not

- Do not apply gradients, drop shadows, glows, bevels, or any "SaaS-y" effects. The mark is flat, period.
- Do not use emoji adjacent to the wordmark in official materials. SPUR is a developer tool; emoji on the mark cheapens it.
- Do not rotate, skew, stretch, or condense the wordmark.
- Do not place the wordmark on a background that fails WCAG AA contrast against it.
- Do not pair the wordmark with a tagline as part of the logo. Tagline is a separate text element.
- Do not invent an icon or symbol to accompany the wordmark without founder review.

## Co-brand lockups

When SPUR appears alongside another logo (e.g. integration announcements, co-marketing), separate the two marks by **2× the SPUR clear-space minimum** with a vertical hairline divider in the dominant color of the lighter mark. Do not merge, overlap, or stack.

## Files to produce

- `spur-wordmark-dark.svg`
- `spur-wordmark-light.svg`
- `spur-wordmark-black.svg`
- `spur-wordmark-white.svg`
- PNG @ 1x, 2x, 3x for each of the four variants
- Single-color PDF for each of the four variants
- This spec file, updated with the final typeface name and hex codes once chosen

TBD — founder to confirm exact typeface choice and brand background hex.

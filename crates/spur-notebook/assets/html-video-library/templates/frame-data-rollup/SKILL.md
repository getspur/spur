---
name: frame-data-rollup
description: Animated horizontal bar chart frame for showing a small set of labels and values in video.
---
# Frame: Data Rollup

Use this template when a video needs a concise data visualization: rankings,
metric comparisons, progress summaries, or KPI rollups.

## Inputs

- `labels`: edit the `label` fields in the `data` array inside `template.html`.
- `values`: edit the numeric `value` fields. The script scales bars against
  the largest value in the array.
- Optional title and deck copy live in the `.eyebrow`, `h1`, and `.note`
  elements.

## Usage

Keep all CSS and JavaScript inline. The bars animate on page load with a
requestAnimationFrame spring loop, so browser capture starts from zero width
and settles into the final chart without external dependencies.

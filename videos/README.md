# videos/ (not part of the OSS product tree)

Launch films, HyperFrames projects, contact sheets, and related renders are
**gitignored** in this monorepo. SPUR’s public surface is the Rust product
(`crates/`, `scripts/`, contributor docs) — not media production.

## Where this content should live

- Private or public **media** repo / object storage (`getspur/spur-media` or R2/S3)
- Release assets attached to GitHub Releases when a film must be public
- Prefer Git LFS or CDN; do not re-commit multi‑MB `.mp4` into product history

Local working copies may remain under this directory; they will not be tracked.

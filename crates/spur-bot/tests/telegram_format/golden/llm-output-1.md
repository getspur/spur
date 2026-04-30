# Chunked renderer rollout notes

The renderer walks **pulldown-cmark** events once and keeps chunk state for
open block contexts. This lets the bot split after HTML escaping instead of
guessing from raw markdown.

Key rules:

1. Budget against Telegram's HTML body limit.
2. Keep links like [the spec](https://example.test/spec?section=chunking&format=html) valid.
3. Preserve `inline code` and escaped text such as `<xml attr="&">`.

```rust
fn render(input: &str) -> Vec<Chunk> {
    markdown_to_telegram_chunks(input)
}
```

Nested notes:

> The old path split raw markdown.
>
> - A fenced code block could be cut in half.
> - Escaping could expand past 4096 units.
>
> ```text
> quoted code remains outside Telegram blockquote tags
> ```
>
> Rendering resumes inside the quote after the code block.

| case | expected behavior |
| --- | --- |
| code | preserve `<pre><code>` pairs |
| link | fall back to label plus URL when needed |
| table | split only between rows |

The remaining paragraphs deliberately contain escapable characters. Symbols:
<<<<<<<< &&&&&& >>>>>>. More explanation follows so this file is long enough
to exercise normal chunk boundaries without being adversarial.

Repeatable details:

- The plain fallback should read naturally.
- Markdown markers should not leak into fallback text.
- Numbered lists should continue at the authored number.

3. third item starts at three
4. fourth item includes enough words to cross a soft boundary when combined
   with the surrounding explanation and keep the next item number stable
5. fifth item closes the list

The final section includes a long explanatory paragraph about how a worker
should reason from parser events, not raw text offsets. Parser events already
know whether text is inside a link, table cell, blockquote, or code block. The
renderer can therefore make small local decisions while still producing global
invariants: every chunk is balanced, every chunk fits the limit, and every
plain fallback is free of markdown control syntax. This is the property that
keeps Telegram delivery boring even for dense final answers.

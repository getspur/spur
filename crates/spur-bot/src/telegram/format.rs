/// Telegram caps message text at 4096 UTF-16 code units.
pub const TELEGRAM_TEXT_MAX_UTF16_UNITS: usize = 4096;

/// Telegram caps inline-button labels at 64 UTF-8 bytes.
pub const TELEGRAM_BUTTON_LABEL_MAX_BYTES: usize = 64;

const FINAL_ANSWER_SPLIT_BOUNDARY_WINDOW_UTF16_UNITS: usize = 256;

/// Truncate `text` so its UTF-16 code-unit length is at most `max_units`.
/// Cuts on a char boundary. Returns the kept prefix and the count of dropped
/// chars.
pub fn truncate_to_utf16_units(text: &str, max_units: usize) -> (String, usize) {
    let total_chars = text.chars().count();
    let mut units = 0usize;
    let mut keep = String::with_capacity(text.len().min(max_units.saturating_mul(2)));
    for ch in text.chars() {
        let next = units + ch.len_utf16();
        if next > max_units {
            break;
        }
        units = next;
        keep.push(ch);
    }
    let dropped = total_chars - keep.chars().count();
    (keep, dropped)
}

/// Truncate `label` so its UTF-8 byte length is at most `max_bytes`. Cuts on a
/// char boundary and appends an ellipsis when truncation actually happens.
pub fn truncate_button_label_bytes(label: &str, max_bytes: usize) -> String {
    if label.len() <= max_bytes {
        return label.to_string();
    }
    const ELLIPSIS: &str = "\u{2026}"; // "…", 3 UTF-8 bytes
    let cap = max_bytes.saturating_sub(ELLIPSIS.len());
    let mut out = String::with_capacity(cap);
    for ch in label.chars() {
        if out.len() + ch.len_utf8() > cap {
            break;
        }
        out.push(ch);
    }
    out.push_str(ELLIPSIS);
    out
}

/// Render `text` as a single message that fits Telegram's 4096-UTF-16-unit
/// limit. If truncation is required, append `\n\n…[truncated; N chars
/// dropped]` where N is the count of dropped chars.
///
/// Budget is computed against the worst-case tail length (digit width sized to
/// total chars), guaranteeing the final body never exceeds the limit
/// regardless of the actual dropped count.
pub fn render_truncated_text(text: &str) -> String {
    if text.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS {
        return text.to_string();
    }
    let total_chars = text.chars().count();
    let worst_tail = format!("\n\n\u{2026}[truncated; {total_chars} chars dropped]");
    let worst_tail_units = worst_tail.encode_utf16().count();
    let budget = TELEGRAM_TEXT_MAX_UTF16_UNITS.saturating_sub(worst_tail_units);
    let (kept, dropped) = truncate_to_utf16_units(text, budget);
    let actual_tail = format!("\n\n\u{2026}[truncated; {dropped} chars dropped]");
    debug_assert!(
        kept.encode_utf16().count() + actual_tail.encode_utf16().count()
            <= TELEGRAM_TEXT_MAX_UTF16_UNITS,
    );
    format!("{kept}{actual_tail}")
}

pub fn split_for_final_answer(text: &str, max_units: usize) -> Vec<String> {
    assert!(max_units > 0, "max_units must be positive");

    if text.encode_utf16().count() <= max_units {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        if remaining.encode_utf16().count() <= max_units {
            chunks.push(remaining.to_string());
            break;
        }

        let hard_split = truncate_to_utf16_units(remaining, max_units).0.len();
        let split_at = preferred_final_answer_split(remaining, hard_split, max_units)
            .unwrap_or(hard_split)
            .max(remaining.chars().next().map(char::len_utf8).unwrap_or(1));

        chunks.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..];
    }

    chunks
}

fn preferred_final_answer_split(text: &str, hard_split: usize, max_units: usize) -> Option<usize> {
    let prefix = &text[..hard_split];
    let min_units = max_units.saturating_sub(FINAL_ANSWER_SPLIT_BOUNDARY_WINDOW_UTF16_UNITS);

    ["\n\n", "\n", " "]
        .into_iter()
        .find_map(|delimiter| best_split_after_delimiter(prefix, delimiter, min_units, max_units))
}

fn best_split_after_delimiter(
    text: &str,
    delimiter: &str,
    min_units: usize,
    max_units: usize,
) -> Option<usize> {
    text.match_indices(delimiter)
        .map(|(idx, matched)| idx + matched.len())
        .filter(|&split_at| {
            let units = text[..split_at].encode_utf16().count();
            units > min_units && units <= max_units
        })
        .last()
}

pub fn split_for_telegram(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if current.chars().count() == max_chars {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    if chunks.is_empty() {
        vec![String::new()]
    } else {
        chunks
    }
}

pub fn short_button_label(label: &str, max_chars: usize) -> String {
    if label.chars().count() <= max_chars {
        return label.to_string();
    }

    let first_word = label.split_whitespace().next().unwrap_or(label);
    label
        .split_whitespace()
        .scan(String::new(), |acc, part| {
            let candidate = if acc.is_empty() {
                part.to_string()
            } else {
                format!("{acc} {part}")
            };
            if candidate.chars().count() < max_chars || acc.as_str() == first_word {
                *acc = candidate.clone();
                Some(acc.clone())
            } else {
                None
            }
        })
        .last()
        .unwrap_or_else(|| first_word.to_string())
}

/// The kind of prefix that opened the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    /// Slash-command: `/…`. v1 only fires at byte offset 0.
    Slash,
    /// Resource mention: `@…`. Fires anywhere after whitespace or at offset 0.
    Mention,
}

/// An active popup trigger detected in the InputBar text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub kind: TriggerKind,
    /// Byte offset of the trigger char (`/` or `@`) in `text`.
    pub prefix_start: usize,
    /// The query between the trigger char and the cursor (no leading char).
    pub query: String,
}

/// Decide whether a popup should be open given `(text, cursor)`.
///
/// Rules (v1):
///   * `/` fires only at byte offset 0.
///   * `@` fires at byte offset 0 OR immediately after ASCII whitespace.
///   * Any whitespace character between the trigger char and the cursor
///     closes the popup.
pub fn detect(text: &str, cursor: usize) -> Option<Trigger> {
    if cursor == 0 || cursor > text.len() {
        return None;
    }
    let before = &text[..cursor];

    // Slash: at offset 0 only.
    if let Some(query) = before.strip_prefix('/') {
        if !query.contains(char::is_whitespace) {
            return Some(Trigger {
                kind: TriggerKind::Slash,
                prefix_start: 0,
                query: query.to_string(),
            });
        }
    }

    // Mention: find the last '@' preceded by start-of-string or whitespace,
    // then verify no whitespace intervenes between '@' and cursor.
    if let Some(at_pos) = before.rfind('@') {
        let prev_is_boundary = at_pos == 0
            || before[..at_pos]
                .chars()
                .last()
                .is_none_or(|c| c.is_whitespace());
        if prev_is_boundary {
            let query = &before[at_pos + 1..];
            if !query.contains(char::is_whitespace) {
                return Some(Trigger {
                    kind: TriggerKind::Mention,
                    prefix_start: at_pos,
                    query: query.to_string(),
                });
            }
        }
    }

    None
}

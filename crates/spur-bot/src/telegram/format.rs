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

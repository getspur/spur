//! Grounding tests for the cursor-split renderer design.
//!
//! These tests run against the real pulldown-cmark 0.13 used by the crate
//! to verify the assumptions baked into the design spec at
//! `docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md`.
//!
//! Each test encodes one load-bearing claim from the spec. If a test fails,
//! the corresponding spec claim must be revised before implementation.

#![cfg(feature = "markdown")]

use pulldown_cmark::{Event, Options, Parser, TagEnd};

/// Collect (Event, Range) pairs for a given input. Helper used by all tests.
fn events(input: &str) -> Vec<(Event<'_>, std::ops::Range<usize>)> {
    Parser::new_ext(input, Options::empty())
        .into_offset_iter()
        .collect()
}

/// Mirrors the spec's `scan_authoritative` rule: an End is authoritative iff
/// depth drops to 0 AND range.end < input.len() (or <= len in EOF-permissive
/// mode). Depth tracks Start/End nesting across all tags; only document-root
/// block closes advance the cursor.
fn authoritative_end(input: &str, permit_eof: bool) -> usize {
    let mut max_end = 0;
    let mut depth: i32 = 0;
    for (ev, range) in Parser::new_ext(input, Options::empty()).into_offset_iter() {
        match &ev {
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                depth -= 1;
                let permitted = if permit_eof {
                    range.end <= input.len()
                } else {
                    range.end < input.len()
                };
                if depth == 0 && permitted {
                    max_end = max_end.max(range.end);
                }
            }
            _ => {}
        }
    }
    max_end
}

/// Old rule (naive, len-only) kept for the regression test that proves it
/// would mis-commit nested container closes. Not used anywhere in the spec;
/// documented here as the wrong answer.
#[allow(dead_code)]
fn naive_authoritative_end(input: &str) -> usize {
    let mut max_end = 0;
    for (ev, range) in Parser::new_ext(input, Options::empty()).into_offset_iter() {
        if matches!(ev, Event::End(_)) && range.end < input.len() {
            max_end = max_end.max(range.end);
        }
    }
    max_end
}

// ───────────────────── Block-level range semantics ─────────────────────

#[test]
fn claim_1_paragraph_at_eof_has_range_end_equal_to_len() {
    // Spec claim (Section 1): content at EOF emits End with range.end == len,
    // which the rule `range.end < len` excludes from authoritative.
    let input = "Hello world";
    let evs = events(input);
    let end_events: Vec<_> = evs
        .iter()
        .filter(|(e, _)| matches!(e, Event::End(TagEnd::Paragraph)))
        .collect();
    assert_eq!(end_events.len(), 1, "expected one paragraph End");
    let (_, range) = end_events[0];
    assert_eq!(
        range.end,
        input.len(),
        "paragraph at EOF must have range.end == input.len(); got {}",
        range.end
    );
    assert_eq!(
        authoritative_end(input, false),
        0,
        "no authoritative end under non-permissive rule"
    );
    assert_eq!(
        authoritative_end(input, true),
        input.len(),
        "EOF-permissive mode admits the EOF end"
    );
}

#[test]
fn claim_2_paragraph_with_content_after_is_authoritative() {
    // Spec claim: content followed by \n\n + more → End(Paragraph) with
    // range.end < len, so cursor advances.
    let input = "Hello\n\nworld";
    let evs = events(input);
    let mut saw_first_paragraph_end_before_eof = false;
    for (ev, range) in &evs {
        if matches!(ev, Event::End(TagEnd::Paragraph)) {
            if range.end < input.len() {
                saw_first_paragraph_end_before_eof = true;
                break;
            }
        }
    }
    assert!(
        saw_first_paragraph_end_before_eof,
        "expected paragraph End with range.end < input.len(); events: {:?}",
        evs.iter()
            .map(|(e, r)| (format!("{:?}", e), r.clone()))
            .collect::<Vec<_>>()
    );
    let auth = authoritative_end(input, false);
    assert!(
        auth > 0 && auth < input.len(),
        "authoritative end must be in (0, len); got {}",
        auth
    );
}

#[test]
fn claim_3_setext_promotion_emits_heading_not_paragraph() {
    // Spec claim (Section 5.2): once `===` arrives, pulldown emits End(Heading)
    // covering both lines — NOT End(Paragraph) then End(Heading).
    let input = "Hello\n===\nbody";
    let evs = events(input);
    let paragraph_ends: Vec<_> = evs
        .iter()
        .filter(|(e, _)| matches!(e, Event::End(TagEnd::Paragraph)))
        .collect();
    let heading_ends: Vec<_> = evs
        .iter()
        .filter(|(e, _)| matches!(e, Event::End(TagEnd::Heading(_))))
        .collect();
    // pulldown must emit a heading End for "Hello\n===" and the following
    // paragraph End for "body".
    assert_eq!(
        heading_ends.len(),
        1,
        "expected one heading End, got {:?}",
        heading_ends
            .iter()
            .map(|(e, r)| (format!("{:?}", e), r.clone()))
            .collect::<Vec<_>>()
    );
    // There will be ONE paragraph End for "body" (which is at EOF).
    assert_eq!(
        paragraph_ends.len(),
        1,
        "expected one paragraph End for trailing 'body'"
    );
    let (_, heading_range) = heading_ends[0];
    assert!(
        heading_range.end < input.len(),
        "heading range.end {} must be < input.len() {}",
        heading_range.end,
        input.len()
    );
}

#[test]
fn claim_4_setext_at_eof_has_range_end_equal_len() {
    // Critical case from Section 5.2: just "Hello\n===\n" with no trailing
    // content → End(Heading) at range.end == len → NOT authoritative.
    let input = "Hello\n===\n";
    let evs = events(input);
    let heading_ends: Vec<_> = evs
        .iter()
        .filter(|(e, _)| matches!(e, Event::End(TagEnd::Heading(_))))
        .collect();
    assert_eq!(heading_ends.len(), 1);
    let (_, range) = heading_ends[0];
    assert_eq!(
        range.end,
        input.len(),
        "setext heading at EOF must have range.end == len (non-authoritative)"
    );
}

#[test]
fn claim_5_unclosed_fence_at_eof_range_end_equals_len() {
    // Spec claim: an unclosed fence auto-closes at EOF; End(CodeBlock) lands
    // at range.end == len → NOT authoritative under the cursor rule.
    let input = "```rust\nfn main() {\n";
    let evs = events(input);
    let cb_ends: Vec<_> = evs
        .iter()
        .filter(|(e, _)| matches!(e, Event::End(TagEnd::CodeBlock)))
        .collect();
    assert_eq!(cb_ends.len(), 1);
    let (_, range) = cb_ends[0];
    assert_eq!(
        range.end,
        input.len(),
        "unclosed fence End must be at EOF (non-authoritative)"
    );
    assert_eq!(authoritative_end(input, false), 0);
}

#[test]
fn claim_6_closed_fence_with_content_after_is_authoritative() {
    let input = "```rust\nfn x() {}\n```\nmore";
    let auth = authoritative_end(input, false);
    assert!(
        auth > 0 && auth < input.len(),
        "closed fence with trailing content must advance cursor; got {} (len {})",
        auth,
        input.len()
    );
    // Verify auth lands at or past the closing backticks. pulldown's End
    // range may or may not include the trailing newline after the fence;
    // either is fine semantically — we only assert the fence content is
    // behind the cursor.
    let body_end_pos = input.find("```").expect("closing ``` exists");
    // Find the SECOND ``` (the closing one).
    let closing_pos = input[body_end_pos + 3..]
        .find("```")
        .map(|i| body_end_pos + 3 + i)
        .unwrap();
    assert!(
        auth >= closing_pos,
        "cursor {} must be at or past closing backticks at {}",
        auth,
        closing_pos
    );
}

#[test]
fn claim_7_open_list_at_eof_not_authoritative() {
    // Spec claim (Section 5.3): list_in_progress renders as tail plain until
    // the list CLOSES. At EOF, the list End is at range.end == len.
    let input = "- item1\n- item2\n";
    let evs = events(input);
    let list_ends: Vec<_> = evs
        .iter()
        .filter(|(e, _)| matches!(e, Event::End(TagEnd::List(_))))
        .collect();
    assert_eq!(list_ends.len(), 1);
    let (_, range) = list_ends[0];
    assert_eq!(
        range.end,
        input.len(),
        "list at EOF must have range.end == len"
    );
    assert_eq!(
        authoritative_end(input, false),
        0,
        "no cursor advance while list is open at EOF"
    );
}

#[test]
fn claim_8_list_terminated_by_blank_and_block_is_authoritative() {
    let input = "- item1\n- item2\n\nnext paragraph";
    let auth = authoritative_end(input, false);
    assert!(
        auth > 0 && auth <= input.find("next").unwrap(),
        "cursor must advance past the list into the blank separator; got {}",
        auth
    );
}

// ──────────────────── Prefix-stability under append ────────────────────

#[test]
fn claim_9_prefix_events_stable_under_append() {
    // Spec claim (Section 1 "Why it works"): events whose range ends strictly
    // before a prior EOF must have identical ranges when parsed again with
    // additional content appended.
    let prefix = "Hello\n\nworld\n\n";
    let full = "Hello\n\nworld\n\nnewer content here";

    // Collect End events at range.end < prefix.len() from both parses.
    fn authoritative_ends(s: &str) -> Vec<(String, std::ops::Range<usize>)> {
        Parser::new_ext(s, Options::empty())
            .into_offset_iter()
            .filter_map(|(ev, r)| match ev {
                Event::End(tag) => {
                    if r.end < s.len() {
                        Some((format!("{:?}", tag), r))
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect()
    }

    let prefix_ends = authoritative_ends(prefix);
    let full_ends = authoritative_ends(full);

    // Every End present in the prefix parse must be present, with identical
    // offset, in the full parse.
    for (tag, range) in &prefix_ends {
        assert!(
            full_ends.iter().any(|(t, r)| t == tag && r == range),
            "prefix End {:?}@{:?} not preserved in full parse: {:?}",
            tag,
            range,
            full_ends
        );
    }
}

#[test]
fn claim_10_setext_retroactive_does_not_corrupt_prior_commitment() {
    // Setext promotion is the classic retroactive scenario. Under the cursor
    // rule (range.end < len), we never commit a paragraph that could later
    // be promoted to a setext heading.
    //
    // Verify: at no intermediate append does an authoritative End exist that
    // would later be contradicted.
    let steps = [
        "Hello",
        "Hello\n",
        "Hello\n=",
        "Hello\n==",
        "Hello\n===",
        "Hello\n===\n",
    ];
    for input in &steps {
        let auth = authoritative_end(input, false);
        assert_eq!(
            auth, 0,
            "during setext build-up, no authoritative end should exist; step={:?} auth={}",
            input, auth
        );
    }
    // After a trailing paragraph, the setext is closed and cursor may advance.
    let finalized = "Hello\n===\n\nbody";
    let auth = authoritative_end(finalized, false);
    assert!(
        auth > 0,
        "once setext is followed by content, cursor advances"
    );
    assert!(auth <= finalized.find("body").unwrap());
}

// ──────────────────── Busy-loop regression reproducer ────────────────────

#[test]
fn claim_11_fast_path_heuristic_fires_on_blank_line_inside_open_fence() {
    // Section 3 bug case: unclosed fence containing \n\n. The heuristic
    // fires on \n\n but the parse does NOT advance cursor (range.end == len).
    // This is the pattern the `dirty_since.is_some()` guard must break.
    let input = "```rust\nfn main() {\n\n    body\n\n}\n";

    // Heuristic match (paragraph pattern):
    let has_nn_with_content = input
        .rfind("\n\n")
        .map(|i| i + 2 < input.len())
        .unwrap_or(false);
    assert!(
        has_nn_with_content,
        "heuristic `\\n\\n with content after` fires on this input"
    );

    // Cursor does NOT advance (confirms the bug):
    let auth = authoritative_end(input, false);
    assert_eq!(
        auth, 0,
        "cursor cannot advance: open fence with body at EOF has no authoritative End. \
         This is the pattern that would cause busy-loop without the dirty_since guard."
    );
}

// ────────────── UTF-8 char-boundary invariant ──────────────

#[test]
fn claim_12_range_ends_are_utf8_char_boundaries() {
    // Spec claim (Section 5.6): flushed_byte_len is always on a char boundary
    // because pulldown's range.end is.
    let input = "abc 🎉\n\n漢字 test\n\ntrailing";
    for (_ev, range) in Parser::new_ext(input, Options::empty()).into_offset_iter() {
        assert!(
            input.is_char_boundary(range.start),
            "range.start {} not on char boundary in {:?}",
            range.start,
            input
        );
        assert!(
            input.is_char_boundary(range.end),
            "range.end {} not on char boundary in {:?}",
            range.end,
            input
        );
    }
    // Slicing at the authoritative end must produce valid UTF-8.
    let auth = authoritative_end(input, false);
    let _prefix = &input[..auth]; // would panic if not on boundary
    let _tail = &input[auth..];
}

// ────────────── TurnComplete EOF-permissive vs normal rule ──────────────

#[test]
fn claim_13_flush_final_permits_eof_closure() {
    // Section 5.1: flush_final should commit a trailing fenced code block
    // that closes exactly at EOF.
    let input = "intro\n\n```rust\nfn x() {}\n```";
    let normal_auth = authoritative_end(input, false);
    let final_auth = authoritative_end(input, true);
    assert!(
        final_auth > normal_auth,
        "EOF-permissive rule should advance cursor further; normal={} final={}",
        normal_auth,
        final_auth
    );
    assert_eq!(
        final_auth,
        input.len(),
        "EOF-permissive rule commits everything at TurnComplete"
    );
}

// ────────────── Fast-path declines at EOF-only pattern ──────────────

#[test]
fn claim_14_double_newline_at_eof_heuristic_declines() {
    // Section 3 false-positive avoidance: "\n\n" at exact EOF must NOT trigger
    // fast path (no content after last \n\n).
    let input = "paragraph\n\n";
    let has_nn_with_content = input
        .rfind("\n\n")
        .map(|i| i + 2 < input.len())
        .unwrap_or(false);
    assert!(
        !has_nn_with_content,
        "heuristic must decline when \\n\\n is at EOF"
    );
}

#[test]
fn claim_16_naive_rule_mis_commits_nested_list_item() {
    // Regression guard: this is the failure mode that forced the depth-0 gate.
    // The naive "range.end < len for any End" rule advances past End(Item)
    // inside an open list, which would retroactively be wrong if the list
    // later became loose.
    let input = "- item1\n- item2\n";
    let naive = naive_authoritative_end(input);
    let correct = authoritative_end(input, false);
    assert!(naive > 0, "naive rule commits at byte {} (bug)", naive);
    assert_eq!(
        correct, 0,
        "depth-gated rule correctly refuses commit while list is open"
    );
}

#[test]
fn claim_17_depth_gate_allows_top_level_paragraph_end() {
    // Positive case: a plain paragraph at the top level closes cleanly.
    // depth goes 0→1 at Start(Paragraph), then 1→0 at End(Paragraph).
    // We expect cursor advance.
    let input = "first para\n\nsecond para";
    let auth = authoritative_end(input, false);
    assert!(
        auth > 0 && auth < input.len(),
        "top-level paragraph End with depth→0 must be authoritative; got {}",
        auth
    );
    assert!(auth <= input.find("second").unwrap());
}

#[test]
fn claim_18_depth_gate_blocks_paragraph_inside_blockquote() {
    // A paragraph inside a blockquote closes at depth 2→1, not to 0.
    // Only End(BlockQuote) at the outer level should advance the cursor.
    let input = "> quoted line\n> more\n\nafter";
    let auth = authoritative_end(input, false);
    assert!(auth > 0, "blockquote must eventually close authoritatively");
    // The cursor advance must be at or past the blockquote's End
    // (which closes depth 1→0). It should NOT equal the position of an
    // inner End(Paragraph).
    let bq_content_end = input.find("\n\n").unwrap(); // blank line separator
    assert!(
        auth >= bq_content_end,
        "cursor {} must be at or past blockquote outer close at {}",
        auth,
        bq_content_end
    );
}

#[test]
fn claim_15_fence_close_heuristic_requires_trailing_content() {
    // Fence close at EOF (no content after the closing ```\n) must NOT trigger
    // fast path.
    let at_eof = "```\ncode\n```\n";
    let after = at_eof.find("\n```").map(|i| i + 4).unwrap();
    let fires_at_eof = at_eof.as_bytes().get(after) == Some(&b'\n') && after + 1 < at_eof.len();
    assert!(
        !fires_at_eof,
        "fence-close heuristic must decline at EOF; input={:?}, after={}",
        at_eof, after
    );

    let with_content = "```\ncode\n```\nmore";
    let after2 = with_content.find("\n```").map(|i| i + 4).unwrap();
    let fires_with_content =
        with_content.as_bytes().get(after2) == Some(&b'\n') && after2 + 1 < with_content.len();
    assert!(
        fires_with_content,
        "fence-close heuristic must fire with trailing content"
    );
}

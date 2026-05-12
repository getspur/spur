mod common;

use crossterm::event::KeyCode;
use std::time::Duration;

use common::TestHarness;

fn enable_paste_burst(h: &mut TestHarness) {
    h.app_mut()
        .dashboard_mut_for_test()
        .enable_paste_burst_for_test(true);
}

#[test]
fn rapid_char_enter_paste_burst_does_not_submit() {
    let mut h = TestHarness::new(80, 24);
    enable_paste_burst(&mut h);

    h.send_key(KeyCode::Char('f'));
    h.send_key(KeyCode::Char('n'));
    h.send_key(KeyCode::Char(' '));
    h.send_key(KeyCode::Enter);
    h.send_key(KeyCode::Char('m'));
    h.send_key(KeyCode::Char('a'));
    h.send_key(KeyCode::Char('i'));
    h.send_key(KeyCode::Char('n'));

    assert!(
        h.take_actions().is_empty(),
        "raw rapid paste must not submit when Enter arrives as a key event"
    );

    std::thread::sleep(Duration::from_millis(80));
    h.app_mut().tick();

    let text = h
        .app_mut()
        .dashboard_mut_for_test()
        .input_bar_text_for_test();
    assert!(
        text.contains("[Paste #1 · 2 lines]"),
        "tick should flush buffered raw paste through insert_paste, got {text:?}"
    );
}

#[test]
fn alphanumeric_end_paste_burst_does_not_submit() {
    let mut h = TestHarness::new(80, 24);
    enable_paste_burst(&mut h);

    for code in [
        KeyCode::Char('h'),
        KeyCode::Char('e'),
        KeyCode::Char('l'),
        KeyCode::Char('l'),
        KeyCode::Char('o'),
        KeyCode::Enter,
        KeyCode::Char('w'),
        KeyCode::Char('o'),
        KeyCode::Char('r'),
        KeyCode::Char('l'),
        KeyCode::Char('d'),
    ] {
        h.send_key(code);
    }

    assert!(
        h.take_actions().is_empty(),
        "alphanumeric-ended paste must not submit"
    );

    std::thread::sleep(Duration::from_millis(80));
    h.app_mut().tick();

    let text = h
        .app_mut()
        .dashboard_mut_for_test()
        .input_bar_text_for_test();
    assert!(
        text.contains("[Paste #1 · 2 lines]"),
        "idle flush should atomize the buffered multi-line paste, got {text:?}"
    );
}

#[test]
fn single_line_large_paste_buffers_after_arming() {
    let mut h = TestHarness::new(80, 24);
    enable_paste_burst(&mut h);
    let pasted = "x".repeat(200);

    h.type_text(&pasted);

    let before_flush = h
        .app_mut()
        .dashboard_mut_for_test()
        .input_bar_text_for_test();
    assert_eq!(
        before_flush, "xxx",
        "only the pre-armed prefix should be visible before idle flush"
    );

    std::thread::sleep(Duration::from_millis(80));
    h.app_mut().tick();

    let text = h
        .app_mut()
        .dashboard_mut_for_test()
        .input_bar_text_for_test();
    assert_eq!(text, pasted);
}

#[test]
fn buffer_overflow_force_flushes_without_losing_text() {
    let mut h = TestHarness::new(80, 24);
    enable_paste_burst(&mut h);

    for _ in 0..600_000 {
        h.send_key(KeyCode::Char('x'));
    }

    std::thread::sleep(Duration::from_millis(80));
    h.app_mut().tick();

    let text = h
        .app_mut()
        .dashboard_mut_for_test()
        .input_bar_text_for_test();
    assert_eq!(text.len(), 600_000);
    assert!(text.bytes().all(|byte| byte == b'x'));
}

#[test]
fn at_prefixed_paste_burst_does_not_submit() {
    let mut h = TestHarness::new(80, 24);
    enable_paste_burst(&mut h);

    for code in [
        KeyCode::Char('@'),
        KeyCode::Char('z'),
        KeyCode::Char('z'),
        KeyCode::Char('z'),
        KeyCode::Char('z'),
        KeyCode::Enter,
        KeyCode::Char('x'),
        KeyCode::Char('y'),
        KeyCode::Char('z'),
    ] {
        h.send_key(code);
    }

    assert!(
        h.take_actions().is_empty(),
        "@-prefixed paste must not submit"
    );

    std::thread::sleep(Duration::from_millis(80));
    h.app_mut().tick();

    let text = h
        .app_mut()
        .dashboard_mut_for_test()
        .input_bar_text_for_test();
    assert!(
        text.contains("[Paste #1 · 2 lines]"),
        "idle flush should atomize the @-prefixed multi-line paste, got {text:?}"
    );
}

#[test]
fn paste_burst_is_disabled_by_default_in_tests() {
    let mut h = TestHarness::new(80, 24);

    for code in [
        KeyCode::Char('h'),
        KeyCode::Char('e'),
        KeyCode::Char('l'),
        KeyCode::Char('l'),
        KeyCode::Char('o'),
        KeyCode::Enter,
    ] {
        h.send_key(code);
    }

    let actions = h.take_actions();
    assert_eq!(
        actions.len(),
        1,
        "without explicit test opt-in, rapid keys should behave normally"
    );
}

use spur_bot::telegram::poll_loop::advance_offset;

#[test]
fn offset_advances_only_after_accepted_batch() {
    assert_eq!(advance_offset(100, &[101, 102], true), 103);
    assert_eq!(advance_offset(100, &[101, 102], false), 100);
}

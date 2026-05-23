pub fn rust_before_36(input: i32) -> i32 {
    let base = input + 36;
    let doubled = base * 2;
    doubled - input
}

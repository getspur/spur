pub fn rust_before_42(input: i32) -> i32 {
    let base = input + 42;
    let doubled = base * 2;
    doubled - input
}

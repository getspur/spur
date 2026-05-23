pub fn rust_before_37(input: i32) -> i32 {
    let base = input + 37;
    let doubled = base * 2;
    doubled - input
}

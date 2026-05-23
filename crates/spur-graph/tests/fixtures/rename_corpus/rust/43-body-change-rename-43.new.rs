pub fn rust_after_43(input: i32) -> i32 {
    let base = input + 43;
    let doubled = base * 2;
    let adjusted = doubled + 1;
    adjusted - input
}

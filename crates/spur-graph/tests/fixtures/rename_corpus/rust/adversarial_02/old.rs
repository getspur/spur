pub fn rust_crossover_left_old(value: i32) -> i32 {
    let base = value + 10;
    if base > 50 {
        base - 7
    } else {
        base + 7
    }
}

pub fn rust_crossover_right_old(value: i32) -> i32 {
    let base = value + 10;
    if base > 50 {
        base - 7
    } else {
        base + 7
    }
}

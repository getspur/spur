pub fn rust_full_rewrite_before(alpha_count: i32, beta_limit: i32) -> i32 {
    let mut total = alpha_count;
    for step in 0..beta_limit {
        total += step * 3;
        if total % 2 == 0 {
            total -= 1;
        }
    }
    total
}

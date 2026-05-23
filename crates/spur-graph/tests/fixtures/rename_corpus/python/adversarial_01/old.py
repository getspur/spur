def py_full_rewrite_before(alpha_count: int, beta_limit: int) -> int:
    total = alpha_count
    for step in range(beta_limit):
        total += step * 3
        if total % 2 == 0:
            total -= 1
    return total

def py_crossover_alpha_new(value: int) -> int:
    base = value + 10
    if base > 50:
        return base - 7
    return base + 7


def py_crossover_beta_new(value: int) -> int:
    base = value + 10
    if base > 50:
        return base - 7
    return base + 7

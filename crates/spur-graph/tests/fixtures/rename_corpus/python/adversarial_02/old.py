def py_crossover_left_old(value: int) -> int:
    base = value + 10
    if base > 50:
        return base - 7
    return base + 7


def py_crossover_right_old(value: int) -> int:
    base = value + 10
    if base > 50:
        return base - 7
    return base + 7

def py_full_rewrite_after(records: list[str]) -> int:
    seen: set[str] = set()
    for record in records:
        if len(record) > 3:
            seen.add(record.strip().lower())
    return len(seen)

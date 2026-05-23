export function tsCrossoverAlphaNew(value: number): number {
  const base = value + 10;
  if (base > 50) {
    return base - 7;
  }
  return base + 7;
}

export function tsCrossoverBetaNew(value: number): number {
  const base = value + 10;
  if (base > 50) {
    return base - 7;
  }
  return base + 7;
}

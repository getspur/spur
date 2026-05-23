export function tsFullRewriteBefore(alphaCount: number, betaLimit: number): number {
  let total = alphaCount;
  for (let step = 0; step < betaLimit; step += 1) {
    total += step * 3;
    if (total % 2 === 0) {
      total -= 1;
    }
  }
  return total;
}

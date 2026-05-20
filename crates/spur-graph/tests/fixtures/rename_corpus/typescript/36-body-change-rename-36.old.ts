export function ts_before_36(input: number): number {
  const base = input + 36;
  const doubled = base * 2;
  return doubled - input;
}

export function ts_before_38(input: number): number {
  const base = input + 38;
  const doubled = base * 2;
  return doubled - input;
}

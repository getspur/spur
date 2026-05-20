export function ts_before_40(input: number): number {
  const base = input + 40;
  const doubled = base * 2;
  return doubled - input;
}

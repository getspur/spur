export function ts_before_37(input: number): number {
  const base = input + 37;
  const doubled = base * 2;
  return doubled - input;
}

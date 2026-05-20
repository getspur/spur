export function ts_before_42(input: number): number {
  const base = input + 42;
  const doubled = base * 2;
  return doubled - input;
}

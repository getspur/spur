export function ts_old_32(input: number): number {
  const base = input + 32;
  const doubled = base * 2;
  return doubled - input;
}

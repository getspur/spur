export function ts_after_36(input: number): number {
  const base = input + 36;
  const doubled = base * 2;
  const adjusted = doubled + 1;
  return adjusted - input;
}

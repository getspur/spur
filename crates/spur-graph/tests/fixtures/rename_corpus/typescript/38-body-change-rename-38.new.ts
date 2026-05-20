export function ts_after_38(input: number): number {
  const base = input + 38;
  const doubled = base * 2;
  const adjusted = doubled + 1;
  return adjusted - input;
}

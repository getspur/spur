export function ts_after_40(input: number): number {
  const base = input + 40;
  const doubled = base * 2;
  const adjusted = doubled + 1;
  return adjusted - input;
}

export function ts_after_37(input: number): number {
  const base = input + 37;
  const doubled = base * 2;
  const adjusted = doubled + 1;
  return adjusted - input;
}

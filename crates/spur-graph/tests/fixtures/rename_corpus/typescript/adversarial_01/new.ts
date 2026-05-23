export function tsFullRewriteAfter(records: string[]): number {
  const seen = new Set<string>();
  for (const record of records) {
    if (record.length > 3) {
      seen.add(record.trim().toLowerCase());
    }
  }
  return seen.size;
}

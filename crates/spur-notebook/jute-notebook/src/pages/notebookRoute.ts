type NotebookRouteTab = {
  path?: string;
};

export function notebookRouteForPaths(
  paths: readonly string[],
  activePath?: string,
  pinnedPaths: readonly string[] = [],
): string {
  const params = new URLSearchParams();
  for (const path of uniquePaths(paths)) {
    params.append("path", path);
  }
  for (const path of uniquePaths(pinnedPaths)) {
    params.append("pinned", path);
  }
  if (activePath) {
    params.set("active", activePath);
  }
  const query = params.toString();
  return query ? `/notebook?${query}` : "/notebook";
}

export function notebookRouteWithPath(
  tabs: readonly NotebookRouteTab[],
  path: string,
  pinnedPaths: readonly string[] = [],
): string {
  const paths = tabs.flatMap((tab) => (tab.path ? [tab.path] : []));
  if (!paths.includes(path)) {
    paths.push(path);
  }
  return notebookRouteForPaths(paths, path, pinnedPaths);
}

export function activeTabIdFromSearch(search: string): string | undefined {
  return new URLSearchParams(search).get("active") ?? undefined;
}

export function pinnedPathsFromSearch(search: string): string[] {
  return new URLSearchParams(search).getAll("pinned");
}

function uniquePaths(paths: readonly string[]): string[] {
  const seen = new Set<string>();
  return paths.filter((path) => {
    if (seen.has(path)) return false;
    seen.add(path);
    return true;
  });
}

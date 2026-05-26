export function isMermaidLanguageClassName(className: string | undefined) {
  return className?.startsWith("language-mermaid") ?? false;
}

export function htmlOutputSandbox(activeContent: boolean): string {
  return activeContent ? "allow-scripts" : "";
}

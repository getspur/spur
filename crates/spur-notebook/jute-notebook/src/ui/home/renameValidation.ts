export type RenameValidationResult =
  | { ok: true; fileName: string }
  | { ok: false; error: string };

export function validateRename(input: string): RenameValidationResult {
  const trimmed = input.trim();

  if (!trimmed) {
    return { ok: false, error: "Notebook name must not be empty." };
  }

  if (/[\\/]/.test(trimmed)) {
    return {
      ok: false,
      error: "Notebook name must not contain path separators.",
    };
  }

  return {
    ok: true,
    fileName: trimmed.toLowerCase().endsWith(".ipynb")
      ? trimmed
      : `${trimmed}.ipynb`,
  };
}

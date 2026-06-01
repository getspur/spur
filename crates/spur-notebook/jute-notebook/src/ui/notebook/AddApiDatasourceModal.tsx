import { useEffect, useState } from "react";

type ApiDatasourceAdapter = {
  label: string;
  value: string;
};

const API_DATASOURCE_ADAPTERS: ApiDatasourceAdapter[] = [
  { label: "Polymarket", value: "polymarket" },
];

const DEFAULT_SOURCE = API_DATASOURCE_ADAPTERS[0]?.value ?? "";

export type AddApiDatasourceModalProps = {
  onAdd: (name: string, source: string) => Promise<void>;
  onCancel: () => void;
  open: boolean;
};

export default function AddApiDatasourceModal({
  onAdd,
  onCancel,
  open,
}: AddApiDatasourceModalProps) {
  const [source, setSource] = useState(DEFAULT_SOURCE);
  const [name, setName] = useState(DEFAULT_SOURCE);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    const handler = (event: KeyboardEvent) => {
      if (event.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [open, onCancel]);

  useEffect(() => {
    if (!open) return;
    setSource(DEFAULT_SOURCE);
    setName(DEFAULT_SOURCE);
    setPending(false);
    setError(null);
  }, [open]);

  if (!open) return null;

  const handleSubmit = async (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setPending(true);
    setError(null);

    try {
      await onAdd(name, source);
    } catch (caught) {
      setError(
        caught instanceof Error
          ? caught.message
          : "Unable to add API datasource",
      );
    } finally {
      setPending(false);
    }
  };

  return (
    <div
      aria-modal="true"
      className="fixed inset-0 z-40 flex items-center justify-center bg-black/30"
      onClick={onCancel}
      role="dialog"
    >
      <form
        className="w-full max-w-md rounded border border-gray-300 bg-white p-5 shadow-xl"
        onClick={(event) => event.stopPropagation()}
        onSubmit={(event) => void handleSubmit(event)}
      >
        <h2 className="text-lg text-gray-950">Add API datasource</h2>
        <div className="mt-4 space-y-3">
          <label className="block">
            <span className="text-xs font-medium uppercase text-gray-500">
              Source
            </span>
            <select
              aria-label="Source"
              className="mt-1 h-8 w-full rounded border border-gray-300 bg-white px-2 text-sm text-gray-900 outline-none transition-colors focus:border-gray-900"
              onChange={(event) => {
                const nextSource = event.currentTarget.value;
                setSource(nextSource);
                setName((currentName) =>
                  currentName === source ? nextSource : currentName,
                );
              }}
              value={source}
            >
              {API_DATASOURCE_ADAPTERS.map((adapter) => (
                <option key={adapter.value} value={adapter.value}>
                  {adapter.label}
                </option>
              ))}
            </select>
          </label>
          <label className="block">
            <span className="text-xs font-medium uppercase text-gray-500">
              Name
            </span>
            <input
              aria-label="Name"
              className="mt-1 h-8 w-full rounded border border-gray-300 bg-white px-2 text-sm text-gray-900 outline-none transition-colors placeholder:text-gray-400 focus:border-gray-900"
              onChange={(event) => setName(event.currentTarget.value)}
              value={name}
            />
          </label>
          {error ? (
            <p className="rounded border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700">
              {error}
            </p>
          ) : null}
        </div>
        <div className="mt-5 flex items-center justify-end gap-2">
          <button
            className="rounded border border-gray-300 px-3 py-2 text-sm text-gray-600 transition-colors hover:border-black hover:text-gray-950"
            onClick={onCancel}
            type="button"
          >
            Cancel
          </button>
          <button
            autoFocus
            className="rounded bg-gray-950 px-3 py-2 text-sm text-white transition-colors hover:bg-black disabled:cursor-not-allowed disabled:bg-gray-400"
            disabled={pending}
            type="submit"
          >
            {pending ? "Adding..." : "Add"}
          </button>
        </div>
      </form>
    </div>
  );
}

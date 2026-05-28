import { useEffect } from "react";

export type ConfirmModalProps = {
  body: string;
  confirmLabel: string;
  danger?: boolean;
  onCancel: () => void;
  onConfirm: () => void;
  open: boolean;
  title: string;
};

export default function ConfirmModal({
  body,
  confirmLabel,
  danger = false,
  onCancel,
  onConfirm,
  open,
  title,
}: ConfirmModalProps) {
  useEffect(() => {
    if (!open) return;
    const handler = (event: KeyboardEvent) => {
      if (event.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [open, onCancel]);

  if (!open) return null;

  return (
    <div
      aria-modal="true"
      className="fixed inset-0 z-40 flex items-center justify-center bg-black/30"
      onClick={onCancel}
      role="dialog"
    >
      <div
        className="w-full max-w-md rounded border border-gray-300 bg-white p-5 shadow-xl"
        onClick={(event) => event.stopPropagation()}
      >
        <h2 className="text-lg text-gray-950">{title}</h2>
        <p className="mt-2 text-sm text-gray-600">{body}</p>
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
            className={[
              "rounded px-3 py-2 text-sm text-white transition-colors",
              danger ? "bg-red-600 hover:bg-red-700" : "bg-gray-950 hover:bg-black",
            ].join(" ")}
            onClick={onConfirm}
            type="button"
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

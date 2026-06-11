import { useEffect, useRef } from "react";

import type { NotebookTab } from "@/stores/notebook";

type Props = {
  canReopen: boolean;
  closeOthersCount: number;
  closeRightCount: number;
  onClose: () => void;
  onCloseOthers: () => void;
  onCloseRight: () => void;
  onCopyPath: () => void;
  onDismiss: () => void;
  onReopenClosed: () => void;
  onTogglePin: () => void;
  position: { x: number; y: number };
  tab: NotebookTab;
};

type ItemOptions = {
  disabled?: boolean;
  kbd?: string;
};

export default function TabContextMenu(props: Props) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleMouseDown = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (ref.current?.contains(target)) return;
      props.onDismiss();
    };

    document.addEventListener("mousedown", handleMouseDown);
    return () => document.removeEventListener("mousedown", handleMouseDown);
  }, [props]);

  const item = (
    label: string,
    onClick: (() => void) | undefined,
    options: ItemOptions = {},
  ) => (
    <button
      aria-label={options.kbd ? `${label} ${options.kbd}` : label}
      className="flex w-full items-center justify-between gap-4 rounded px-2.5 py-1.5 text-left text-xs text-gray-900 enabled:hover:bg-gray-100 disabled:text-gray-400"
      disabled={options.disabled || !onClick}
      onClick={() => {
        onClick?.();
        props.onDismiss();
      }}
      role="menuitem"
      type="button"
    >
      <span>{label}</span>
      {options.kbd && (
        <span className="font-mono text-[10px] text-gray-400">
          {options.kbd}
        </span>
      )}
    </button>
  );

  const separator = <div className="mx-1.5 my-1 h-px bg-gray-200" />;

  return (
    <div
      className="fixed z-50 min-w-[228px] rounded-lg border border-gray-200 bg-white p-1 shadow-lg"
      ref={ref}
      role="menu"
      style={{ left: props.position.x, top: props.position.y }}
    >
      {item(props.tab.pinned ? "Unpin tab" : "Pin tab", props.onTogglePin)}
      {item("Duplicate", undefined, { disabled: true })}
      {separator}
      {item("Close", props.onClose, {
        kbd: props.tab.pinned ? undefined : "⌘W",
      })}
      {item(`Close others (${props.closeOthersCount})`, props.onCloseOthers, {
        disabled: props.closeOthersCount === 0,
      })}
      {item(
        `Close to the right (${props.closeRightCount})`,
        props.onCloseRight,
        { disabled: props.closeRightCount === 0 },
      )}
      {item("Reopen closed tab", props.onReopenClosed, {
        disabled: !props.canReopen,
        kbd: "⌘⇧T",
      })}
      {separator}
      {item("Copy path", props.onCopyPath, { disabled: !props.tab.path })}
      {item("Move to new window", undefined, { disabled: true })}
    </div>
  );
}

import clsx from "clsx";
import { type RefObject, useEffect, useRef } from "react";

import type { CodeType } from "@/bindings";
import type { CellType } from "@/stores/notebook";

import {
  CELL_LANGUAGE_TOKENS,
  CODE_LANGUAGE_ORDER,
  type CellLanguageId,
} from "./cellLanguage";

type TextCellType = "markdown";

type TextTypeOption =
  | {
      type: TextCellType;
      label: string;
      glyph: string;
      disabled?: false;
      title?: undefined;
    }
  | {
      type: "raw";
      label: string;
      glyph: string;
      disabled: true;
      title: string;
    };

type CellLanguageMenuProps = {
  currentLanguageId: CellLanguageId;
  currentType: CellType;
  onClose: () => void;
  onSelectCodeType: (codeType: CodeType) => void;
  onSelectType: (type: TextCellType) => void;
  anchorRef?: RefObject<HTMLElement | null>;
};

const AGENT_DISABLED_TITLE = "Agent cells require backend wiring (bd-1bpb)";
const RAW_DISABLED_TITLE = "Raw cells not supported yet";

const TEXT_TYPE_OPTIONS: TextTypeOption[] = [
  { type: "markdown", label: "Markdown", glyph: "Md" },
  {
    type: "raw",
    label: "Raw",
    glyph: "Txt",
    disabled: true,
    title: RAW_DISABLED_TITLE,
  },
];

function isCodeType(id: CellLanguageId): id is CodeType {
  return id !== "spur";
}

export default function CellLanguageMenu({
  anchorRef,
  currentLanguageId,
  currentType,
  onClose,
  onSelectCodeType,
  onSelectType,
}: CellLanguageMenuProps) {
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleMouseDown = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (menuRef.current?.contains(target)) return;
      if (anchorRef?.current?.contains(target)) return;
      onClose();
    };

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      onClose();
    };

    document.addEventListener("mousedown", handleMouseDown);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("mousedown", handleMouseDown);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [anchorRef, onClose]);

  const selectCodeType = (codeType: CodeType) => {
    onSelectCodeType(codeType);
    onClose();
  };

  const selectType = (type: TextCellType) => {
    onSelectType(type);
    onClose();
  };

  return (
    <div
      ref={menuRef}
      aria-label="Cell language"
      className="absolute left-0 top-full z-30 mt-1 w-44 overflow-hidden rounded-md border border-gray-200 bg-white py-1 text-xs shadow-lg"
      role="menu"
    >
      {CODE_LANGUAGE_ORDER.map((id) => {
        const token = CELL_LANGUAGE_TOKENS[id];
        const disabled = id === "spur";
        const selected = currentType === "code" && currentLanguageId === id;
        return (
          <button
            key={id}
            aria-current={selected ? "true" : undefined}
            className={clsx(
              "flex w-full items-center gap-2 px-2 py-1.5 text-left font-mono transition-colors",
              selected
                ? "bg-violet-600 text-white"
                : "text-gray-700 hover:bg-gray-100",
              disabled && "cursor-not-allowed opacity-50 hover:bg-white",
            )}
            disabled={disabled}
            onClick={() => {
              if (isCodeType(id)) selectCodeType(id);
            }}
            role="menuitem"
            title={disabled ? AGENT_DISABLED_TITLE : undefined}
            type="button"
          >
            <span
              aria-hidden="true"
              className="inline-flex h-[18px] w-[24px] shrink-0 items-center justify-center rounded text-[9px] font-semibold"
              style={{
                background: selected ? "rgba(255,255,255,0.18)" : token.glyphBg,
                color: selected ? "inherit" : token.chipText,
              }}
            >
              {token.glyph}
            </span>
            <span className="truncate">{token.label}</span>
          </button>
        );
      })}

      <div className="my-1 border-t border-gray-200" role="separator" />

      {TEXT_TYPE_OPTIONS.map((option) => {
        const disabled = option.disabled === true;
        const selected =
          option.type === "markdown" && currentType === option.type;
        return (
          <button
            key={option.type}
            aria-current={selected ? "true" : undefined}
            className={clsx(
              "flex w-full items-center gap-2 px-2 py-1.5 text-left font-mono transition-colors",
              selected
                ? "bg-violet-600 text-white"
                : "text-gray-700 hover:bg-gray-100",
              disabled && "cursor-not-allowed opacity-50 hover:bg-white",
            )}
            disabled={disabled}
            onClick={() => {
              if (option.type === "markdown") selectType(option.type);
            }}
            role="menuitem"
            title={option.title}
            type="button"
          >
            <span
              aria-hidden="true"
              className={clsx(
                "inline-flex h-[18px] w-[24px] shrink-0 items-center justify-center rounded text-[9px] font-semibold",
                selected
                  ? "bg-white/20 text-white"
                  : "bg-gray-100 text-gray-600",
              )}
            >
              {option.glyph}
            </span>
            <span className="truncate">{option.label}</span>
          </button>
        );
      })}
    </div>
  );
}

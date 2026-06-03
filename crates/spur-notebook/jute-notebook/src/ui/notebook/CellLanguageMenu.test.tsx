import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import CellLanguageMenu from "./CellLanguageMenu";

function renderMenu() {
  const onClose = vi.fn();
  const onSelectCodeType = vi.fn();
  const onSelectType = vi.fn();

  render(
    <CellLanguageMenu
      currentLanguageId="python"
      currentType="code"
      onClose={onClose}
      onSelectCodeType={onSelectCodeType}
      onSelectType={onSelectType}
    />,
  );

  return { onClose, onSelectCodeType, onSelectType };
}

afterEach(() => {
  cleanup();
});

describe("CellLanguageMenu", () => {
  test("renders language and type rows with unsupported rows disabled", () => {
    renderMenu();

    for (const label of [
      "Python",
      "JavaScript",
      "Rust",
      "Go",
      "AI Agent",
      "Markdown",
      "Raw",
    ]) {
      expect(screen.getByRole("menuitem", { name: label })).toBeInTheDocument();
    }

    expect(screen.getByRole("separator")).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "AI Agent" })).toBeDisabled();
    expect(screen.getByRole("menuitem", { name: "AI Agent" })).toHaveAttribute(
      "title",
      "Agent cells require backend wiring (bd-1bpb)",
    );
    expect(screen.getByRole("menuitem", { name: "Raw" })).toBeDisabled();
    expect(screen.getByRole("menuitem", { name: "Raw" })).toHaveAttribute(
      "title",
      "Raw cells not supported yet",
    );
  });

  test("selects code and notebook cell types", () => {
    const { onClose, onSelectCodeType, onSelectType } = renderMenu();

    fireEvent.click(screen.getByRole("menuitem", { name: "Rust" }));
    expect(onSelectCodeType).toHaveBeenCalledWith("rust");
    expect(onClose).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("menuitem", { name: "Markdown" }));
    expect(onSelectType).toHaveBeenCalledWith("markdown");
    expect(onClose).toHaveBeenCalledTimes(2);

    expect(onSelectType).toHaveBeenCalledTimes(1);
  });

  test("closes on outside click and Escape", () => {
    const { onClose } = renderMenu();

    fireEvent.mouseDown(document.body);
    expect(onClose).toHaveBeenCalledTimes(1);

    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(2);
  });
});

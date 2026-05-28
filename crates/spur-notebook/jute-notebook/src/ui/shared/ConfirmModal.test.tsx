import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import ConfirmModal from "./ConfirmModal";

describe("ConfirmModal", () => {
  it("renders nothing when open is false", () => {
    const { container } = render(
      <ConfirmModal
        body="x"
        confirmLabel="y"
        onCancel={() => {}}
        onConfirm={() => {}}
        open={false}
        title="z"
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("fires onConfirm when the confirm button is clicked", () => {
    const onConfirm = vi.fn();
    render(
      <ConfirmModal
        body="Discard 3 notebooks?"
        confirmLabel="Discard"
        onCancel={() => {}}
        onConfirm={onConfirm}
        open
        title="Confirm"
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("fires onCancel when Escape is pressed", () => {
    const onCancel = vi.fn();
    render(
      <ConfirmModal
        body="x"
        confirmLabel="ok"
        onCancel={onCancel}
        onConfirm={() => {}}
        open
        title="t"
      />,
    );
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onCancel).toHaveBeenCalledTimes(1);
  });
});

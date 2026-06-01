import "@testing-library/jest-dom/vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import AddApiDatasourceModal from "./AddApiDatasourceModal";

describe("AddApiDatasourceModal", () => {
  afterEach(() => {
    cleanup();
  });

  test("renders nothing when open is false", () => {
    const { container } = render(
      <AddApiDatasourceModal
        onAdd={async () => {}}
        onCancel={() => {}}
        open={false}
      />,
    );

    expect(container).toBeEmptyDOMElement();
  });

  test("calls onAdd with the selected source and edited name", async () => {
    const onAdd = vi.fn().mockResolvedValue(undefined);

    render(<AddApiDatasourceModal onAdd={onAdd} onCancel={() => {}} open />);

    fireEvent.change(screen.getByLabelText("Name"), {
      target: { value: "prediction" },
    });
    fireEvent.change(screen.getByLabelText("Source"), {
      target: { value: "polymarket" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() =>
      expect(onAdd).toHaveBeenCalledWith("prediction", "polymarket"),
    );
  });

  test("shows an error and stays open when onAdd rejects", async () => {
    const onAdd = vi.fn().mockRejectedValue(new Error("adapter unavailable"));

    render(<AddApiDatasourceModal onAdd={onAdd} onCancel={() => {}} open />);

    fireEvent.click(screen.getByRole("button", { name: "Add" }));

    expect(await screen.findByText("adapter unavailable")).toBeInTheDocument();
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });
});

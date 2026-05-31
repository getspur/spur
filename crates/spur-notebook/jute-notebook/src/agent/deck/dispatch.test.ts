import { describe, expect, test, vi } from "vitest";

import type { Notebook } from "@/stores/notebook";

import { dispatchDeckCommand } from "./dispatch";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

describe("dispatchDeckCommand", () => {
  test("delegates deck work with a compact notebook summary", async () => {
    invokeMock.mockResolvedValue({ delegation_id: "delegation-1" });
    const longSource = "x".repeat(120);
    const notebook = {
      store: {
        getState: () => ({
          serverState: {
            lastAppliedVersion: 0,
            notebookMetadata: {},
            cellIds: ["markdown-1", "code-1"],
            cells: {
              "markdown-1": {
                type: "markdown",
                initialText: longSource,
                source: longSource,
                version: 1,
                juteDeckMetadata: { layout: "title" },
              },
              "code-1": {
                type: "code",
                initialText: "print('hello')",
                source: "print('hello')",
                version: 1,
              },
            },
          },
          viewState: {
            path: "/tmp/deck.ipynb",
            selectedCellId: null,
            isLoading: false,
            viewMode: "cells",
          },
          editBuffer: {
            cellSources: {},
          },
          dagStatus: {},
        }),
      },
    } as unknown as Notebook;

    const result = await dispatchDeckCommand(notebook, "draft", "make a demo");

    expect(result).toEqual({ delegation_id: "delegation-1" });
    expect(invokeMock).toHaveBeenCalledWith("spur_delegate_to_worker", {
      task: expect.stringContaining("User's request: make a demo"),
      workerType: "coder",
      toolAllowlist: ["mcp__notebook__*"],
    });

    const [{ task }] = invokeMock.mock.calls[0].slice(1);
    const summary = JSON.parse(task.split("Current notebook (2 cells):\n")[1]);
    expect(summary).toEqual({
      path: "/tmp/deck.ipynb",
      cells: [
        {
          id: "markdown-1",
          type: "markdown",
          layout: "title",
          preview: "x".repeat(80),
        },
        {
          id: "code-1",
          type: "code",
          layout: "auto",
          preview: "print('hello')",
        },
      ],
    });
  });
});

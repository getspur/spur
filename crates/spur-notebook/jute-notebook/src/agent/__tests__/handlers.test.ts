import { describe, expect, test, vi } from "vitest";

import type { CodeType } from "@/bindings";
import type { Notebook } from "@/stores/notebook";

import { dispatchAgentRequest } from "../handlers";
import { AgentHandlerError } from "../types";

describe("dispatchAgentRequest notebook.insert_cell", () => {
  test.each(["python", "go"] satisfies CodeType[])(
    "accepts code_type %s",
    async (codeType) => {
      const notebook = createNotebook();

      await expect(
        dispatchAgentRequest(notebook, {
          requestId: "request-1",
          method: "notebook.insert_cell",
          params: {
            kind: "code",
            source: "print('hello')",
            code_type: codeType,
          },
        }),
      ).resolves.toEqual({ id: "cell-1", version: 1 });
      expect(notebook.insertCellAfter).toHaveBeenCalledWith(
        undefined,
        "code",
        "print('hello')",
        "brain",
        codeType,
      );
    },
  );

  test("rejects unsupported code_type", async () => {
    await expect(
      dispatchAgentRequest(createNotebook(), {
        requestId: "request-1",
        method: "notebook.insert_cell",
        params: {
          kind: "code",
          source: "puts 'hello'",
          code_type: "ruby",
        },
      }),
    ).rejects.toMatchObject({
      code: "invalid_params",
      message: "notebook.insert_cell code_type is invalid",
    } satisfies Partial<AgentHandlerError>);
  });
});

function createNotebook(): Notebook {
  const state = {
    viewState: {
      isLoading: false,
      loadError: undefined,
    },
    serverState: {
      cells: {} as Record<
        string,
        {
          type: "code" | "markdown";
          source: string;
          version: number;
        }
      >,
      cellIds: [] as string[],
    },
    editBuffer: {
      cellSources: {},
    },
  };

  const notebook = {
    state,
    insertCellAfter: vi.fn(
      (
        _afterId: string | undefined,
        kind: "code" | "markdown",
        source: string,
        _lastEditedBy: string,
        _codeType: CodeType | undefined,
      ) => {
        const id = `cell-${state.serverState.cellIds.length + 1}`;
        state.serverState.cellIds.push(id);
        state.serverState.cells[id] = { type: kind, source, version: 1 };
        return id;
      },
    ),
  };

  return notebook as unknown as Notebook;
}

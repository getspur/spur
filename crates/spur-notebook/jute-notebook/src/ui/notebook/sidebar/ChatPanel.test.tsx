import "@testing-library/jest-dom/vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  within,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import type { ChatEvent } from "@/stores/chat";
import { useChat } from "@/stores/chat";

import ChatPanel from "./ChatPanel";
import { SIDEBAR_PANELS } from "./panels";

const tauriMocks = vi.hoisted(() => {
  const channels: Array<{ onmessage?: (message: ChatEvent) => void }> = [];
  class TestChannel<T> {
    onmessage?: (message: T) => void;

    constructor() {
      channels.push(this as { onmessage?: (message: ChatEvent) => void });
    }
  }

  return {
    channels,
    Channel: TestChannel,
    invoke: vi.fn(),
  };
});
let notebookPath = "/tmp/revenue.ipynb";
let appOpenInfo: { app_name?: string; app_root?: string } | undefined;

vi.mock("@tauri-apps/api/core", () => ({
  Channel: tauriMocks.Channel,
  invoke: tauriMocks.invoke,
}));

vi.mock("@/stores/notebook", () => ({
  useNotebook: () => ({
    store: {
      getInitialState: () => ({
        viewState: { appOpenInfo, path: notebookPath },
      }),
      getState: () => ({
        viewState: { appOpenInfo, path: notebookPath },
      }),
      subscribe: () => () => undefined,
    },
  }),
}));

describe("ChatPanel", () => {
  beforeEach(() => {
    useChat.getState().reset();
    tauriMocks.channels.length = 0;
    tauriMocks.invoke.mockReset();
    tauriMocks.invoke.mockImplementation((command: string) => {
      if (command === "chat_new_session") return Promise.resolve("session-1");
      return Promise.resolve(undefined);
    });
    notebookPath = "/tmp/revenue.ipynb";
    appOpenInfo = undefined;
  });

  afterEach(cleanup);

  test("renders scope, messages, streaming text, and permission prompt", async () => {
    const s = useChat.getState();
    s.setScope("notebook:/tmp/revenue.ipynb", "revenue.ipynb");
    s.applyEvent({
      type: "messageChunk",
      text: "Working",
    });
    s.applyEvent({
      type: "permissionRequest",
      id: "perm-1",
      title: "Run notebook edit?",
      options: [
        { id: "allow", label: "Allow" },
        { id: "deny", label: "Deny" },
      ],
    });

    render(<ChatPanel />);

    expect(screen.getByText("revenue.ipynb")).toBeInTheDocument();
    expect(screen.getByText("Working")).toBeInTheDocument();
    expect(screen.getByText("Run notebook edit?")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Deny" }));

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith(
        "chat_permission_respond",
        {
          requestId: "perm-1",
          optionId: null,
        },
      );
    });
    expect(useChat.getState().pendingPermission).toBeNull();
  });

  test("switches sessions for path and streams chat_turn events", async () => {
    render(<ChatPanel />);

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith("chat_switch_session", {
        notebookPath: "/tmp/revenue.ipynb",
        sessionId: "session-1",
      });
    });

    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), {
      target: { value: "Summarize the notebook" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith(
        "chat_turn",
        expect.objectContaining({
          notebookPath: "/tmp/revenue.ipynb",
          prompt: "Summarize the notebook",
          onEvent: tauriMocks.channels[0],
        }),
      );
    });

    tauriMocks.channels[0]?.onmessage?.({
      type: "messageChunk",
      text: "Summary",
    });
    expect(await screen.findByText("Summary")).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue("");
  });

  test("routes streamed turn events to the notebook scope that started the turn", async () => {
    notebookPath = "/tmp/revenue.ipynb";
    const firstPanel = render(<ChatPanel />);

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith("chat_switch_session", {
        notebookPath: "/tmp/revenue.ipynb",
        sessionId: "session-1",
      });
    });

    fireEvent.change(
      within(firstPanel.container).getByRole("textbox", { name: "Message" }),
      {
        target: { value: "Summarize revenue" },
      },
    );
    fireEvent.click(
      within(firstPanel.container).getByRole("button", {
        name: "Send message",
      }),
    );

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith(
        "chat_turn",
        expect.objectContaining({
          notebookPath: "/tmp/revenue.ipynb",
          prompt: "Summarize revenue",
          onEvent: tauriMocks.channels[0],
        }),
      );
    });

    notebookPath = "/tmp/costs.ipynb";
    const secondPanel = render(<ChatPanel />);

    await waitFor(() => {
      expect(
        within(secondPanel.container).getByText("costs.ipynb"),
      ).toBeInTheDocument();
    });

    await act(async () => {
      tauriMocks.channels[0]?.onmessage?.({
        type: "messageChunk",
        text: "Revenue summary",
      });
    });

    const state = useChat.getState();
    expect(
      state.conversations["notebook:/tmp/revenue.ipynb"]?.streamingText,
    ).toBe("Revenue summary");
    expect(state.conversations["notebook:/tmp/costs.ipynb"]?.streamingText).toBe(
      "",
    );
    expect(
      within(secondPanel.container).queryByText("Revenue summary"),
    ).not.toBeInTheDocument();
  });

  test("registers the agent sidebar panel", () => {
    const panel = SIDEBAR_PANELS.find((entry) => entry.id === "agent");

    expect(panel).toMatchObject({
      id: "agent",
      title: "AI Agent",
      ariaLabel: "AI Agent",
      Component: ChatPanel,
    });
  });
});

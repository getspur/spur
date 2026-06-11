import "@testing-library/jest-dom/vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
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

vi.mock("@tauri-apps/api/core", () => ({
  Channel: tauriMocks.Channel,
  invoke: tauriMocks.invoke,
}));

vi.mock("@/stores/notebook", () => ({
  useNotebook: () => ({
    store: {
      getInitialState: () => ({
        viewState: { appOpenInfo: undefined, path: notebookPath },
      }),
      getState: () => ({
        viewState: { appOpenInfo: undefined, path: notebookPath },
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
  });

  afterEach(cleanup);

  test("renders scope, messages, streaming text, and permission prompt", async () => {
    useChat.getState().applyEvent({
      type: "messageChunk",
      text: "Working",
    });
    useChat.getState().applyEvent({
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
          optionId: "deny",
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

  test("keeps late chat_turn events in the originating conversation", async () => {
    render(<ChatPanel />);

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
        }),
      );
    });

    useChat.getState().setScope("/apps/sales", "Sales App");

    tauriMocks.channels[0]?.onmessage?.({
      type: "messageChunk",
      text: "Notebook summary",
    });
    tauriMocks.channels[0]?.onmessage?.({ type: "done" });

    expect(useChat.getState().activeAppKey).toBe("/apps/sales");
    expect(useChat.getState().messages).toEqual([]);

    useChat.getState().setScope("notebook", "Notebook");
    expect(useChat.getState().messages.map((message) => message.text)).toEqual([
      "Notebook summary",
    ]);
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

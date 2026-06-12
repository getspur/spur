import "@testing-library/jest-dom/vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
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
      if (command === "chat_agents_list") {
        return Promise.resolve([
          { name: "claude-code", label: "Claude Code", selected: true },
        ]);
      }
      if (command === "chat_sessions_list") return Promise.resolve([]);
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
    expect(await screen.findByRole("combobox", { name: "Agent" })).toHaveValue(
      "claude-code",
    );

    fireEvent.click(screen.getByRole("button", { name: "Deny" }));

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith(
        "chat_permission_respond",
        {
          requestId: "perm-1",
          optionId: "deny",
          agentName: "claude-code",
        },
      );
    });
    expect(useChat.getState().pendingPermission).toBeNull();
  });

  test("ensures a session for path without loading it and streams chat_turn events", async () => {
    render(<ChatPanel />);

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith("chat_new_session", {
        agentName: "claude-code",
        notebookPath: "/tmp/revenue.ipynb",
      });
    });
    expect(
      tauriMocks.invoke.mock.calls.some(
        ([command]) => command === "chat_switch_session",
      ),
    ).toBe(false);

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
          agentName: "claude-code",
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

  test("submits a prompt when Enter is pressed in the message box", async () => {
    render(<ChatPanel />);

    await screen.findByRole("combobox", { name: "Agent" });
    const messageBox = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(messageBox, {
      target: { value: "hello" },
    });

    fireEvent.keyDown(messageBox, { key: "Enter" });

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith(
        "chat_turn",
        expect.objectContaining({
          notebookPath: "/tmp/revenue.ipynb",
          prompt: "hello",
          agentName: "claude-code",
        }),
      );
    });
  });

  test("keeps the send button clickable when stale streaming state exists", async () => {
    const s = useChat.getState();
    s.setScope("notebook:/tmp/revenue.ipynb", "revenue.ipynb");
    s.applyEvent({
      type: "messageChunk",
      text: "Unfinished previous response",
    });

    render(<ChatPanel />);

    await screen.findByRole("combobox", { name: "Agent" });
    const messageBox = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(messageBox, {
      target: { value: "hello" },
    });

    const sendButton = screen.getByRole("button", { name: "Send message" });
    expect((sendButton as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(sendButton);

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith(
        "chat_turn",
        expect.objectContaining({
          notebookPath: "/tmp/revenue.ipynb",
          prompt: "hello",
          agentName: "claude-code",
        }),
      );
    });
  });

  test("lists notebook sessions, selects the ensured session, and switches resumed sessions", async () => {
    const sessionLists = [
      [{ id: "session-old" }, { id: "session-older" }],
      [{ id: "session-1" }, { id: "session-old" }, { id: "session-older" }],
    ];
    tauriMocks.invoke.mockImplementation((command: string) => {
      if (command === "chat_sessions_list") {
        return Promise.resolve(sessionLists.shift() ?? sessionLists[0] ?? []);
      }
      if (command === "chat_agents_list") {
        return Promise.resolve([
          { name: "claude-code", label: "Claude Code", selected: true },
        ]);
      }
      if (command === "chat_new_session") return Promise.resolve("session-1");
      return Promise.resolve(undefined);
    });

    render(<ChatPanel />);

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith("chat_sessions_list", {
        agentName: "claude-code",
        notebookPath: "/tmp/revenue.ipynb",
      });
    });
    await waitFor(() => {
      expect(
        tauriMocks.invoke.mock.calls.filter(
          ([command]) => command === "chat_sessions_list",
        ),
      ).toHaveLength(2);
    });

    const picker = await screen.findByRole("combobox", {
      name: "Agent session",
    });
    await waitFor(() => {
      expect(picker).toHaveValue("session-1");
    });

    fireEvent.change(picker, { target: { value: "session-old" } });

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith("chat_switch_session", {
        agentName: "claude-code",
        notebookPath: "/tmp/revenue.ipynb",
        sessionId: "session-old",
      });
    });
  });

  test("renders session-list failures as scoped chat errors", async () => {
    tauriMocks.invoke.mockImplementation((command: string) => {
      if (command === "chat_sessions_list") {
        return Promise.reject(new Error("sessions unavailable"));
      }
      if (command === "chat_agents_list") {
        return Promise.resolve([
          { name: "claude-code", label: "Claude Code", selected: true },
        ]);
      }
      if (command === "chat_new_session") return Promise.resolve("session-1");
      return Promise.resolve(undefined);
    });

    render(<ChatPanel />);

    expect(await screen.findByText("sessions unavailable")).toBeInTheDocument();
    expect(
      useChat.getState().conversations["notebook:/tmp/revenue.ipynb"].messages,
    ).toEqual([
      expect.objectContaining({
        kind: "error",
        text: "sessions unavailable",
      }),
    ]);
  });

  test("routes streamed turn events to the notebook scope that started the turn", async () => {
    notebookPath = "/tmp/revenue.ipynb";
    const firstPanel = render(<ChatPanel />);

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith("chat_new_session", {
        agentName: "claude-code",
        notebookPath: "/tmp/revenue.ipynb",
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
          agentName: "claude-code",
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
    expect(
      state.conversations["notebook:/tmp/costs.ipynb"]?.streamingText,
    ).toBe("");
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

  test("selects a configured agent and submits turns through that agent", async () => {
    tauriMocks.invoke.mockImplementation((command: string, args?: unknown) => {
      const payload = args as { agentName?: string } | undefined;
      if (command === "chat_agents_list") {
        return Promise.resolve([
          { name: "claude-code", label: "Claude Code", selected: true },
          { name: "codex", label: "Codex", selected: false },
        ]);
      }
      if (command === "chat_sessions_list") return Promise.resolve([]);
      if (command === "chat_new_session") {
        return Promise.resolve(`${payload?.agentName ?? "agent"}-session`);
      }
      return Promise.resolve(undefined);
    });

    render(<ChatPanel />);

    const agentPicker = await screen.findByRole("combobox", { name: "Agent" });
    expect(agentPicker).toHaveValue("claude-code");

    fireEvent.change(agentPicker, { target: { value: "codex" } });

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith("chat_new_session", {
        agentName: "codex",
        notebookPath: "/tmp/revenue.ipynb",
      });
    });

    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), {
      target: { value: "Use Codex" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith(
        "chat_turn",
        expect.objectContaining({
          agentName: "codex",
          notebookPath: "/tmp/revenue.ipynb",
          prompt: "Use Codex",
        }),
      );
    });
  });
});

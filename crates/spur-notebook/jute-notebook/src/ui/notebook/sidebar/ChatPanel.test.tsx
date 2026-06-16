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
let selectedCellId: string | undefined;
let viewMode: "cells" | "dag" | "app" = "cells";

vi.mock("@tauri-apps/api/core", () => ({
  Channel: tauriMocks.Channel,
  invoke: tauriMocks.invoke,
}));

vi.mock("@/stores/notebook", () => ({
  useNotebook: () => ({
    store: {
      getInitialState: () => ({
        viewState: {
          appOpenInfo,
          path: notebookPath,
          selectedCellId,
          viewMode,
        },
      }),
      getState: () => ({
        viewState: {
          appOpenInfo,
          path: notebookPath,
          selectedCellId,
          viewMode,
        },
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
      if (command === "chat_session_modes_list") {
        return Promise.resolve({ modes: [], current: null });
      }
      return Promise.resolve(undefined);
    });
    notebookPath = "/tmp/revenue.ipynb";
    appOpenInfo = undefined;
    selectedCellId = undefined;
    viewMode = "cells";
  });

  afterEach(cleanup);

  test("renders the approved notebook empty state and ready scope status", () => {
    render(<ChatPanel />);

    expect(screen.getByText("Build on this notebook")).toBeInTheDocument();
    expect(
      screen.getByText(
        "Ask for the next cell, a cleaner analysis path, or stronger explanation.",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Ready with scoped tools enabled"),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Ready in revenue.ipynb - Builder lens"),
    ).toBeInTheDocument();
  });

  test("renders notebook lens controls and updates copy for deep dive", () => {
    render(<ChatPanel />);

    expect(screen.getByRole("button", { name: "Builder" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByRole("button", { name: "Deep dive" })).toHaveAttribute(
      "aria-pressed",
      "false",
    );

    fireEvent.click(screen.getByRole("button", { name: "Deep dive" }));

    expect(screen.getByText("Understand this notebook")).toBeInTheDocument();
    expect(
      screen.getByText(
        "Ask how the cells, outputs, and assumptions fit together.",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Ready in revenue.ipynb - Deep dive lens"),
    ).toBeInTheDocument();
  });

  test("changing notebook lens does not create a new chat session", async () => {
    render(<ChatPanel />);

    await waitFor(() => {
      expect(
        tauriMocks.invoke.mock.calls.filter(
          ([command]) => command === "chat_new_session",
        ),
      ).toHaveLength(1);
    });

    fireEvent.click(screen.getByRole("button", { name: "Deep dive" }));

    expect(
      tauriMocks.invoke.mock.calls.filter(
        ([command]) => command === "chat_new_session",
      ),
    ).toHaveLength(1);
  });

  test("renders the operations indicator in dag mode without notebook toggle", () => {
    viewMode = "dag";

    render(<ChatPanel />);

    expect(screen.getByText("Operations")).toBeInTheDocument();
    expect(screen.getByText("Operate this graph")).toBeInTheDocument();
    expect(
      screen.getByText(
        "Ask about failed nodes, stale dependencies, or recomputation order.",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Ready in revenue.ipynb - Operations lens"),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Builder" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Deep dive" })).toBeNull();
  });

  test("renders the product indicator in app mode", () => {
    viewMode = "app";
    appOpenInfo = {
      app_name: "Revenue App",
      app_root: "/tmp/revenue-app",
    };

    render(<ChatPanel />);

    expect(screen.getByText("Product")).toBeInTheDocument();
    expect(screen.getByText("Improve this app")).toBeInTheDocument();
    expect(
      screen.getByText(
        "Ask about workflow, UI quality, copy, or product behavior.",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Ready in Revenue App - Product lens"),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Builder" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Deep dive" })).toBeNull();
  });

  test("resets manual notebook lens override when view mode changes", () => {
    const { rerender } = render(<ChatPanel />);

    fireEvent.click(screen.getByRole("button", { name: "Deep dive" }));
    expect(screen.getByText("Understand this notebook")).toBeInTheDocument();

    viewMode = "dag";
    rerender(<ChatPanel />);
    expect(screen.getByText("Operate this graph")).toBeInTheDocument();

    viewMode = "cells";
    rerender(<ChatPanel />);
    expect(screen.getByText("Build on this notebook")).toBeInTheDocument();
  });

  test("renders tool calls and results as timeline events", () => {
    const s = useChat.getState();
    s.setScope("notebook:/tmp/revenue.ipynb", "revenue.ipynb");
    s.applyEvent({
      type: "toolCall",
      name: "code_symbol_search",
      argsSummary: "query: ChatPanel",
    });
    s.applyEvent({
      type: "toolResult",
      summary: "Found ChatPanel symbol.",
    });

    render(<ChatPanel />);

    expect(
      screen.getByText("Tool call: code_symbol_search"),
    ).toBeInTheDocument();
    expect(screen.getByText("Tool result")).toBeInTheDocument();
  });

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
    expect(screen.getByText("Waiting for permission")).toBeInTheDocument();
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

  test("renders streaming assistant markdown as formatted content", () => {
    const s = useChat.getState();
    s.setScope("notebook:/tmp/revenue.ipynb", "revenue.ipynb");
    s.applyEvent({
      type: "messageChunk",
      text: "### Streaming plan\n\n- Add markdown renderer",
    });

    render(<ChatPanel />);

    expect(
      screen.getByRole("heading", { level: 3, name: "Streaming plan" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Add markdown renderer").tagName).toBe("LI");
  });

  test("renders completed assistant markdown as formatted content", () => {
    const s = useChat.getState();
    s.setScope("notebook:/tmp/revenue.ipynb", "revenue.ipynb");
    s.applyEvent({
      type: "messageChunk",
      text: "### Completed plan\n\n- Keep markdown renderer",
    });
    s.applyEvent({ type: "done" });

    render(<ChatPanel />);

    expect(
      screen.getByRole("heading", { level: 3, name: "Completed plan" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Keep markdown renderer").tagName).toBe("LI");
  });

  test("ensures a session for path without loading it and streams chat_turn events", async () => {
    selectedCellId = "cell-42";

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
          context: expect.objectContaining({
            notebookPath: "/tmp/revenue.ipynb",
            viewMode: "notebook",
            lens: "notebook_builder",
            selectedCellRef: "cell://cell-42",
          }),
          onEvent: tauriMocks.channels[0],
        }),
      );
    });
    expect(screen.getByText("Summarize the notebook")).toBeInTheDocument();
    expect(
      useChat.getState().conversations["notebook:/tmp/revenue.ipynb"].messages,
    ).toEqual([
      expect.objectContaining({
        kind: "user",
        text: "Summarize the notebook",
      }),
    ]);

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

  test("lists advertised session modes and switches the active mode", async () => {
    tauriMocks.invoke.mockImplementation((command: string) => {
      if (command === "chat_agents_list") {
        return Promise.resolve([
          { name: "claude-code", label: "Claude Code", selected: true },
        ]);
      }
      if (command === "chat_sessions_list") return Promise.resolve([]);
      if (command === "chat_new_session") return Promise.resolve("session-1");
      if (command === "chat_session_modes_list") {
        return Promise.resolve({
          modes: ["default", "acceptEdits", "bypassPermissions"],
          current: "acceptEdits",
        });
      }
      return Promise.resolve(undefined);
    });

    render(<ChatPanel />);

    const modePicker = await screen.findByRole("combobox", {
      name: "Agent mode",
    });
    expect(modePicker).toHaveValue("acceptEdits");
    expect(tauriMocks.invoke).toHaveBeenCalledWith("chat_session_modes_list", {
      agentName: "claude-code",
      notebookPath: "/tmp/revenue.ipynb",
    });

    fireEvent.change(modePicker, { target: { value: "bypassPermissions" } });

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith(
        "chat_set_session_mode",
        {
          agentName: "claude-code",
          notebookPath: "/tmp/revenue.ipynb",
          modeId: "bypassPermissions",
        },
      );
    });
    expect(modePicker).toHaveValue("bypassPermissions");
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

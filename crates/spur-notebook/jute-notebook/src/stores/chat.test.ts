import { beforeEach, describe, expect, test } from "vitest";

import { DEFAULT_CHAT_APP_KEY, useChat } from "./chat";

describe("useChat", () => {
  beforeEach(() => {
    useChat.getState().reset();
  });

  test("message chunks accumulate then finalize on done", () => {
    const s = useChat.getState();

    s.applyEvent({ type: "messageChunk", text: "Hel" });
    s.applyEvent({ type: "messageChunk", text: "lo" });
    s.applyEvent({ type: "done" });

    const state = useChat.getState();
    expect(state.messages.at(-1)?.text).toBe("Hello");
    expect(state.streaming).toBe(false);
    expect(state.streamingText).toBe("");
  });

  test("appends user messages to the active conversation", () => {
    const s = useChat.getState();

    s.appendUserMessage("Summarize this notebook");

    expect(useChat.getState().messages).toEqual([
      expect.objectContaining({
        kind: "user",
        text: "Summarize this notebook",
      }),
    ]);
  });

  test("keeps conversations isolated by active app key", () => {
    const s = useChat.getState();

    s.applyEvent({ type: "messageChunk", text: "Notebook" });
    s.applyEvent({ type: "done" });
    s.setScope("/apps/sales", "Sales App");
    s.applyEvent({ type: "messageChunk", text: "Sales" });
    s.applyEvent({ type: "done" });

    expect(useChat.getState().activeAppKey).toBe("/apps/sales");
    expect(useChat.getState().scopeLabel).toBe("Sales App");
    expect(useChat.getState().messages.map((m) => m.text)).toEqual(["Sales"]);

    useChat.getState().setScope(DEFAULT_CHAT_APP_KEY, "Notebook");
    expect(useChat.getState().messages.map((m) => m.text)).toEqual([
      "Notebook",
    ]);
  });

  test("applies events to the supplied scope key instead of the active scope", () => {
    const s = useChat.getState();
    s.setScope("notebook:/tmp/a.ipynb", "a.ipynb");
    s.setScope("notebook:/tmp/b.ipynb", "b.ipynb");

    s.applyEventForScope("notebook:/tmp/a.ipynb", {
      type: "messageChunk",
      text: "A",
    });
    s.applyEventForScope("notebook:/tmp/a.ipynb", { type: "done" });

    expect(
      useChat
        .getState()
        .conversations["notebook:/tmp/a.ipynb"].messages.map((m) => m.text),
    ).toEqual(["A"]);
    expect(useChat.getState().messages.map((m) => m.text)).toEqual([]);
  });

  test("permission requests become pending permission views", () => {
    useChat.getState().applyEvent({
      type: "permissionRequest",
      id: "perm-1",
      title: "Edit notebook?",
      options: [
        { id: "allow", label: "Allow" },
        { id: "deny", label: "Deny" },
      ],
    });

    expect(useChat.getState().pendingPermission).toEqual({
      id: "perm-1",
      title: "Edit notebook?",
      options: [
        { id: "allow", label: "Allow" },
        { id: "deny", label: "Deny" },
      ],
    });
  });

  test("clearPendingPermission clears only the matching request id", () => {
    const s = useChat.getState();
    s.applyEvent({
      type: "permissionRequest",
      id: "perm-1",
      title: "Edit notebook?",
      options: [{ id: "allow", label: "Allow" }],
    });

    s.clearPendingPermission("other");
    expect(useChat.getState().pendingPermission?.id).toBe("perm-1");

    s.clearPendingPermission("perm-1");
    expect(useChat.getState().pendingPermission).toBeNull();
  });
});

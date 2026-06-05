import { afterEach, describe, expect, test, vi } from "vitest";

import { dispose, get, on, set } from "@/stores/widgetRegistry";

import { installAfmHostTransport } from "./afmHost";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

const touchedModelIds = new Set<string>();
const uninstallers: Array<() => void> = [];

function track(modelId: string): string {
  touchedModelIds.add(modelId);
  return modelId;
}

function installTransport() {
  const uninstall = installAfmHostTransport();
  uninstallers.push(uninstall);
  return uninstall;
}

function postAfmMessage(data: Record<string, unknown>) {
  window.dispatchEvent(new MessageEvent("message", { data }));
}

async function nextTick() {
  await new Promise((resolve) => window.setTimeout(resolve, 0));
}

describe("AFM host transport", () => {
  afterEach(() => {
    while (uninstallers.length > 0) {
      uninstallers.pop()?.();
    }
    for (const modelId of touchedModelIds) {
      dispose(modelId);
    }
    touchedModelIds.clear();
    invokeMock.mockReset();
  });

  test("routes widget send through anywidget_command and emits the response once", async () => {
    const modelId = track("afm-send-model");
    const content = {
      id: "cmd-1",
      kind: "anywidget-command",
      name: "source.push",
      msg: { port: "forecast", payload: [1, 2, 3] },
    };
    const response = {
      id: "cmd-1",
      kind: "anywidget-command-response",
      response: { accepted: true, port: "forecast" },
    };
    const customListener = vi.fn();
    on(modelId, "msg:custom", customListener);
    invokeMock.mockResolvedValueOnce(response);

    installTransport();
    postAfmMessage({
      source: "jute-afm",
      type: "send",
      modelId,
      content,
      buffers: [[7, 8, 9]],
    });
    await nextTick();

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("anywidget_command", {
      intent: { ...content, buffers: [[7, 8, 9]] },
    });
    expect(customListener).toHaveBeenCalledTimes(1);
    expect(customListener).toHaveBeenCalledWith(response, []);
  });

  test("deduplicates repeated AFM command messages by model and command id", async () => {
    const modelId = track("afm-dedup-model");
    const content = {
      id: "cmd-repeat",
      kind: "anywidget-command",
      name: "source.push",
      msg: { port: "forecast", payload: [1] },
    };
    const response = {
      id: "cmd-repeat",
      kind: "anywidget-command-response",
      response: { accepted: true },
    };
    const customListener = vi.fn();
    on(modelId, "msg:custom", customListener);
    invokeMock.mockResolvedValue(response);

    installTransport();
    installTransport();
    postAfmMessage({
      source: "jute-afm",
      type: "send",
      modelId,
      content,
      buffers: [],
    });
    postAfmMessage({
      source: "jute-afm",
      type: "send",
      modelId,
      content,
      buffers: [],
    });
    await nextTick();

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(customListener).toHaveBeenCalledTimes(1);
  });

  test("merges model-state.update responses into the widget registry", async () => {
    const modelId = track("afm-state-model");
    set(modelId, {
      state: { value: 1, preserved: "yes" },
      esm: "export default { render() {} }",
    });
    const content = {
      id: "cmd-state",
      kind: "anywidget-command",
      name: "model-state.update",
      msg: { state: { value: 2, added: "ok" } },
    };
    const response = {
      id: "cmd-state",
      kind: "anywidget-command-response",
      response: {
        method: "update",
        state: { value: 2, added: "ok" },
      },
    };
    const customListener = vi.fn();
    on(modelId, "msg:custom", customListener);
    invokeMock.mockResolvedValueOnce(response);

    installTransport();
    postAfmMessage({
      source: "jute-afm",
      type: "send",
      modelId,
      content,
      buffers: [],
    });
    await nextTick();

    expect(get(modelId)?.state).toEqual({
      value: 2,
      preserved: "yes",
      added: "ok",
    });
    expect(customListener).toHaveBeenCalledTimes(1);
    expect(customListener).toHaveBeenCalledWith(response, []);
  });

  test("routes save_changes messages as model-state.update commands", async () => {
    const modelId = track("afm-save-model");
    set(modelId, {
      state: { value: 1, preserved: "yes" },
      esm: "export default { render() {} }",
    });
    const customListener = vi.fn();
    on(modelId, "msg:custom", customListener);
    invokeMock.mockImplementationOnce(async (_command, args) => {
      const intent = (args as { intent: { id: string } }).intent;
      return {
        id: intent.id,
        kind: "anywidget-command-response",
        response: {
          method: "update",
          state: { value: 3 },
        },
      };
    });

    installTransport();
    postAfmMessage({
      source: "jute-afm",
      type: "save_changes",
      modelId,
      state: { value: 3 },
    });
    await nextTick();

    expect(invokeMock).toHaveBeenCalledWith("anywidget_command", {
      intent: expect.objectContaining({
        id: expect.stringContaining(`save_changes:${modelId}:`),
        kind: "anywidget-command",
        name: "model-state.update",
        msg: { state: { value: 3 } },
        buffers: [],
      }),
    });
    expect(get(modelId)?.state).toEqual({
      value: 3,
      preserved: "yes",
    });
    expect(customListener).toHaveBeenCalledTimes(1);
  });
});

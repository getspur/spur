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
      intent: { ...content, commId: modelId, buffers: [[7, 8, 9]] },
    });
    expect(customListener).toHaveBeenCalledTimes(1);
    expect(customListener).toHaveBeenCalledWith(response, []);
  });

  test("normalizes binary send buffers to byte arrays", async () => {
    const modelId = track("afm-binary-send-model");
    const content = {
      id: "cmd-binary",
      kind: "anywidget-command",
      name: "source.push",
      msg: { port: "binary", payload: [] },
    };
    const viewBacking = new ArrayBuffer(6);
    new Uint8Array(viewBacking).set([0, 9, 8, 7, 6, 0]);
    const dataView = new DataView(viewBacking, 1, 4);
    const arrayBuffer = new Uint8Array([1, 2, 255]).buffer;
    invokeMock.mockResolvedValueOnce({
      id: "cmd-binary",
      kind: "anywidget-command-response",
      response: { accepted: true },
    });

    installTransport();
    postAfmMessage({
      source: "jute-afm",
      type: "send",
      modelId,
      content,
      buffers: [dataView, arrayBuffer],
    });
    await nextTick();

    expect(invokeMock).toHaveBeenCalledWith("anywidget_command", {
      intent: {
        ...content,
        commId: modelId,
        buffers: [[9, 8, 7, 6], [1, 2, 255]],
      },
    });
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
        commId: modelId,
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

  test("normalizes save_changes buffers to byte arrays", async () => {
    const modelId = track("afm-save-binary-model");
    const typedArray = new Uint8Array([4, 5, 6]);
    const alreadyBytes = [10, 11, 12];
    invokeMock.mockImplementationOnce(async (_command, args) => {
      const intent = (args as { intent: { id: string } }).intent;
      return {
        id: intent.id,
        kind: "anywidget-command-response",
        response: {
          method: "update",
          state: { value: 4 },
        },
      };
    });

    installTransport();
    postAfmMessage({
      source: "jute-afm",
      type: "save_changes",
      modelId,
      state: { value: 4 },
      buffers: [typedArray, alreadyBytes],
    });
    await nextTick();

    expect(invokeMock).toHaveBeenCalledWith("anywidget_command", {
      intent: expect.objectContaining({
        name: "model-state.update",
        commId: modelId,
        buffers: [[4, 5, 6], [10, 11, 12]],
      }),
    });
  });

  test("routes generic widget send messages as custom comm commands", async () => {
    const modelId = track("afm-custom-send-model");
    const content = { arbitrary: true, count: 2 };
    const customListener = vi.fn();
    on(modelId, "msg:custom", customListener);
    invokeMock.mockImplementationOnce(async (_command, args) => {
      const intent = (args as { intent: { id: string } }).intent;
      return {
        id: intent.id,
        kind: "anywidget-command-response",
        response: {
          method: "custom",
          kernelDelivery: { status: "sent" },
        },
      };
    });

    installTransport();
    postAfmMessage({
      source: "jute-afm",
      type: "send",
      modelId,
      content,
      buffers: [new Uint8Array([9, 8, 7])],
    });
    await nextTick();

    expect(invokeMock).toHaveBeenCalledWith("anywidget_command", {
      intent: expect.objectContaining({
        id: expect.stringMatching(/^send:afm-custom-send-model:\d+$/),
        kind: "anywidget-command",
        name: "model-state.custom",
        commId: modelId,
        msg: { content },
        buffers: [[9, 8, 7]],
      }),
    });
    expect(customListener).toHaveBeenCalledTimes(1);
  });

  test("warns for malformed custom send messages without content", async () => {
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const modelId = track("afm-malformed-send-model");

    installTransport();
    postAfmMessage({
      source: "jute-afm",
      type: "send",
      modelId,
    });
    await nextTick();

    expect(invokeMock).not.toHaveBeenCalled();
    expect(warnSpy).toHaveBeenCalledWith(
      "[jute-afm] ignoring malformed custom send message",
      {
        type: "send",
        modelId,
      },
    );
    warnSpy.mockRestore();
  });
});

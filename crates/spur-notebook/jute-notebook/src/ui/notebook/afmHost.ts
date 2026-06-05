import { invoke } from "@tauri-apps/api/core";

import {
  emit as emitWidgetModel,
  get as getWidgetModel,
  set as setWidgetModel,
} from "@/stores/widgetRegistry";

const AFM_SOURCE = "jute-afm";
const ANYWIDGET_COMMAND_KIND = "anywidget-command";
const MODEL_STATE_UPDATE = "model-state.update";
const MAX_DEDUP_KEYS = 512;

type AfmWindowMessage = {
  source: typeof AFM_SOURCE;
  type: "send" | "save_changes";
  modelId: string;
  content?: unknown;
  buffers?: unknown;
  state?: unknown;
};

type AnywidgetCommandContent = Record<string, unknown> & {
  id: string;
  kind: string;
  name: string;
  msg: unknown;
};

type AnywidgetCommandIntent = AnywidgetCommandContent & {
  buffers: number[][];
};

type AnywidgetCommandResponse = {
  id: string;
  kind: string;
  response: unknown;
  buffers?: unknown[];
  responseBuffers?: unknown[];
};

let installCount = 0;
let removeWindowListener: (() => void) | null = null;
const deliveredKeys = new Set<string>();
const deliveredKeyOrder: string[] = [];

export function installAfmHostTransport(): () => void {
  installCount += 1;

  if (!removeWindowListener) {
    const listener = (event: MessageEvent) => {
      void handleWindowMessage(event);
    };
    window.addEventListener("message", listener);
    removeWindowListener = () =>
      window.removeEventListener("message", listener);
  }

  let disposed = false;
  return () => {
    if (disposed) return;
    disposed = true;
    installCount = Math.max(0, installCount - 1);
    if (installCount === 0) {
      removeWindowListener?.();
      removeWindowListener = null;
      deliveredKeys.clear();
      deliveredKeyOrder.length = 0;
    }
  };
}

async function handleWindowMessage(event: MessageEvent) {
  const message = afmMessageFromEventData(event.data);
  if (!message) return;

  const intent = intentFromMessage(message);
  if (!intent) return;

  const dedupKey = `${message.modelId}:${message.type}:${intent.id}`;
  if (!rememberDelivery(dedupKey)) return;

  try {
    const response = await invoke<AnywidgetCommandResponse>(
      "anywidget_command",
      {
        intent,
      },
    );
    applyModelStateResponse(message.modelId, intent, response);
    emitWidgetModel(
      message.modelId,
      "msg:custom",
      response,
      responseBuffers(response),
    );
  } catch (error) {
    forgetDelivery(dedupKey);
    console.warn("[jute-afm] anywidget command failed", error);
  }
}

function afmMessageFromEventData(data: unknown): AfmWindowMessage | null {
  if (
    !isRecord(data) ||
    data.source !== AFM_SOURCE ||
    (data.type !== "send" && data.type !== "save_changes") ||
    typeof data.modelId !== "string"
  ) {
    return null;
  }

  return data as AfmWindowMessage;
}

function intentFromMessage(
  message: AfmWindowMessage,
): AnywidgetCommandIntent | null {
  const buffers = buffersFromMessage(message.buffers);
  const content = commandContent(message.content);
  if (content) {
    return { ...content, buffers };
  }

  if (message.type === "send") {
    console.warn("[jute-afm] dropping unsupported custom message", {
      type: message.type,
      modelId: message.modelId,
    });
    return null;
  }

  if (!isRecord(message.state)) {
    return null;
  }

  return {
    id: `save_changes:${message.modelId}:${stableJson(message.state)}`,
    kind: ANYWIDGET_COMMAND_KIND,
    name: MODEL_STATE_UPDATE,
    msg: { state: { ...message.state } },
    buffers,
  };
}

function commandContent(content: unknown): AnywidgetCommandContent | null {
  if (
    !isRecord(content) ||
    typeof content.id !== "string" ||
    typeof content.kind !== "string" ||
    typeof content.name !== "string" ||
    !Object.hasOwn(content, "msg")
  ) {
    return null;
  }

  return content as AnywidgetCommandContent;
}

function buffersFromMessage(buffers: unknown): number[][] {
  if (!Array.isArray(buffers)) {
    return [];
  }
  return buffers.map(normalizeBuffer);
}

function normalizeBuffer(buffer: unknown): number[] {
  if (isNumberArray(buffer)) {
    return buffer;
  }

  if (buffer instanceof ArrayBuffer) {
    return Array.from(new Uint8Array(buffer));
  }

  if (ArrayBuffer.isView(buffer)) {
    return Array.from(
      new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength),
    );
  }

  return [];
}

function applyModelStateResponse(
  modelId: string,
  intent: AnywidgetCommandIntent,
  response: AnywidgetCommandResponse,
) {
  if (intent.name !== MODEL_STATE_UPDATE || !isRecord(response.response)) {
    return;
  }

  const payload = response.response;
  if (payload.method !== "update" || !isRecord(payload.state)) {
    return;
  }

  setWidgetModel(modelId, {
    state: {
      ...(getWidgetModel(modelId)?.state ?? {}),
      ...payload.state,
    },
  });
}

function responseBuffers(response: AnywidgetCommandResponse): unknown[] {
  if (Array.isArray(response.responseBuffers)) {
    return response.responseBuffers;
  }
  if (Array.isArray(response.buffers)) {
    return response.buffers;
  }
  return [];
}

function rememberDelivery(key: string): boolean {
  if (deliveredKeys.has(key)) {
    return false;
  }

  deliveredKeys.add(key);
  deliveredKeyOrder.push(key);
  while (deliveredKeyOrder.length > MAX_DEDUP_KEYS) {
    const oldest = deliveredKeyOrder.shift();
    if (oldest) {
      deliveredKeys.delete(oldest);
    }
  }
  return true;
}

function forgetDelivery(key: string) {
  if (!deliveredKeys.delete(key)) return;

  const index = deliveredKeyOrder.indexOf(key);
  if (index !== -1) {
    deliveredKeyOrder.splice(index, 1);
  }
}

function stableJson(value: unknown): string {
  try {
    return JSON.stringify(value) ?? "";
  } catch {
    return String(value);
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isNumberArray(value: unknown): value is number[] {
  return Array.isArray(value) && value.every((item) => typeof item === "number");
}

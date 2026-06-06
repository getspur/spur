import clsx from "clsx";
import {
  useCallback,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
} from "react";

import {
  get as getWidgetModel,
  on as onWidgetModel,
  set as setWidgetModel,
} from "@/stores/widgetRegistry";

import { htmlOutputSandbox } from "./rendering";

export const WIDGET_VIEW_MIME = "application/vnd.jupyter.widget-view+json";
export const JUTE_PORT_BINDINGS_STATE_KEY = "__jute_port_bindings";

export type AfmBoundPortState = {
  currentVersion?: number;
  executionCount?: number;
  ranVersion?: number;
  state?: string;
};

export type AfmPortBindingSnapshot = {
  cellId: string;
  binds: string[];
  ports: Record<string, AfmBoundPortState>;
};

type AnywidgetView = {
  modelId: string;
  versionMajor?: number;
  versionMinor?: number;
};

type Props = {
  modelId: string;
  widgetView?: AnywidgetView;
  chromeless?: boolean;
  portBindings?: AfmPortBindingSnapshot;
};

type AfmDocument = {
  modelId: string;
  state: Record<string, unknown>;
  esm: string;
  css?: string;
};

const AFM_MIN_HEIGHT = 160;
const AFM_HEIGHT_MESSAGE = "jute-afm-height";

export function anywidgetViewFromData(value: unknown): AnywidgetView | null {
  if (!isRecord(value) || typeof value.model_id !== "string") {
    return null;
  }

  return {
    modelId: value.model_id,
    versionMajor: numberValue(value.version_major),
    versionMinor: numberValue(value.version_minor),
  };
}

export default function AfmView({
  modelId,
  widgetView,
  chromeless = false,
  portBindings,
}: Props) {
  const revision = useWidgetModelRevision(modelId);
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(AFM_MIN_HEIGHT);
  const model = getWidgetModel(modelId);
  const modelCss = model?.css;
  const modelEsm = model?.esm;
  const portBindingsKey = useMemo(
    () => stableJson(portBindings),
    [portBindings],
  );

  const srcDoc = useMemo(() => {
    if (!model || typeof modelEsm !== "string" || modelEsm.trim() === "") {
      return null;
    }

    return afmSrcDoc({
      modelId,
      state: model.state,
      esm: modelEsm,
      css: modelCss,
    });
  }, [model, modelCss, modelEsm, modelId]);

  const postModelUpdate = useCallback(() => {
    const frame = iframeRef.current?.contentWindow;
    const model = getWidgetModel(modelId);
    if (!frame || !model) return;

    frame.postMessage(
      {
        source: "jute-afm-host",
        type: "jute-afm-model-update",
        modelId,
        state: model.state,
        css: model.css,
      },
      "*",
    );
  }, [modelId]);

  useEffect(() => {
    postModelUpdate();
  }, [postModelUpdate, revision]);

  useEffect(() => {
    setHeight(AFM_MIN_HEIGHT);
  }, [srcDoc]);

  useEffect(() => {
    const handleMessage = (event: MessageEvent) => {
      if (event.source !== iframeRef.current?.contentWindow) {
        return;
      }
      if (
        typeof event.data !== "object" ||
        event.data === null ||
        event.data.type !== AFM_HEIGHT_MESSAGE ||
        typeof event.data.height !== "number" ||
        !Number.isFinite(event.data.height)
      ) {
        return;
      }
      setHeight(Math.max(AFM_MIN_HEIGHT, Math.ceil(event.data.height)));
    };

    window.addEventListener("message", handleMessage);
    return () => window.removeEventListener("message", handleMessage);
  }, []);

  useEffect(() => {
    if (!portBindings || portBindings.binds.length === 0) return;
    const model = getWidgetModel(modelId);
    if (!model) return;

    setWidgetModel(modelId, {
      state: {
        ...model.state,
        [JUTE_PORT_BINDINGS_STATE_KEY]: portBindings,
      },
    });
  }, [modelId, portBindings, portBindingsKey]);

  useEffect(() => {
    return onWidgetModel(modelId, "msg:custom", (content, buffers) => {
      const frame = iframeRef.current?.contentWindow;
      if (!frame) return;

      frame.postMessage(
        {
          source: "jute-afm-host",
          type: "jute-afm-custom-message",
          modelId,
          content,
          buffers,
        },
        "*",
      );
    });
  }, [modelId]);

  if (!srcDoc) {
    return <AnywidgetPlaceholder widgetView={widgetView ?? { modelId }} />;
  }

  return (
    <iframe
      ref={iframeRef}
      title={`anywidget ${modelId}`}
      srcDoc={srcDoc}
      sandbox={htmlOutputSandbox(true)}
      onLoad={postModelUpdate}
      style={{ height, minHeight: AFM_MIN_HEIGHT }}
      className={clsx(
        "block w-full bg-white",
        chromeless ? "border-0" : "rounded border border-slate-200",
      )}
    />
  );
}

function useWidgetModelRevision(modelId: string) {
  const [revision, incrementRevision] = useReducer((value: number) => {
    return value + 1;
  }, 0);

  useEffect(() => {
    return onWidgetModel(modelId, "change", incrementRevision);
  }, [modelId]);

  return revision;
}

function AnywidgetPlaceholder({ widgetView }: { widgetView: AnywidgetView }) {
  const version =
    widgetView.versionMajor === undefined
      ? null
      : `${widgetView.versionMajor}.${widgetView.versionMinor ?? 0}`;

  return (
    <section className="rounded border border-slate-200 bg-slate-50 px-3 py-2 text-xs text-slate-700">
      <div className="flex items-center gap-2">
        <span className="font-medium text-slate-900">anywidget model</span>
        {version ? (
          <span className="font-mono text-slate-500">v{version}</span>
        ) : null}
      </div>
      <div className="mt-1 flex flex-wrap items-center gap-2">
        <code className="rounded border border-slate-200 bg-white px-1.5 py-0.5">
          {widgetView.modelId}
        </code>
        <span>waiting for widget model assets</span>
      </div>
    </section>
  );
}

function afmSrcDoc(document: AfmDocument) {
  return `<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <style>
    html,body,#jute-afm-root{min-height:100%;margin:0}
    body{font:13px system-ui,sans-serif;color:#172033;background:#fff}
  </style>
</head>
<body>
<main id="jute-afm-root"></main>
<script type="module">
const modelId = ${jsonForScript(document.modelId)};
let state = cloneRecord(${jsonForScript(document.state)});
let cssText = ${jsonForScript(document.css ?? "")};
const source = ${jsonForScript(document.esm)};
const root = document.getElementById("jute-afm-root");
const listeners = new Map();

function postHeight() {
  window.parent.postMessage({
    type: "${AFM_HEIGHT_MESSAGE}",
    height: Math.max(
      document.body.scrollHeight,
      document.documentElement.scrollHeight,
      root?.scrollHeight ?? 0,
    ),
  }, "*");
}

window.addEventListener("load", postHeight);
if ("ResizeObserver" in window) {
  const heightObserver = new ResizeObserver(postHeight);
  heightObserver.observe(document.body);
  if (root) heightObserver.observe(root);
}
requestAnimationFrame(postHeight);
postHeight();

function cloneRecord(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? { ...value } : {};
}

function callbacksFor(name) {
  let callbacks = listeners.get(name);
  if (!callbacks) {
    callbacks = new Set();
    listeners.set(name, callbacks);
  }
  return callbacks;
}

function emit(name, ...args) {
  for (const callback of Array.from(listeners.get(name) ?? [])) {
    callback(...args);
  }
}

function updateState(nextState) {
  const previous = state;
  state = cloneRecord(nextState);
  const keys = new Set([...Object.keys(previous), ...Object.keys(state)]);
  for (const key of keys) {
    if (!Object.is(previous[key], state[key])) {
      emit("change:" + key);
    }
  }
  emit("change");
}

function updateCss(nextCssText) {
  cssText = typeof nextCssText === "string" ? nextCssText : "";
  let style = document.getElementById("jute-afm-css");
  if (!style) {
    style = document.createElement("style");
    style.id = "jute-afm-css";
    document.head.appendChild(style);
  }
  style.textContent = cssText;
}

function randomId() {
  if (globalThis.crypto?.randomUUID) {
    return globalThis.crypto.randomUUID();
  }
  return Math.random().toString(36).slice(2) + Date.now().toString(36);
}

function timeoutSignal(ms) {
  const controller = new AbortController();
  window.setTimeout(() => controller.abort(new Error("anywidget invoke timed out")), ms);
  return controller.signal;
}

const model = {
  get(key) {
    return state[key];
  },
  set(key, value) {
    const previous = state[key];
    state[key] = value;
    if (!Object.is(previous, value)) {
      emit("change:" + key);
    }
    emit("change");
  },
  on(name, callback) {
    callbacksFor(name).add(callback);
  },
  off(name, callback) {
    if (name == null) {
      listeners.clear();
      return;
    }
    if (callback == null) {
      listeners.delete(name);
      return;
    }
    const callbacks = listeners.get(name);
    callbacks?.delete(callback);
    if (callbacks?.size === 0) {
      listeners.delete(name);
    }
  },
  save_changes() {
    window.parent.postMessage({ source: "jute-afm", type: "save_changes", modelId, state }, "*");
  },
  send(content, callbacks, buffers) {
    window.parent.postMessage({ source: "jute-afm", type: "send", modelId, content, buffers }, "*");
  },
  widget_manager: {
    async get_model(ref) {
      throw new Error("Jute AFM widget_manager.get_model is not implemented for " + ref);
    },
  },
};

function invoke(name, msg, options = {}) {
  const id = randomId();
  const signal = options.signal ?? timeoutSignal(3000);
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(signal.reason);
      return;
    }
    function cleanup() {
      model.off("msg:custom", handleResponse);
      signal.removeEventListener("abort", handleAbort);
    }
    function handleAbort() {
      cleanup();
      reject(signal.reason);
    }
    function handleResponse(message, buffers = []) {
      if (
        !message ||
        message.id !== id ||
        message.kind !== "anywidget-command-response"
      ) {
        return;
      }
      cleanup();
      resolve([message.response, buffers]);
    }
    signal.addEventListener("abort", handleAbort, { once: true });
    model.on("msg:custom", handleResponse);
    model.send(
      { id, kind: "anywidget-command", name, msg },
      undefined,
      options.buffers ?? [],
    );
  });
}

const experimental = { invoke };
const host = {
  async getWidget(ref) {
    throw new Error("Jute AFM host.getWidget is not implemented for " + ref);
  },
  async getModel(ref) {
    throw new Error("Jute AFM host.getModel is not implemented for " + ref);
  },
};

async function loadWidget(esm) {
  let mod;
  if (esm.startsWith("http://") || esm.startsWith("https://")) {
    mod = await import(esm);
  } else {
    const blobUrl = URL.createObjectURL(new Blob([esm], { type: "text/javascript" }));
    try {
      mod = await import(blobUrl);
    } finally {
      URL.revokeObjectURL(blobUrl);
    }
  }
  if (mod.render) {
    return { initialize: async () => {}, render: mod.render };
  }
  if (!mod.default) {
    throw new Error("[anywidget] module must export a default function or object.");
  }
  return typeof mod.default === "function" ? await mod.default() : mod.default;
}

async function safeCleanup(cleanup, reason) {
  if (typeof cleanup !== "function") return;
  try {
    await cleanup();
  } catch (error) {
    console.warn("[jute-afm] cleanup failed during " + reason, error);
  }
}

function showError(error) {
  root.innerHTML = "";
  const pre = document.createElement("pre");
  pre.style.whiteSpace = "pre-wrap";
  pre.style.color = "#b91c1c";
  pre.style.padding = "12px";
  pre.textContent = error instanceof Error ? error.stack ?? error.message : String(error);
  root.appendChild(pre);
}

window.addEventListener("message", (event) => {
  const message = event.data;
  if (!message || message.source !== "jute-afm-host" || message.modelId !== modelId) {
    return;
  }
  if (message.type === "jute-afm-model-update") {
    updateState(message.state);
    updateCss(message.css);
  } else if (message.type === "jute-afm-custom-message") {
    emit("msg:custom", message.content, message.buffers ?? []);
  }
});

updateCss(cssText);

const initializeController = new AbortController();
const viewController = new AbortController();
let initializeCleanup;
let renderCleanup;
let disposed = false;

async function disposeRuntime(reason) {
  if (disposed) return;
  disposed = true;
  viewController.abort();
  initializeController.abort();
  await safeCleanup(renderCleanup, reason + " render");
  await safeCleanup(initializeCleanup, reason + " initialize");
}

window.addEventListener("pagehide", () => void disposeRuntime("pagehide"), { once: true });
window.addEventListener("beforeunload", () => void disposeRuntime("beforeunload"), { once: true });

try {
  const widget = await loadWidget(source);
  const initializeResult = await widget.initialize?.({ model, signal: initializeController.signal, experimental });
  if (typeof initializeResult === "function") {
    initializeCleanup = initializeResult;
  }
  if (!initializeController.signal.aborted && widget.render) {
    renderCleanup = await widget.render?.({ model, el: root, signal: viewController.signal, host, experimental });
  }
} catch (error) {
  showError(error);
}
</script>
</body>
</html>`;
}

function jsonForScript(value: unknown) {
  return JSON.stringify(value)
    .replace(/</g, "\\u003c")
    .replace(/>/g, "\\u003e")
    .replace(/&/g, "\\u0026")
    .replace(/\u2028/g, "\\u2028")
    .replace(/\u2029/g, "\\u2029");
}

function numberValue(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value)
    ? value
    : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stableJson(value: unknown): string {
  try {
    return JSON.stringify(value) ?? "";
  } catch {
    return String(value);
  }
}

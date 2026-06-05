import { useMemo } from "react";

import { htmlOutputSandbox } from "./rendering";

export const WIDGET_VIEW_MIME = "application/vnd.jupyter.widget-view+json";
export const JUTE_APP_MIME = "application/vnd.jute.app+json";

type AnywidgetView = {
  modelId: string;
  versionMajor?: number;
  versionMinor?: number;
};

type JuteApp = {
  appId: string;
  title: string;
  esm: string;
  state: Record<string, unknown>;
  height: number;
};

type Props = {
  widgetView: AnywidgetView;
  appPayload?: unknown;
};

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

export default function JuteAppOutput({ widgetView, appPayload }: Props) {
  const app = normalizeJuteApp(appPayload, widgetView);
  if (!app) {
    return <AnywidgetPlaceholder widgetView={widgetView} />;
  }

  return <JuteAppIframe app={app} widgetView={widgetView} />;
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
        <span>waiting for Jute app payload</span>
      </div>
    </section>
  );
}

function JuteAppIframe({
  app,
  widgetView,
}: {
  app: JuteApp;
  widgetView: AnywidgetView;
}) {
  const srcDoc = useMemo(
    () => juteAppSrcDoc({ app, widgetView }),
    [app, widgetView],
  );

  return (
    <iframe
      title={app.title}
      srcDoc={srcDoc}
      sandbox={htmlOutputSandbox(true)}
      style={{ height: app.height }}
      className="block w-full rounded border border-slate-200 bg-white"
    />
  );
}

function juteAppSrcDoc({
  app,
  widgetView,
}: {
  app: JuteApp;
  widgetView: AnywidgetView;
}) {
  return `<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <style>
    html,body,#jute-app-root{min-height:100%;margin:0}
    body{font:13px system-ui,sans-serif;color:#172033;background:#fff}
  </style>
</head>
<body>
<main id="jute-app-root"></main>
<script type="module">
const appId = ${JSON.stringify(app.appId)};
const modelId = ${JSON.stringify(widgetView.modelId)};
const state = ${JSON.stringify(app.state)};
const listeners = new Map();
function emit(name) {
  for (const callback of listeners.get(name) ?? []) callback();
}
const model = {
  model_id: modelId,
  get(key) { return state[key]; },
  set(key, value) {
    state[key] = value;
    emit("change:" + key);
    emit("change");
  },
  on(name, callback) {
    const callbacks = listeners.get(name) ?? [];
    callbacks.push(callback);
    listeners.set(name, callbacks);
  },
  off(name, callback) {
    if (!name) return listeners.clear();
    if (!callback) return listeners.delete(name);
    listeners.set(name, (listeners.get(name) ?? []).filter((item) => item !== callback));
  },
  save_changes() {
    window.parent.postMessage({ source: "jute-app", kind: "save_changes", appId, modelId, state }, "*");
  },
  send(content, callbacks, buffers) {
    window.parent.postMessage({ source: "jute-app", kind: "custom", appId, modelId, content, buffers }, "*");
  }
};
const host = {
  async getWidget(ref) { throw new Error("Jute app host has no widget composition for " + ref); },
  async getModel(ref) { throw new Error("Jute app host has no model composition for " + ref); }
};
const source = ${JSON.stringify(app.esm)};
const blobUrl = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
try {
  const mod = await import(blobUrl);
  const api = mod.render
    ? mod
    : typeof mod.default === "function"
      ? { render: mod.default }
      : mod.default ?? mod;
  const initResult = await api?.initialize?.({ model, signal: undefined, host });
  const exports = initResult && typeof initResult === "object" ? initResult : undefined;
  await api?.render?.({ model, el: document.getElementById("jute-app-root"), signal: undefined, host, exports });
} catch (error) {
  document.body.innerHTML = "<pre style='white-space:pre-wrap;color:#b91c1c;padding:12px'></pre>";
  document.querySelector("pre").textContent = error instanceof Error ? error.stack ?? error.message : String(error);
} finally {
  URL.revokeObjectURL(blobUrl);
}
</script>
</body>
</html>`;
}

function normalizeJuteApp(
  value: unknown,
  widgetView: AnywidgetView,
): JuteApp | null {
  if (
    !isRecord(value) ||
    typeof value.esm !== "string" ||
    value.esm.trim() === ""
  ) {
    return null;
  }

  const appId =
    typeof value.appId === "string" && value.appId.trim() !== ""
      ? value.appId
      : widgetView.modelId;
  const title =
    typeof value.title === "string" && value.title.trim() !== ""
      ? value.title
      : appId;

  return {
    appId,
    title,
    esm: value.esm,
    state: isRecord(value.state) ? value.state : {},
    height: normalizeHeight(value.height),
  };
}

function normalizeHeight(value: unknown) {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return 360;
  }
  return Math.max(120, Math.min(1200, Math.round(value)));
}

function numberValue(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value)
    ? value
    : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

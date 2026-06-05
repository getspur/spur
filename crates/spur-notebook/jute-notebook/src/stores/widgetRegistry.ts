export type WidgetModelListener = (...args: unknown[]) => void;

export type WidgetModelRecord = {
  state: Record<string, unknown>;
  esm?: string;
  css?: string;
  listeners: Map<string, Set<WidgetModelListener>>;
};

export type WidgetModelUpdate = {
  state?: Record<string, unknown>;
  esm?: string;
  css?: string;
};

const models = new Map<string, WidgetModelRecord>();

function createModel(): WidgetModelRecord {
  return {
    state: {},
    listeners: new Map(),
  };
}

function ensureModel(modelId: string): WidgetModelRecord {
  let model = models.get(modelId);
  if (!model) {
    model = createModel();
    models.set(modelId, model);
  }
  return model;
}

export function get(modelId: string): WidgetModelRecord | undefined {
  return models.get(modelId);
}

export function set(
  modelId: string,
  update: WidgetModelUpdate,
): WidgetModelRecord {
  const model = ensureModel(modelId);
  if (update.state !== undefined) {
    model.state = update.state;
  }
  if (update.esm !== undefined) {
    model.esm = update.esm;
  }
  if (update.css !== undefined) {
    model.css = update.css;
  }
  emit(modelId, "change", model);
  return model;
}

export function on(
  modelId: string,
  event: string,
  listener: WidgetModelListener,
): () => void {
  const model = ensureModel(modelId);
  let listeners = model.listeners.get(event);
  if (!listeners) {
    listeners = new Set();
    model.listeners.set(event, listeners);
  }
  listeners.add(listener);
  return () => off(modelId, event, listener);
}

export function off(
  modelId: string,
  event: string,
  listener: WidgetModelListener,
) {
  const model = models.get(modelId);
  const listeners = model?.listeners.get(event);
  listeners?.delete(listener);
  if (listeners?.size === 0) {
    model?.listeners.delete(event);
  }
}

export function emit(modelId: string, event: string, ...args: unknown[]) {
  const model = models.get(modelId);
  const listeners = model?.listeners.get(event);
  if (!listeners) return;

  for (const listener of Array.from(listeners)) {
    listener(...args);
  }
}

export function dispose(modelId: string) {
  const model = models.get(modelId);
  model?.listeners.clear();
  models.delete(modelId);
}

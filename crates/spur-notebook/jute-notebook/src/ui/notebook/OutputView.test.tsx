import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { dispose, on, set } from "@/stores/widgetRegistry";

import OutputView from "./OutputView";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

const WIDGET_VIEW_MIME = "application/vnd.jupyter.widget-view+json";
const AFM_MODEL_ID = "jute-afm-model";

const DENO_STYLE_ESM = `
const format = (value) => value.toUpperCase();
export default {
  initialize({ model, signal, experimental }) {
    model.on("msg:custom", () => {});
    return { ready: !signal.aborted, invoke: experimental.invoke };
  },
  render({ model, el, signal, host, experimental }) {
    el.className = "deno-widget";
    el.textContent = format(model.get("letters"));
    model.on("change:letters", () => {
      el.textContent = format(model.get("letters"));
    });
    return () => {
      el.dataset.cleaned = String(signal.aborted && Boolean(host) && Boolean(experimental));
    };
  },
};
`;

function outputValue(modelId = AFM_MODEL_ID) {
  return {
    status: "success",
    outputs: [
      {
        output_type: "display_data",
        data: {
          [WIDGET_VIEW_MIME]: {
            version_major: 2,
            version_minor: 1,
            model_id: modelId,
          },
          "text/plain": "anywidget view",
        },
        metadata: {},
      },
    ],
  } as any;
}

describe("OutputView AFM widget rendering", () => {
  afterEach(() => {
    cleanup();
    dispose(AFM_MODEL_ID);
    dispose("model-only");
    invokeMock.mockReset();
  });

  test("renders a standard anywidget placeholder when registry assets are not available", () => {
    render(<OutputView value={outputValue("model-only")} />);

    expect(screen.getByText("anywidget model")).toBeInTheDocument();
    expect(screen.getByText("model-only")).toBeInTheDocument();
    expect(
      screen.getByText("waiting for widget model assets"),
    ).toBeInTheDocument();
  });

  test("renders an AFM iframe from the widget registry model", () => {
    set(AFM_MODEL_ID, {
      state: { letters: "abcd" },
      esm: DENO_STYLE_ESM,
      css: ".deno-widget { color: green; }",
    });

    render(<OutputView value={outputValue()} />);

    const iframe = screen.getByTitle(`anywidget ${AFM_MODEL_ID}`);
    expect(iframe).toHaveAttribute(
      "sandbox",
      expect.stringContaining("allow-scripts"),
    );
    expect(iframe.getAttribute("sandbox")).not.toContain("allow-same-origin");
    expect(iframe).toHaveAttribute(
      "srcdoc",
      expect.stringContaining('"letters":"abcd"'),
    );
    expect(iframe).toHaveAttribute(
      "srcdoc",
      expect.stringContaining(".deno-widget { color: green; }"),
    );
  });

  test("AFM iframe runtime follows the anywidget ABI", () => {
    set(AFM_MODEL_ID, {
      state: { letters: "abcd" },
      esm: DENO_STYLE_ESM,
      css: ".deno-widget { color: green; }",
    });

    render(<OutputView value={outputValue()} />);

    const iframe = screen.getByTitle(`anywidget ${AFM_MODEL_ID}`);
    const srcDoc = iframe.getAttribute("srcdoc") ?? "";

    expect(srcDoc).toContain(
      "initialize?.({ model, signal: initializeController.signal, experimental })",
    );
    expect(srcDoc).toContain(
      "render?.({ model, el: root, signal: viewController.signal, host, experimental })",
    );
    expect(srcDoc).toContain("widget_manager");
    expect(srcDoc).toContain("anywidget-command");
    expect(srcDoc).toContain("anywidget-command-response");
    expect(srcDoc).toContain("renderCleanup");
    expect(srcDoc).toContain("jute-afm-model-update");
    expect(srcDoc).toContain("jute-afm-custom-message");
    expect(srcDoc).not.toContain("signal: undefined");
    expect(srcDoc).not.toContain("exports");
  });

  test("routes AFM iframe command responses back through widget custom messages", async () => {
    set(AFM_MODEL_ID, {
      state: { letters: "abcd" },
      esm: DENO_STYLE_ESM,
    });
    const customListener = vi.fn();
    on(AFM_MODEL_ID, "msg:custom", customListener);
    const content = {
      id: "cmd-output-view",
      kind: "anywidget-command",
      name: "source.push",
      msg: { port: "letters", payload: [97, 98] },
    };
    const response = {
      id: "cmd-output-view",
      kind: "anywidget-command-response",
      response: { accepted: true, port: "letters" },
    };
    invokeMock.mockResolvedValueOnce(response);

    render(<OutputView value={outputValue()} />);
    window.dispatchEvent(
      new MessageEvent("message", {
        data: {
          source: "jute-afm",
          type: "send",
          modelId: AFM_MODEL_ID,
          content,
          buffers: [[1, 2, 3]],
        },
      }),
    );

    await waitFor(() => expect(customListener).toHaveBeenCalledTimes(1));
    expect(invokeMock).toHaveBeenCalledWith("anywidget_command", {
      intent: { ...content, commId: AFM_MODEL_ID, buffers: [[1, 2, 3]] },
    });
    expect(customListener).toHaveBeenCalledWith(response, []);
  });
});

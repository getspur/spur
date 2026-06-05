import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import OutputView from "./OutputView";

const WIDGET_VIEW_MIME = "application/vnd.jupyter.widget-view+json";
const JUTE_APP_MIME = "application/vnd.jute.app+json";

function outputValue(appPayload: unknown) {
  return {
    status: "success",
    outputs: [
      {
        output_type: "display_data",
        data: {
          [WIDGET_VIEW_MIME]: {
            version_major: 2,
            version_minor: 1,
            model_id: "jute-app-model",
          },
          [JUTE_APP_MIME]: appPayload,
          "text/plain": "jute app",
        },
        metadata: {},
      },
    ],
  } as any;
}

describe("OutputView Jute app rendering", () => {
  afterEach(() => cleanup());

  test("renders a standard anywidget placeholder when Jute app payload is not available", () => {
    render(
      <OutputView
        value={
          {
            status: "success",
            outputs: [
              {
                output_type: "display_data",
                data: {
                  [WIDGET_VIEW_MIME]: {
                    version_major: 2,
                    version_minor: 1,
                    model_id: "model-only",
                  },
                },
                metadata: {},
              },
            ],
          } as any
        }
      />,
    );

    expect(screen.getByText("anywidget model")).toBeInTheDocument();
    expect(screen.getByText("model-only")).toBeInTheDocument();
    expect(
      screen.getByText("waiting for Jute app payload"),
    ).toBeInTheDocument();
  });

  test("renders a Jute cell app iframe from the app payload", () => {
    render(
      <OutputView
        value={outputValue({
          appId: "cloud-controller",
          title: "Cloud Controller",
          height: 280,
          state: { serverStatus: "Stopped", cpuUsage: 0 },
          esm: `
            export function render({ model, el }) {
              el.innerHTML = "<strong>" + model.get("serverStatus") + "</strong>";
            }
          `,
        })}
      />,
    );

    const iframe = screen.getByTitle("Cloud Controller");
    expect(iframe).toHaveAttribute(
      "sandbox",
      expect.stringContaining("allow-scripts"),
    );
    expect(iframe).toHaveAttribute(
      "srcdoc",
      expect.stringContaining("cloud-controller"),
    );
    expect(iframe).toHaveAttribute(
      "srcdoc",
      expect.stringContaining("serverStatus"),
    );
  });

  test("updates the hosted app document when display data changes", () => {
    const { rerender } = render(
      <OutputView
        value={outputValue({
          appId: "cloud-controller",
          title: "Cloud Controller",
          state: { serverStatus: "Stopped" },
          esm: "export function render({ model, el }) { el.textContent = model.get('serverStatus'); }",
        })}
      />,
    );

    expect(screen.getByTitle("Cloud Controller")).toHaveAttribute(
      "srcdoc",
      expect.stringContaining('"serverStatus":"Stopped"'),
    );

    rerender(
      <OutputView
        value={outputValue({
          appId: "cloud-controller",
          title: "Cloud Controller",
          state: { serverStatus: "Running" },
          esm: "export function render({ model, el }) { el.textContent = model.get('serverStatus'); }",
        })}
      />,
    );

    expect(screen.getByTitle("Cloud Controller")).toHaveAttribute(
      "srcdoc",
      expect.stringContaining('"serverStatus":"Running"'),
    );
  });
});

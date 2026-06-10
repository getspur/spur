import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { type StoreApi, createStore } from "zustand/vanilla";

import { useSettings } from "@/stores/settings";

import { ScriptsDisabledBanner } from "./AppGrantPrompt";

const mocks = vi.hoisted(() => ({
  notebook: undefined as
    | {
        store: StoreApi<any>;
      }
    | undefined,
}));

vi.mock("@/stores/notebook", () => ({
  useNotebook: () => {
    if (!mocks.notebook) throw new Error("Notebook mock not configured");
    return mocks.notebook;
  },
}));

const APP_ROOT = "/apps/html-video";

function createAppOpenStore() {
  return createStore<any>()(() => ({
    viewState: {
      appOpenInfo: {
        open_mode: "app",
        app_name: "HTML Video",
        app_root: APP_ROOT,
        capabilities: {
          active_output_scripts: true,
          canvas_capture: true,
          artifacts_dir: false,
          ports: true,
        },
        skill: "",
      },
    },
  }));
}

describe("ScriptsDisabledBanner", () => {
  afterEach(() => {
    cleanup();
    useSettings.getState().reset();
    mocks.notebook = undefined;
  });

  test("renders the denied-grant banner without an infinite render loop", () => {
    useSettings.getState().setAppGrant(APP_ROOT, false);
    mocks.notebook = { store: createAppOpenStore() };

    // With an unstable zustand v5 selector (fresh object per getSnapshot),
    // this render throws "Maximum update depth exceeded" (React error #185).
    render(<ScriptsDisabledBanner />);

    expect(screen.getByRole("status")).toHaveTextContent(
      "Output scripts disabled for HTML Video",
    );
  });

  test("re-prompt revokes the grant and hides the banner", () => {
    useSettings.getState().setAppGrant(APP_ROOT, false);
    mocks.notebook = { store: createAppOpenStore() };

    render(<ScriptsDisabledBanner />);

    fireEvent.click(screen.getByRole("button", { name: "re-prompt" }));

    expect(useSettings.getState().appGrants[APP_ROOT]).toBeUndefined();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });
});

import { afterEach, describe, expect, test } from "vitest";

import { DEFAULT_SETTINGS, useSettings } from "../settings";

describe("settings store", () => {
  afterEach(() => {
    useSettings.getState().reset();
  });

  test("defaults conservative notebook rendering settings to disabled", () => {
    expect(useSettings.getState().markdown).toEqual(DEFAULT_SETTINGS.markdown);
    expect(useSettings.getState().output).toEqual(DEFAULT_SETTINGS.output);
  });

  test("updates markdown Mermaid and HTML active-content settings independently", () => {
    useSettings.getState().setMarkdownMermaid(true);
    useSettings.getState().setOutputActiveContent(true);

    expect(useSettings.getState().markdown.mermaid).toBe(true);
    expect(useSettings.getState().output.activeContent).toBe(true);

    useSettings.getState().setMarkdownMermaid(false);

    expect(useSettings.getState().markdown.mermaid).toBe(false);
    expect(useSettings.getState().output.activeContent).toBe(true);
  });
});

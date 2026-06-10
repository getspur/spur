import { afterEach, describe, expect, test } from "vitest";

import {
  DEFAULT_SETTINGS,
  useEffectiveActiveContent,
  useSettings,
} from "../settings";

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

describe("app grants", () => {
  afterEach(() => {
    useSettings.getState().reset();
  });

  test("starts with no grants", () => {
    expect(useSettings.getState().appGrants).toEqual({});
  });

  test("setAppGrant persists an allow grant keyed by app root", () => {
    useSettings.getState().setAppGrant("/my/app", true);
    const grant = useSettings.getState().appGrants["/my/app"];
    expect(grant).toBeDefined();
    expect(grant.activeOutputScripts).toBe(true);
    expect(typeof grant.grantedAt).toBe("string");
  });

  test("setAppGrant persists a deny grant (activeOutputScripts false)", () => {
    useSettings.getState().setAppGrant("/my/app", false);
    const grant = useSettings.getState().appGrants["/my/app"];
    expect(grant).toBeDefined();
    expect(grant.activeOutputScripts).toBe(false);
  });

  test("revokeAppGrant removes an existing grant", () => {
    useSettings.getState().setAppGrant("/my/app", true);
    useSettings.getState().revokeAppGrant("/my/app");
    expect(useSettings.getState().appGrants["/my/app"]).toBeUndefined();
  });

  test("revokeAppGrant is a no-op for unknown roots", () => {
    expect(() =>
      useSettings.getState().revokeAppGrant("/no-such-app"),
    ).not.toThrow();
  });

  test("reset clears all grants", () => {
    useSettings.getState().setAppGrant("/my/app", true);
    useSettings.getState().reset();
    expect(useSettings.getState().appGrants).toEqual({});
  });
});

describe("useEffectiveActiveContent", () => {
  afterEach(() => {
    useSettings.getState().reset();
  });

  test("returns global toggle when no app root given", () => {
    useSettings.getState().setOutputActiveContent(true);
    expect(useEffectiveActiveContent(undefined)).toBe(true);
    useSettings.getState().setOutputActiveContent(false);
    expect(useEffectiveActiveContent(undefined)).toBe(false);
  });

  test("returns global toggle when no grant exists for app root", () => {
    useSettings.getState().setOutputActiveContent(true);
    expect(useEffectiveActiveContent("/my/app")).toBe(true);
    useSettings.getState().setOutputActiveContent(false);
    expect(useEffectiveActiveContent("/my/app")).toBe(false);
  });

  test("returns grant value when a grant exists, overriding global toggle", () => {
    useSettings.getState().setOutputActiveContent(false);
    useSettings.getState().setAppGrant("/my/app", true);
    expect(useEffectiveActiveContent("/my/app")).toBe(true);
  });

  test("grant false overrides global true", () => {
    useSettings.getState().setOutputActiveContent(true);
    useSettings.getState().setAppGrant("/my/app", false);
    expect(useEffectiveActiveContent("/my/app")).toBe(false);
  });
});

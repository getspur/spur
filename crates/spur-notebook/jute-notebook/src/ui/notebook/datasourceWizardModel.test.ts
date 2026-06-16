import { describe, expect, test } from "vitest";

import {
  DATASOURCE_SOURCE_FAMILIES,
  DATASOURCE_WIZARD_STEPS,
  REST_API_SOURCE_MODES,
  REST_API_WIZARD_STEPS,
  datasourceFamilyByKey,
  datasourceWizardStepByKey,
} from "./datasourceWizardModel";

describe("datasourceWizardModel", () => {
  test("defines the generalized source families with required metadata", () => {
    expect(DATASOURCE_SOURCE_FAMILIES.map((family) => family.key)).toEqual([
      "file",
      "cloud_object_storage",
      "lakehouse",
      "database",
      "rest_api",
      "rss",
      "advanced_sql",
    ]);

    for (const family of DATASOURCE_SOURCE_FAMILIES) {
      expect(family.label).not.toEqual("");
      expect(family.shortDetail).not.toEqual("");
      expect(family.duckDbMechanism).not.toEqual("");
      expect(family.setupRequirements.length).toBeGreaterThan(0);
      expect(family.defaultExampleInput).not.toEqual("");
    }

    expect(datasourceFamilyByKey.rest_api.duckDbMechanism).toContain("httpfs");
    expect(datasourceFamilyByKey.rss.defaultExampleInput).toContain("rsshub://");
  });

  test("marks only attachable schema-backed families as attachable", () => {
    expect(
      DATASOURCE_SOURCE_FAMILIES.filter((family) => family.attachable).map(
        (family) => family.key,
      ),
    ).toEqual(["file", "rest_api", "rss"]);
    expect(
      DATASOURCE_SOURCE_FAMILIES.filter((family) => !family.attachable).map(
        (family) => family.key,
      ),
    ).toEqual([
      "cloud_object_storage",
      "lakehouse",
      "database",
      "advanced_sql",
    ]);

    for (const family of DATASOURCE_SOURCE_FAMILIES.filter(
      (family) => !family.attachable,
    )) {
      expect(family.attachUnavailableReason).toMatch(/attach contract/i);
    }
  });

  test("defines generalized wizard steps and a REST-compatible subset", () => {
    expect(DATASOURCE_WIZARD_STEPS.map((step) => step.key)).toEqual([
      "source",
      "locate",
      "auth",
      "inspect",
      "attach",
    ]);

    expect(REST_API_WIZARD_STEPS.map((step) => step.key)).toEqual([
      "source",
      "auth",
      "inspect",
      "attach",
    ]);
    expect(datasourceWizardStepByKey.auth.label).toEqual("Auth");
  });

  test("keeps existing REST API source modes available for the wizard", () => {
    expect(REST_API_SOURCE_MODES.map((mode) => mode.key)).toEqual([
      "catalog",
      "saved",
      "openapi",
      "manual",
    ]);
    expect(REST_API_SOURCE_MODES.every((mode) => mode.family === "rest_api"))
      .toBe(true);
  });
});

import { describe, expect, it } from "vitest";
import type {
  ProjectConfiguration,
  SettingsConfigurationV2,
} from "../configuration";
import {
  settingsDocumentsMatch,
  settingsSaveContentIssue,
} from "./settingsSavePostcondition";

function document(project: ProjectConfiguration): SettingsConfigurationV2 {
  return {
    approvals: { defaultMode: "ask_for_approval" },
    schemaVersion: 2,
    providers: [],
    modelTiers: [],
    credentials: [],
    tools: [],
    extensions: [],
    mcpServers: [],
    externalAgents: [],
    data: {
      portableHistoryEnabled: false,
      detailedCaptureEnabled: false,
      portableDirectory: ".aworkit/sessions",
    },
    projects: [project],
    appearance: { mode: "system", fontScale: 1 },
    chatDefaults: {},
    layout: {},
  };
}

const project: ProjectConfiguration = {
  id: "project.a",
  name: "Project A",
  workspace: { kind: "local_directory", location: "C:\\src\\A" },
  defaultWorkflowId: "workflow.simple-chat",
  portableHistoryEnabled: false,
};

describe("settings save postcondition", () => {
  it("accepts the core's absent encoding of an unset optional field", () => {
    // The trusted core skips `Option::None`, so a newly added project comes back
    // from the canonical snapshot without `defaultWorkflowId` at all.
    const submitted = document({ ...project, defaultWorkflowId: null });
    const committed = document({ ...project });
    delete committed.projects[0]!.defaultWorkflowId;

    expect(settingsSaveContentIssue(committed, submitted)).toBeNull();
    expect(settingsDocumentsMatch(committed, submitted)).toBe(true);
  });

  it("still rejects a canonical document that lost or changed a value", () => {
    const submitted = document({ ...project, defaultWorkflowId: null });
    const dropped = document({ ...project });
    delete dropped.projects[0]!.defaultWorkflowId;
    dropped.projects[0]!.name = "Other";

    const resolved = document({
      ...project,
      defaultWorkflowId: "workflow.standard-agent",
    });
    expect(settingsSaveContentIssue(dropped, submitted)).not.toBeNull();
    expect(settingsSaveContentIssue(resolved, submitted)).not.toBeNull();
  });

  it("does not confuse a null value with a present one on either side", () => {
    expect(
      settingsDocumentsMatch(
        { providers: [{ credentialRef: null }] },
        { providers: [{ credentialRef: "credential.a" }] },
      ),
    ).toBe(false);
    expect(
      settingsDocumentsMatch(
        { providers: [{ credentialRef: null }] },
        { providers: [{}] },
      ),
    ).toBe(true);
  });

  it("compares array order and length exactly", () => {
    expect(
      settingsDocumentsMatch({ tools: ["a", "b"] }, { tools: ["b", "a"] }),
    ).toBe(false);
    expect(settingsDocumentsMatch({ tools: ["a"] }, { tools: ["a", "b"] })).toBe(
      false,
    );
  });
});

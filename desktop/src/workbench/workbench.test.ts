import { describe, expect, it } from "vitest";
import { darkTokens, lightTokens, resolveAppearance } from "./appearance";
import { ProjectionGateway, type ProjectionReducer } from "./projection";
import { canCommitDraft, resolveCapabilities } from "./settings";
import {
  createEditor,
  editWorkflow,
  parseWorkflow,
  redoWorkflow,
  serializeWorkflow,
  undoWorkflow,
  workflowSummary,
} from "./workflow";

describe("compact appearance contract", () => {
  it("resolves System before rendering and keeps semantic token roles in parity", () => {
    expect(
      resolveAppearance("system", {
        prefersDark: true,
        forcedColors: false,
        reducedMotion: false,
      }),
    ).toBe("dark");
    expect(Object.keys(lightTokens)).toEqual(Object.keys(darkTokens));
    expect(lightTokens.text).not.toBe(lightTokens.window);
  });
});

describe("ordered core projections", () => {
  it("marks sequence gaps stale and only clears them with a newer snapshot", () => {
    const reducer: ProjectionReducer<string[]> = {
      initial: [],
      reduce: (model, event) => [...model, event.kind],
    };
    const gateway = new ProjectionGateway(reducer);
    gateway.receiveEvent({ sequence: 2, kind: "gap", payload: {} });
    expect(gateway.snapshot().stale).toBe(true);
    gateway.resynchronize(2, ["snapshot"]);
    expect(gateway.snapshot()).toMatchObject({
      stale: false,
      sequence: 2,
      model: ["snapshot"],
    });
    gateway.receiveEvent({ sequence: 3, kind: "delta", payload: {} });
    expect(gateway.snapshot().model).toEqual(["snapshot", "delta"]);
  });
});

describe("lossless workflow kernel", () => {
  it("preserves unknown fields through edit, undo, redo, and serialization", () => {
    const original = parseWorkflow(
      '{"schemaVersion":1,"nodes":[{"id":"a","type":"future","future":{"v":2}}],"edges":[],"unknown":{"kept":true}}',
    );
    const edited = editWorkflow(createEditor(original), (document) => ({
      ...document,
      comments: "added",
    }));
    expect(workflowSummary(edited.document)).toEqual({
      nodes: 1,
      edges: 0,
      unresolved: 0,
      issues: 0,
    });
    expect(JSON.parse(serializeWorkflow(edited.document)).unknown).toEqual({
      kept: true,
    });
    expect(undoWorkflow(edited).document.comments).toBeUndefined();
    expect(redoWorkflow(undoWorkflow(edited)).document.comments).toBe("added");
  });
});

describe("settings drafts", () => {
  it("reports requirements without committing a stale draft", () => {
    const configured = new Set(["model"]);
    expect(
      resolveCapabilities(
        [
          { id: "model", label: "Model" },
          { id: "tool", label: "Tool" },
        ],
        configured,
      ).missing,
    ).toHaveLength(1);
    expect(
      canCommitDraft(
        {
          version: 2,
          appearance: "system",
          configuredCapabilities: configured,
        },
        1,
      ),
    ).toBe(false);
  });
});

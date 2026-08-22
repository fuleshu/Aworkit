import { readFileSync } from "node:fs";
import { performance } from "node:perf_hooks";
import { describe, expect, it } from "vitest";
import { contrastRatio, darkTokens, lightTokens } from "./appearance";
import { projectWorkflowSurface } from "./graphSurface";
import {
  addWorkflowNode,
  coalesceWorkflowEdits,
  commitPropertyDraft,
  connectWorkflowNodes,
  createPropertyDraft,
  createEditor,
  moveWorkflowNode,
  parseWorkflow,
  serializeWorkflow,
  updatePropertyDraft,
  validateWorkflow,
} from "./workflow";

describe("desktop design and workflow contracts", () => {
  it("meets token contrast and exact accepted pane geometry", () => {
    for (const tokens of [lightTokens, darkTokens]) {
      expect(contrastRatio(tokens.text, tokens.panel)).toBeGreaterThanOrEqual(
        4.5,
      );
      expect(
        contrastRatio(tokens.secondary, tokens.panel),
      ).toBeGreaterThanOrEqual(4.5);
      expect(
        contrastRatio(tokens.control, tokens.panel),
      ).toBeGreaterThanOrEqual(3);
    }
    const css = readFileSync(new URL("../styles.css", import.meta.url), "utf8");
    for (const contract of [
      "208px",
      "320px",
      "48px",
      "28px",
      "6px",
      "@media (max-width: 1180px)",
      "@media (max-width: 900px)",
      "forced-colors",
      "prefers-reduced-motion",
    ])
      expect(css).toContain(contract);
    const html = readFileSync(
      new URL("../../index.html", import.meta.url),
      "utf8",
    );
    expect(html).not.toContain("localStorage");
    expect(html).toContain('data-appearance="dark"');
    expect(html).toContain('data-appearance-ready="true"');
    expect(html).toContain("visibility: hidden");
    expect(html.indexOf("prefers-color-scheme")).toBeLessThan(
      html.indexOf("/src/main.tsx"),
    );
    const entry = readFileSync(new URL("../main.tsx", import.meta.url), "utf8");
    expect(entry.indexOf("createSettingsCorePort().snapshot()")).toBeLessThan(
      entry.indexOf("createRoot(root!).render"),
    );
  });

  it("preserves unknown workflow fields through typed graph operations", () => {
    const original = parseWorkflow(
      '{"schemaVersion":1,"nodes":[{"id":"a","type":"input","future":{"x":1}},{"id":"b","type":"model"}],"edges":[],"futureRoot":{"retained":true}}',
    );
    let editor = createEditor(original);
    editor = moveWorkflowNode(editor, "a", { x: 10, y: 20 });
    editor = connectWorkflowNodes(editor, "a", "b");
    editor = addWorkflowNode(editor, "tool", { x: 30, y: 40 });
    const roundTrip = JSON.parse(serializeWorkflow(editor.document));
    expect(roundTrip.futureRoot).toEqual({ retained: true });
    expect(roundTrip.nodes[0].future).toEqual({ x: 1 });
    expect(validateWorkflow(editor.document)).toEqual([]);
  });

  it("keeps representative 1,000-node kernel interactions inside the frame-budget gate", () => {
    const document = {
      schemaVersion: 1,
      nodes: Array.from({ length: 1_000 }, (_, index) => ({
        id: `node.${index}`,
        type: "model",
        position: { x: index % 50, y: Math.floor(index / 50) },
      })),
      edges: [],
    };
    const initial = createEditor(document);
    const start = performance.now();
    const editor = moveWorkflowNode(initial, "node.999", {
      x: 100,
      y: 200,
    });
    const surface = projectWorkflowSurface(editor);
    const elapsed = performance.now() - start;
    expect(editor.document.nodes).toHaveLength(1_000);
    expect(surface.nodes).toHaveLength(1_000);
    expect(elapsed).toBeLessThan(16);
  });

  it("projects typed ports, groups, cycles, self-loops, and multi-edges losslessly", () => {
    const editor = createEditor(
      parseWorkflow(
        JSON.stringify({
          schemaVersion: 1,
          nodes: [
            {
              id: "group",
              type: "group",
              width: 480,
              height: 260,
            },
            {
              id: "child",
              type: "model",
              parentId: "group",
              inputPorts: ["prompt", "context"],
              outputPorts: ["answer", "trace"],
            },
          ],
          edges: [
            { id: "self.1", source: "child", target: "child" },
            { id: "self.2", source: "child", target: "child" },
            { id: "cycle", source: "group", target: "child" },
          ],
        }),
      ),
    );
    const surface = projectWorkflowSurface(editor);
    const child = surface.nodes.find((node) => node.id === "child");
    expect(child?.parentId).toBe("group");
    expect(child?.extent).toBe("parent");
    expect(child?.data.inputPorts).toEqual(["prompt", "context"]);
    expect(child?.data.outputPorts).toEqual(["answer", "trace"]);
    expect(surface.edges.map((edge) => edge.id)).toEqual([
      "self.1",
      "self.2",
      "cycle",
    ]);
    expect(surface.edges[0]?.type).toBe("smoothstep");
  });

  it("coalesces transactions and validates schema-driven property drafts", () => {
    const document = parseWorkflow(
      '{"schemaVersion":1,"nodes":[{"id":"a","type":"model","label":"A"}],"edges":[]}',
    );
    const state = createEditor(document);
    const coalesced = coalesceWorkflowEdits(state, [
      (value) => ({ ...value, comments: "one" }),
      (value) => ({ ...value, futureField: { retained: true } }),
    ]);
    expect(coalesced.undo).toHaveLength(1);
    const schema = [
      { key: "label", label: "Label", type: "string", required: true },
      { key: "temperature", label: "Temperature", type: "number" },
    ] as const;
    let draft = createPropertyDraft(coalesced, "a", schema);
    draft = updatePropertyDraft(draft, schema, "label", "Updated");
    draft = updatePropertyDraft(draft, schema, "temperature", "0.2");
    const committed = commitPropertyDraft(coalesced, draft);
    expect(committed.document.nodes[0]).toMatchObject({
      label: "Updated",
      temperature: 0.2,
    });
    expect((committed.document as Record<string, unknown>).futureField).toEqual(
      { retained: true },
    );
  });
});

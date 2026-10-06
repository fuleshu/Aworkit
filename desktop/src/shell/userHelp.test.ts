import { describe, expect, it } from "vitest";
import {
  USER_HELP_DOCUMENT_IDS,
  USER_HELP_HOME_ID,
  resolveUserHelpTarget,
  slugifyHeading,
  userHelpDocument,
  userHelpHomeDocument,
} from "./userHelp";

describe("userHelp registry", () => {
  it("bundles every document the documentation links to", () => {
    for (const id of [
      "index",
      "getting-started",
      "workflows",
      "features",
      "tools",
      "mcp",
      "plugins",
      "comfyui",
      "optional-extras",
      "further-reading",
    ]) {
      expect(userHelpDocument(id), `${id}.md should be bundled`).not.toBeNull();
    }
    expect(USER_HELP_DOCUMENT_IDS).toContain(USER_HELP_HOME_ID);
  });

  it("derives each title from its first level-one heading", () => {
    expect(userHelpHomeDocument().title).toBe("Aworkit documentation");
    expect(userHelpDocument("getting-started")?.title).toBe(
      "Getting started: providers and model tiers",
    );
  });

  it("returns null for an unbundled id", () => {
    expect(userHelpDocument("not-a-document")).toBeNull();
  });
});

describe("resolveUserHelpTarget", () => {
  it("resolves a bundled sibling document with and without an anchor", () => {
    expect(resolveUserHelpTarget("getting-started.md")).toEqual({
      kind: "document",
      documentId: "getting-started",
      anchor: null,
    });
    expect(resolveUserHelpTarget("./tools.md")).toEqual({
      kind: "document",
      documentId: "tools",
      anchor: null,
    });
    expect(resolveUserHelpTarget("plugins.md#skills")).toEqual({
      kind: "document",
      documentId: "plugins",
      anchor: "skills",
    });
  });

  it("resolves an in-page anchor", () => {
    expect(resolveUserHelpTarget("#the-interface")).toEqual({
      kind: "anchor",
      anchor: "the-interface",
    });
  });

  it("resolves the in-app documents-folder link", () => {
    expect(resolveUserHelpTarget("aworkit:documents")).toEqual({
      kind: "documents-folder",
    });
    expect(resolveUserHelpTarget("AWORKIT:Documents")).toEqual({
      kind: "documents-folder",
    });
  });

  it("keeps absolute HTTP(S) links external", () => {
    expect(
      resolveUserHelpTarget("https://github.com/fuleshu/Aworkit/blob/main/README.md"),
    ).toEqual({
      kind: "external",
      url: "https://github.com/fuleshu/Aworkit/blob/main/README.md",
    });
  });

  it("treats a missing document, another scheme or nothing as unsupported", () => {
    expect(resolveUserHelpTarget("missing.md").kind).toBe("unsupported");
    expect(resolveUserHelpTarget("mailto:someone@example.com").kind).toBe(
      "unsupported",
    );
    expect(resolveUserHelpTarget("").kind).toBe("unsupported");
    expect(resolveUserHelpTarget(undefined).kind).toBe("unsupported");
  });
});

describe("slugifyHeading", () => {
  it("matches the heading anchors used inside the documents", () => {
    expect(slugifyHeading("The interface")).toBe("the-interface");
    expect(slugifyHeading("MCP servers")).toBe("mcp-servers");
    expect(slugifyHeading("ComfyUI")).toBe("comfyui");
    expect(slugifyHeading("Step 1 — Add a model provider")).toBe(
      "step-1-add-a-model-provider",
    );
  });
});

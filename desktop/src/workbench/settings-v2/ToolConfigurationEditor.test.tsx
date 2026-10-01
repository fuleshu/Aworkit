// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ToolConfigurationEditor } from "./ToolConfigurationEditor";
import type {
  BuiltInToolConfiguration,
  ExternalAgentConfiguration,
} from "../configuration";

afterEach(cleanup);

const tool: BuiltInToolConfiguration = {
  id: "tool.files.write",
  name: "Write",
  enabled: true,
  requiresProject: false,
  credentialBindings: [],
  configuration: {},
};

describe("tool configuration fields", () => {
  it("lays a boolean field out beside its label and a text field above its control", () => {
    render(
      <ToolConfigurationEditor tool={tool} onChange={vi.fn()} />,
    );
    // A boolean is one row: control first, then the label and its help.
    const checkbox = screen.getByRole("checkbox", {
      name: /Approval policy applies/,
    });
    const booleanField = checkbox.closest("label");
    expect(booleanField).toHaveClass("settings-checkbox-field");
    expect(booleanField?.firstElementChild).toBe(checkbox);
    expect(checkbox.nextElementSibling?.tagName).toBe("SPAN");
    // Every other kind keeps its label above a full-width control.
    const authority = screen.getByLabelText("Authority boundary");
    expect(authority.closest("label")).not.toHaveClass(
      "settings-checkbox-field",
    );
    // The full-width rule must not reach a checkbox, or the box stretches
    // across the field and its glyph centres.
    const css = readFileSync(resolve(process.cwd(), "src/styles.css"), "utf8");
    const widthRule = css.slice(
      css.indexOf(".settings-field > input"),
      css.indexOf("}", css.indexOf(".settings-field > input")),
    );
    expect(widthRule).toContain(':not([type="checkbox"])');
    expect(widthRule).toContain(':not([type="radio"])');
  });
});

describe("external delegation readiness", () => {
  const delegation: BuiltInToolConfiguration = {
    id: "tool.subagent_codex",
    name: "Codex agent delegation",
    enabled: true,
    requiresProject: false,
    credentialBindings: [],
    configuration: { targetId: "" },
  };
  const target = (
    id: string,
    adapter: string,
    enabled: boolean,
    name: string,
  ): ExternalAgentConfiguration => ({
    id,
    name,
    adapter,
    enabled,
    connection: {
      transport: "stdio",
      command: "codex",
      args: ["app-server"],
      cwd: null,
      env: [],
    },
    credentialBindings: [],
    mcpServerIds: [],
    capabilities: {
      progress: false,
      continuation: false,
      cancellation: false,
      approvals: false,
    },
    configuration: {},
  });

  it("reports only this product's connected target and offers no second setup", () => {
    render(
      <ToolConfigurationEditor
        externalAgents={[
          target("agent.one", "codex_app_server", true, "Work Codex"),
          target("agent.two", "codex_app_server", false, "Spare Codex"),
          target("agent.claude", "claude_code", true, "Claude"),
        ]}
        tool={delegation}
        onChange={vi.fn()}
      />,
    );
    // The connection is made under External agents; the tool row only reports it.
    expect(screen.getByText("Connected: Work Codex.")).toBeVisible();
    expect(screen.queryByLabelText("External agent target")).toBeNull();
    expect(screen.queryByRole("option", { name: "Claude" })).toBeNull();
  });

  it("says the product is not connected when it is not ready", () => {
    render(
      <ToolConfigurationEditor
        externalAgents={[
          target("agent.claude", "claude_code", true, "Claude"),
        ]}
        tool={delegation}
        onChange={vi.fn()}
      />,
    );
    expect(
      screen.getByText(/Not connected yet\. Connect Codex under External agents\./),
    ).toBeVisible();
  });
});

// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ToolConfigurationEditor } from "./ToolConfigurationEditor";
import type { BuiltInToolConfiguration } from "../configuration";

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
  it("renders a boolean field as a checkbox beside its label", () => {
    render(
      <ToolConfigurationEditor
        tool={tool}
        onChange={vi.fn()}
        onPickCommand={undefined}
      />,
    );
    const checkbox = screen.getByRole("checkbox", {
      name: /Approval policy applies/,
    });
    const field = checkbox.closest("label");
    expect(field).toHaveClass("settings-field");
    expect(field).toHaveClass("settings-checkbox-field");
    // The CSS lays this out as two columns: the control, then the label text.
    expect(field?.firstElementChild).toBe(checkbox);
    expect(checkbox.nextElementSibling?.tagName).toBe("SPAN");
    expect(checkbox.nextElementSibling).toHaveTextContent(
      "Approval policy applies",
    );
  });

  it("keeps a text field's label above a full-width control", () => {
    render(
      <ToolConfigurationEditor
        tool={tool}
        onChange={vi.fn()}
        onPickCommand={undefined}
      />,
    );
    const authority = screen.getByLabelText("Authority boundary");
    const field = authority.closest("label");
    expect(field).toHaveClass("settings-field");
    expect(field).not.toHaveClass("settings-checkbox-field");
  });

  it("never stretches a checkbox with the text-control width rule", () => {
    // The editor renders booleans and text controls through the same
    // `.settings-field` label, so the full-width rule must exclude checkboxes;
    // otherwise every boolean manifest field renders as a huge centred box.
    const css = readFileSync(
      resolve(process.cwd(), "src/styles.css"),
      "utf8",
    );
    const fullWidth = css.slice(
      css.indexOf(".settings-field > input"),
      css.indexOf("}", css.indexOf(".settings-field > input")),
    );
    expect(fullWidth).toContain(":not([type=\"checkbox\"])");
    expect(fullWidth).toContain(":not([type=\"radio\"])");
    expect(css).toContain(".settings-field.settings-checkbox-field");
  });
});

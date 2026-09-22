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

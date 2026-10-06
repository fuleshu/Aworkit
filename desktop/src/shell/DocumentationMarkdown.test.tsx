// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DocumentationMarkdown } from "./DocumentationMarkdown";

afterEach(cleanup);

describe("DocumentationMarkdown", () => {
  it("keeps the Aworkit documents-folder link usable", () => {
    const onFollow = vi.fn();
    render(
      <DocumentationMarkdown
        markdown="[Open the Aworkit folder](aworkit:documents)"
        onFollow={onFollow}
      />,
    );

    // React Markdown's default transform would blank this scheme; the anchor
    // must keep its href so the click still routes to the folder action.
    const link = screen.getByRole("link", { name: "Open the Aworkit folder" });
    expect(link).toHaveAttribute("href", "aworkit:documents");

    fireEvent.click(link);

    expect(onFollow).toHaveBeenCalledWith({ kind: "documents-folder" });
  });

  it("still refuses an unsafe scheme", () => {
    const onFollow = vi.fn();
    render(
      <DocumentationMarkdown
        markdown="[bad](javascript:alert(1))"
        onFollow={onFollow}
      />,
    );

    // The default transform blanks the unsafe URL, so the anchor keeps no
    // dangerous href and the panel treats the activation as unsupported.
    const link = screen.getByText("bad").closest("a");
    expect(link).not.toBeNull();
    expect(link).not.toHaveAttribute("href", "javascript:alert(1)");

    fireEvent.click(link as HTMLElement);

    expect(onFollow).toHaveBeenCalledWith({ kind: "unsupported" });
  });

  it("publishes heading ids for in-page links", () => {
    render(
      <DocumentationMarkdown
        markdown={"## Step 1\n\n[go](#step-1)"}
        onFollow={vi.fn()}
      />,
    );

    expect(screen.getByRole("heading", { level: 2 })).toHaveAttribute(
      "id",
      "step-1",
    );
  });
});

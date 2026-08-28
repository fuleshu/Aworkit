// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { MarkdownContent } from "./MarkdownContent";

afterEach(cleanup);

describe("MarkdownContent", () => {
  it("renders common model Markdown and GFM citations", () => {
    const { container } = render(
      <MarkdownContent>
        {"**Sunny** tomorrow\n\n- Warm\n- Dry\n\n[Forecast](https://example.test/weather)\n\n| High | Low |\n| --- | --- |\n| 27 | 15 |"}
      </MarkdownContent>,
    );

    expect(screen.getByText("Sunny").tagName).toBe("STRONG");
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
    expect(screen.getByRole("link", { name: "Forecast" })).toHaveAttribute(
      "href",
      "https://example.test/weather",
    );
    expect(screen.getByRole("link", { name: "Forecast" })).toHaveAttribute(
      "target",
      "_blank",
    );
    expect(within(container).getByRole("table")).toBeVisible();
  });

  it("does not execute raw HTML, unsafe links, or remote images", () => {
    const { container } = render(
      <MarkdownContent>
        {'<script>alert("no")</script>\n\n[unsafe](javascript:alert(1))\n\n![tracking](https://example.test/pixel.png)'}
      </MarkdownContent>,
    );

    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("a")?.getAttribute("href")).toBe("");
  });
});

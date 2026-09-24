// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { isTauri } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { NotificationProvider } from "../notifications/NotificationContext";
import { NotificationStore } from "../notifications/NotificationStore";
import { MarkdownContent } from "./MarkdownContent";

vi.mock("@tauri-apps/api/core", () => ({ isTauri: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
beforeEach(() => {
  vi.mocked(isTauri).mockReturnValue(true);
  vi.mocked(openUrl).mockReset().mockResolvedValue(undefined);
});
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
    expect(container.querySelector("a")).not.toHaveAttribute("href");
    fireEvent.click(screen.getByText("unsafe"));
    expect(openUrl).not.toHaveBeenCalled();
  });

  it.each(["https://example.test/weather?q=rain#today", "http://example.test/weather"])(
    "opens %s once in the OS default browser without selecting a parent",
    (url) => {
      const select = vi.fn();
      render(<section onClick={select}><MarkdownContent>{`[**Forecast**](${url})`}</MarkdownContent></section>);
      const label = screen.getByText("Forecast");
      expect(fireEvent.click(label)).toBe(false);
      expect(openUrl).toHaveBeenCalledExactlyOnceWith(url);
      expect(select).not.toHaveBeenCalled();
    },
  );

  it("opens keyboard and middle-click activations without duplicating them", async () => {
    const user = userEvent.setup();
    render(<MarkdownContent>{"[Forecast](https://example.test/weather)"}</MarkdownContent>);
    const link = screen.getByRole("link");
    link.focus();
    await user.keyboard("{Enter}");
    expect(openUrl).toHaveBeenCalledTimes(1);
    fireEvent(link, new MouseEvent("auxclick", { button: 1, bubbles: true, cancelable: true }));
    expect(openUrl).toHaveBeenCalledTimes(2);
    fireEvent(link, new MouseEvent("auxclick", { button: 2, bubbles: true, cancelable: true }));
    expect(openUrl).toHaveBeenCalledTimes(2);
  });

  it("keeps unsupported schemes and relative links from navigating the shell", () => {
    render(<MarkdownContent>{"[File](file:///C:/private.txt) [Relative](/settings) [Data](data:text/plain,hello)"}</MarkdownContent>);
    for (const name of ["File", "Relative", "Data"]) {
      expect(screen.getByText(name)).not.toHaveAttribute("href");
      expect(fireEvent.click(screen.getByText(name))).toBe(false);
    }
    expect(openUrl).not.toHaveBeenCalled();
  });

  it("reports an OS opener failure in the existing notification service", async () => {
    vi.mocked(openUrl).mockRejectedValueOnce(new Error("No browser available"));
    const store = new NotificationStore();
    render(<NotificationProvider store={store}><MarkdownContent>{"[Forecast](https://example.test/weather)"}</MarkdownContent></NotificationProvider>);
    fireEvent.click(screen.getByRole("link"));
    await waitFor(() => expect(store.getSnapshot().active).toEqual([
      expect.objectContaining({ severity: "error", summary: "Could not open the link in your default browser." }),
    ]));
    store.dispose();
  });
});
describe("MarkdownContent code blocks", () => {
  const FENCED = "Intro\n\n```js\nconst answer = 42;\n```\n";

  it("offers a copy control for a fenced block and copies its exact source", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    render(<MarkdownContent>{FENCED}</MarkdownContent>);
    const copy = screen.getByRole("button", { name: "Copy this code block" });
    expect(screen.getByText("js")).toBeVisible();
    fireEvent.click(copy);
    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith("const answer = 42;\n"),
    );
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Copied to the clipboard" }),
      ).toBeTruthy(),
    );
  });

  it("labels a block without a language and never copies on its own", () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    render(<MarkdownContent>{"```\nplain text\n```\n"}</MarkdownContent>);
    expect(
      screen.getByRole("button", { name: "Copy this code block" }),
    ).toBeTruthy();
    expect(screen.queryByText("js")).toBeNull();
    expect(writeText).not.toHaveBeenCalled();
  });

  it("falls back to execCommand when the host has no clipboard API", async () => {
    const exec = vi.fn().mockReturnValue(true);
    vi.stubGlobal("navigator", {});
    Object.defineProperty(document, "execCommand", {
      value: exec,
      configurable: true,
    });
    render(<MarkdownContent>{"```\nfallback\n```\n"}</MarkdownContent>);
    fireEvent.click(
      screen.getByRole("button", { name: "Copy this code block" }),
    );
    await waitFor(() => expect(exec).toHaveBeenCalledWith("copy"));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Copied to the clipboard" }),
      ).toBeTruthy(),
    );
    Reflect.deleteProperty(document, "execCommand");
  });

  it("reports a refused clipboard instead of claiming success", async () => {
    const writeText = vi.fn().mockRejectedValue(new Error("denied"));
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    render(<MarkdownContent>{"```\ndenied\n```\n"}</MarkdownContent>);
    fireEvent.click(
      screen.getByRole("button", { name: "Copy this code block" }),
    );
    await waitFor(() => expect(writeText).toHaveBeenCalled());
    expect(
      screen.getByRole("button", { name: "Copy this code block" }),
    ).toBeTruthy();
  });
});

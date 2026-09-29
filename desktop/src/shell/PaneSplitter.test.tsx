// @vitest-environment jsdom
import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PaneSplitter } from "./PaneSplitter";

afterEach(() => {
  vi.restoreAllMocks();
  document.body.classList.remove("pane-resizing");
});

describe("PaneSplitter", () => {
  it("coalesces pointer previews and commits controlled state only at drag end", () => {
    const frames: FrameRequestCallback[] = [];
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      frames.push(callback);
      return frames.length;
    });
    const onPreview = vi.fn();
    const onChange = vi.fn();
    render(
      <PaneSplitter
        label="Resize test pane"
        max={420}
        min={280}
        value={320}
        onChange={onChange}
        onPreview={onPreview}
      />,
    );
    const splitter = screen.getByRole("separator", {
      name: "Resize test pane",
    });

    fireEvent.pointerDown(splitter, {
      button: 0,
      clientX: 100,
      isPrimary: true,
      pointerId: 7,
    });
    fireEvent.pointerMove(splitter, { clientX: 130, pointerId: 7 });
    fireEvent.pointerMove(splitter, { clientX: 145, pointerId: 7 });

    expect(frames).toHaveLength(1);
    expect(onChange).not.toHaveBeenCalled();
    act(() => frames[0]?.(0));
    expect(onPreview).toHaveBeenLastCalledWith(365);
    expect(splitter).toHaveAttribute("aria-valuenow", "365");

    fireEvent.pointerUp(splitter, { pointerId: 7 });
    expect(onChange).toHaveBeenCalledOnce();
    expect(onChange).toHaveBeenCalledWith(365);
    expect(document.body).not.toHaveClass("pane-resizing");
  });

  it("grows a row panel when dragged up and shrinks it with the down arrow", () => {
    const onChange = vi.fn();
    render(
      <PaneSplitter
        label="Resize chat input"
        max={720}
        min={168}
        orientation="horizontal"
        value={200}
        onChange={onChange}
      />,
    );
    const splitter = screen.getByRole("separator", {
      name: "Resize chat input",
    });
    expect(splitter).toHaveAttribute("aria-orientation", "horizontal");
    expect(splitter).toHaveClass("horizontal");

    // Moving the pointer up (a smaller clientY) makes the panel taller.
    fireEvent.pointerDown(splitter, {
      button: 0,
      clientY: 500,
      isPrimary: true,
      pointerId: 3,
    });
    fireEvent.pointerMove(splitter, { clientY: 380, pointerId: 3 });
    fireEvent.pointerUp(splitter, { pointerId: 3 });
    expect(onChange).toHaveBeenLastCalledWith(320);

    fireEvent.keyDown(splitter, { key: "ArrowUp" });
    expect(onChange).toHaveBeenLastCalledWith(208);
    fireEvent.keyDown(splitter, { key: "ArrowDown" });
    expect(onChange).toHaveBeenLastCalledWith(192);
    fireEvent.keyDown(splitter, { key: "End" });
    expect(onChange).toHaveBeenLastCalledWith(720);
    fireEvent.keyDown(splitter, { key: "Home" });
    expect(onChange).toHaveBeenLastCalledWith(168);
  });
});

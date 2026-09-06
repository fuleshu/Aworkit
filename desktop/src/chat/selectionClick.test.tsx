// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { isSelectionClick } from "./selectionClick";

afterEach(cleanup);

it("gives nested native and custom controls priority and selects only unclaimed content", () => {
  const select = vi.fn();
  const action = vi.fn();
  render(
    <section tabIndex={0} onClick={event => { if (isSelectionClick(event)) select(); }}>
      <button onClick={action}><svg><title>Nested icon</title></svg></button>
      <details><summary><strong>Disclosure</strong></summary>Hidden content</details>
      <label>Input label<input aria-label="Field" /></label>
      <span role="button" tabIndex={0} onClick={action}>Custom button</span>
      <span onClick={event => { event.preventDefault(); action(); }}>Claimed click</span>
      <span onClick={event => { event.stopPropagation(); action(); }}>Stopped click</span>
      <p>Plain content</p>
    </section>,
  );
  for (const label of ["Nested icon", "Disclosure", "Input label", "Custom button", "Claimed click", "Stopped click"]) {
    fireEvent.click(screen.getByText(label));
  }
  fireEvent.click(screen.getByRole("textbox"));
  expect(select).not.toHaveBeenCalled();
  expect(action).toHaveBeenCalledTimes(4);
  fireEvent.click(screen.getByText("Plain content"));
  expect(select).toHaveBeenCalledTimes(1);
});

import type { MouseEvent } from "react";

const interactiveTarget = [
  "a", "button", "input", "select", "textarea", "summary", "label",
  "audio[controls]", "video[controls]", '[role="button"]', '[role="link"]',
  "[tabindex]", '[contenteditable]:not([contenteditable="false"])',
].join(",");

/**
 * Run-details selection is a bubbling fallback. Native controls keep their
 * default action; custom controls can claim a click by preventing its default
 * or stopping propagation. Descendants of a control (including SVGs) count too.
 */
export function isSelectionClick(event: MouseEvent<HTMLElement>): boolean {
  if (event.defaultPrevented) return false;
  let target = event.target instanceof Element ? event.target : null;
  while (target !== null && target !== event.currentTarget) {
    if (target.matches(interactiveTarget)) return false;
    target = target.parentElement;
  }
  return true;
}

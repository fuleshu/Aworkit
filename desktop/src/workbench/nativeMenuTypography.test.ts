// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { projectNativeMenuTypography } from "./nativeMenuTypography";
const native = vi.hoisted(() => ({ invoke: vi.fn().mockResolvedValue(undefined) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
afterEach(() => {
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
  document.documentElement.removeAttribute("style");
  native.invoke.mockClear();
});

it.each([[14, 13], [18.2, 16.9], [22.75, 21.125]])("projects the 13px UI role from a %spx reading baseline", async (rootSize, menuSize) => {
  Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
  Object.defineProperty(window, "devicePixelRatio", { configurable: true, value: 1.25 });
  document.documentElement.style.fontSize = `${rootSize}px`;
  document.documentElement.dataset.appearance = "dark";
  projectNativeMenuTypography();
  await vi.waitFor(() => expect(native.invoke).toHaveBeenCalledOnce());
  expect(native.invoke.mock.calls[0][0]).toBe("native_set_menu_font");
  expect(native.invoke.mock.calls[0][1].fontSize).toBeCloseTo(menuSize);
  expect(native.invoke.mock.calls[0][1].dark).toBe(true);
});

it("does not request native menu rendering in a browser", () => {
  projectNativeMenuTypography();
  expect(native.invoke).not.toHaveBeenCalled();
});

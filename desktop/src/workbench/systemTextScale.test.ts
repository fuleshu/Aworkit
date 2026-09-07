// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { projectAppearancePreference } from "./appearance";
import { initializeSystemTextScale, projectSystemTextScale } from "./systemTextScale";

const native = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn(), unlisten: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: native.listen }));
vi.mock("./nativeMenuTypography", () => ({ projectNativeMenuTypography: vi.fn() }));

beforeEach(() => {
  vi.clearAllMocks();
  document.documentElement.removeAttribute("style");
  Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
  native.listen.mockResolvedValue(native.unlisten);
  native.invoke.mockResolvedValue(1.25);
});
afterEach(() => { Reflect.deleteProperty(window, "__TAURI_INTERNALS__"); });

it("keeps OS text size while app previews and color changes are applied", async () => {
  const dispose = await initializeSystemTextScale();
  projectAppearancePreference("dark", 1.3);
  expect(document.documentElement.style.getPropertyValue("--aw-font-scale")).toBe("1.3");
  expect(document.documentElement.style.getPropertyValue("--aw-system-text-scale")).toBe("1.25");
  native.listen.mock.calls[0][1]({ payload: 1.5 });
  projectAppearancePreference("light", 1);
  expect(document.documentElement.style.getPropertyValue("--aw-system-text-scale")).toBe("1.5");
  dispose();
  expect(native.unlisten).toHaveBeenCalledOnce();
});

it("does not overwrite a newer OS event with the startup snapshot", async () => {
  native.invoke.mockImplementation(async () => {
    native.listen.mock.calls[0][1]({ payload: 1.75 });
    return 1.25;
  });
  await initializeSystemTextScale();
  expect(document.documentElement.style.getPropertyValue("--aw-system-text-scale")).toBe("1.75");
});

it("removes its subscription if the initial native read fails", async () => {
  native.invoke.mockRejectedValue(new Error("native read failed"));
  await expect(initializeSystemTextScale()).rejects.toThrow("native read failed");
  expect(native.unlisten).toHaveBeenCalledOnce();
});

it("leaves browser and non-native default font sizing intact", async () => {
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
  await initializeSystemTextScale();
  expect(native.listen).not.toHaveBeenCalled();
  expect(document.documentElement.style.getPropertyValue("--aw-system-text-scale")).toBe("");
});

it.each([NaN, Infinity, 0, -1])("rejects an invalid native scale: %s", value => {
  projectSystemTextScale(value);
  expect(document.documentElement.style.getPropertyValue("--aw-system-text-scale")).toBe("1");
});

it("keeps every app stylesheet free of fixed font sizes and line heights", () => {
  const root = resolve("src");
  for (const path of readdirSync(root, { recursive: true }) as string[]) {
    if (!path.endsWith(".css")) continue;
    const css = readFileSync(resolve(root, path), "utf8");
    expect(css, path).not.toMatch(/(?:font-size|line-height)\s*:[^;{}]*\d(?:px|pt)\b/);
  }
});

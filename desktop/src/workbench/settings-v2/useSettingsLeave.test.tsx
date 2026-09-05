// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SettingsLeaveGuard } from "../../shell/settingsNavigation";
import { useSettingsLeave } from "./useSettingsLeave";

afterEach(cleanup);
describe("Settings leave decisions", () => {
  function setup(initial = { dirty: true, busy: false, mutationVerified: false }) {
    let guard: SettingsLeaveGuard | null = null;
    const register = (value: SettingsLeaveGuard | null) => { guard = value; };
    const save = vi.fn().mockResolvedValue(false), discard = vi.fn().mockResolvedValue(false), leave = vi.fn();
    const hook = renderHook(props => useSettingsLeave({ ...props, save, discard }, register), { initialProps: initial });
    return { ...hook, save, discard, leave, request: () => act(() => guard?.(leave)) };
  }

  it("returns immediately when clean, while Stay preserves a dirty visit", () => {
    const clean = setup({ dirty: false, busy: false, mutationVerified: false });
    clean.request();
    expect(clean.leave).toHaveBeenCalledOnce();
    clean.unmount();
    const dirty = setup();
    dirty.request();
    expect(dirty.result.current.prompt).toBe(true);
    act(() => dirty.result.current.stay());
    expect(dirty.result.current.prompt).toBe(false);
    expect(dirty.leave).not.toHaveBeenCalled();
    expect(dirty.save).not.toHaveBeenCalled();
  });

  it.each(["save", "discard"] as const)("%s returns only after verified completion", async kind => {
    const hook = setup();
    hook.request();
    await act(() => hook.result.current.decide(kind));
    expect(hook.leave).not.toHaveBeenCalled();
    expect(hook.result.current.prompt).toBe(true);
    hook[kind].mockResolvedValue(true);
    await act(() => hook.result.current.decide(kind));
    expect(hook.leave).toHaveBeenCalledOnce();
    expect(hook.result.current.prompt).toBe(false);
    if (kind === "discard") expect(hook.discard).toHaveBeenCalledWith(false);
  });

  it.each([false, true])("waits for an in-flight mutation, verified=%s, without a second command", verified => {
    const hook = setup({ dirty: true, busy: true, mutationVerified: false });
    hook.request();
    expect(hook.result.current.prompt).toBe(false);
    expect(hook.leave).not.toHaveBeenCalled();
    hook.rerender({ dirty: false, busy: false, mutationVerified: verified });
    expect(hook.leave).toHaveBeenCalledTimes(verified ? 1 : 0);
    expect(hook.save).not.toHaveBeenCalled();
    expect(hook.discard).not.toHaveBeenCalled();
  });
});

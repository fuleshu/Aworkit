// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import type { ReactNode } from "react";
import { NotificationProvider } from "../../notifications/NotificationContext";
import { NotificationStore } from "../../notifications/NotificationStore";
import { useSettingsDiagnostics } from "./useSettingsDiagnostics";

afterEach(cleanup);
it.each(["draft", "visit"])("detaches a late diagnostic after its %s changes", async change => {
  const store = new NotificationStore();
  const wrapper = ({ children }: { children: ReactNode }) => <NotificationProvider store={store}>{children}</NotificationProvider>;
  const hook = renderHook(({ active, fingerprint }) => useSettingsDiagnostics("settings:1", active, fingerprint), { wrapper, initialProps: { active: true, fingerprint: "first" } });
  let resolve!: (value: { ok: boolean }) => void;
  const operation = new Promise<{ ok: boolean }>(done => { resolve = done; });
  let result!: Promise<unknown>;
  act(() => { result = hook.result.current("Tool test", () => operation).catch(error => error); });
  expect(store.getSnapshot().active[0]?.severity).toBe("progress");
  hook.rerender({ active: change !== "visit", fingerprint: change === "draft" ? "second" : "first" });
  expect(store.getSnapshot().active).toHaveLength(0);
  await act(async () => { resolve({ ok: true }); await result; });
  expect(store.getSnapshot().active).toHaveLength(0);
  store.dispose();
});

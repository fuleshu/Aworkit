// @vitest-environment jsdom
import { renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ContextModel } from "./contextProjection";
import { useContextModel } from "./useContextModel";

type Fallback = ContextModel | null;

describe("useContextModel", () => {
  it("re-resolves a draft Chat tier when the surface becomes active again", async () => {
    const read = vi.fn(async () => ({
      name: "qwen3.8-flash-next",
      contextWindow: null,
    }));
    const { result, rerender } = renderHook(
      ({ active }: { active: boolean }) =>
        useContextModel(read, "chat.1", "workflow.standard-agent", null, active),
      { initialProps: { active: true } },
    );
    await waitFor(() =>
      expect(result.current?.name).toBe("qwen3.8-flash-next"),
    );
    expect(read).toHaveBeenCalledTimes(1);

    // The Balanced tier is re-mapped in Settings while this Chat is not visible.
    read.mockResolvedValue({ name: "deepseek-flash", contextWindow: null });
    rerender({ active: false });
    rerender({ active: true });

    await waitFor(() => expect(result.current?.name).toBe("deepseek-flash"));
    expect(read).toHaveBeenCalledTimes(2);
  });

  it("never reports a resolution older than the authoritative projection", async () => {
    const read = vi.fn(async () => ({
      name: "qwen3.8-flash-next",
      contextWindow: 32_000,
    }));
    const { result, rerender } = renderHook(
      ({ fallback }: { fallback: Fallback }) =>
        useContextModel(read, "chat.1", "workflow.standard-agent", fallback, true),
      { initialProps: { fallback: null as Fallback } },
    );
    await waitFor(() =>
      expect(result.current?.name).toBe("qwen3.8-flash-next"),
    );

    // The Chat froze its execution context on the newly mapped DeepSeek model.
    rerender({ fallback: { name: "deepseek-flash", contextWindow: null } });
    expect(result.current?.name).toBe("deepseek-flash");
    await waitFor(() => expect(read).toHaveBeenCalledTimes(2));
    expect(result.current?.name).toBe("deepseek-flash");
  });

  it("adopts the fetched capacity only for the model the projection names", async () => {
    const read = vi.fn(async () => ({
      name: "deepseek-flash",
      contextWindow: 65_536,
    }));
    const { result } = renderHook(() =>
      useContextModel(read, "chat.1", "workflow.standard-agent", {
        name: "deepseek-flash",
        contextWindow: null,
      }, true),
    );
    await waitFor(() => expect(result.current?.contextWindow).toBe(65_536));
    expect(result.current?.name).toBe("deepseek-flash");
  });
});

// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { CompactionSettings } from "./CompactionSettings";
import type { ModelConfiguration } from "../configuration";

afterEach(cleanup);

const model = (overrides: Partial<ModelConfiguration> = {}): ModelConfiguration => ({
  id: "model.test",
  name: "Test",
  remoteId: "test",
  enabled: true,
  capabilities: ["text", "tools"],
  parameters: {},
  contextWindow: 262_144,
  ...overrides,
});

it("sizes the summary share from the model's declared window", () => {
  const change = vi.fn();
  render(<CompactionSettings model={model()} providers={[]} onChange={change} />);
  const share = screen.getByTitle(/this share becomes the written summary/i) as HTMLInputElement;
  expect(share.value).toBe("38.2");
  // The read-out states the numbers the policy will actually use, so the panel
  // cannot promise behaviour the runtime does not implement.
  expect(screen.getByText(/targets 65[.,]536 tokens after compaction/)).toBeTruthy();
  expect(screen.getByText(/the summary gets 38% of what is replaced/)).toBeTruthy();
  expect(screen.queryByTitle(/declares no context window/i)).toBeNull();
  fireEvent.change(share, { target: { value: "25" } });
  expect(change.mock.lastCall?.[0].compaction.summaryShare).toBe(0.25);
});

it("offers the absolute tail only when the model declares no window", () => {
  const change = vi.fn();
  render(
    <CompactionSettings model={model({ contextWindow: null })} providers={[]} onChange={change} />,
  );
  expect(screen.queryByTitle(/this share becomes the written summary/i)).toBeNull();
  expect(screen.queryByText(/targets .* tokens after compaction/)).toBeNull();
  // The default matches the policy: no declared window means no derived tail.
  const tail = screen.getByTitle(/declares no context window/i) as HTMLInputElement;
  expect(tail.value).toBe("0");
});

it("explains the reservation a model makes for its own output", () => {
  render(
    <CompactionSettings
      model={model({ maxOutputTokens: 32_768 })}
      providers={[]}
      onChange={vi.fn()}
    />,
  );
  expect(screen.getByText(/reserves 32,768 tokens for its own output/)).toBeTruthy();
});

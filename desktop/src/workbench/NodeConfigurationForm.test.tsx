// @vitest-environment jsdom

import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SettingsV2Snapshot } from "./configuration";
import { NodeConfigurationForm } from "./NodeConfigurationForm";

afterEach(cleanup);

describe("model node reasoning controls", () => {
  it("uses advertised effort values and writes node-scoped overrides", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <NodeConfigurationForm
        configuration={{ modelTierId: "tier:balanced", toolIds: [] }}
        editable
        nodeType="agent"
        settings={settingsWithCapabilities([
          "reasoning",
          "reasoning_effort:low",
          "reasoning_effort:high",
          "thinking_toggle",
        ])}
        onChange={onChange}
      />,
    );

    const effort = screen.getByLabelText("Reasoning effort");
    expect(within(effort).getByRole("option", { name: "Low" })).toBeVisible();
    expect(within(effort).getByRole("option", { name: "High" })).toBeVisible();
    expect(within(effort).queryByRole("option", { name: "Medium" })).toBeNull();
    await user.selectOptions(effort, "high");
    expect(onChange).toHaveBeenCalledWith({ reasoningEffort: "high" });

    const thinking = screen.getByLabelText("Thinking");
    await user.selectOptions(thinking, "false");
    expect(onChange).toHaveBeenCalledWith({ enableThinking: false });
    await user.selectOptions(thinking, "");
    expect(onChange).toHaveBeenCalledWith({ enableThinking: null });
  });

  it("offers the compatible fallback set when the model API has no metadata", () => {
    render(
      <NodeConfigurationForm
        configuration={{ modelTierId: "tier:balanced" }}
        editable
        nodeType="model_call"
        settings={settingsWithCapabilities(["text", "tools"])}
        onChange={vi.fn()}
      />,
    );

    const effort = screen.getByLabelText("Reasoning effort");
    expect(within(effort).getByRole("option", { name: "None" })).toBeVisible();
    expect(within(effort).getByRole("option", { name: "Medium" })).toBeVisible();
    expect(within(effort).getByRole("option", { name: "Max" })).toBeVisible();
  });
});

function settingsWithCapabilities(
  capabilities: readonly string[],
): SettingsV2Snapshot {
  return {
    version: 1,
    schemaVersion: 2,
    providerHealth: [],
    settings: {
      providers: [
        {
          id: "provider.openai",
          name: "OpenAI compatible",
          kind: "openai_compatible",
          baseUrl: "http://localhost:8000/v1",
          enabled: true,
          credentialRef: null,
          configuration: {},
          models: [
            {
              id: "model.chat",
              name: "Chat",
              remoteId: "chat-model",
              enabled: true,
              capabilities: [...capabilities],
              parameters: {},
            },
          ],
        },
      ],
      modelTiers: [
        {
          id: "tier:balanced",
          name: "Balanced",
          kind: "standard",
          resolution: {
            strategy: "exact",
            target: { providerId: "provider.openai", modelId: "model.chat" },
          },
        },
      ],
      tools: [],
      mcpServers: [],
    },
  } as unknown as SettingsV2Snapshot;
}

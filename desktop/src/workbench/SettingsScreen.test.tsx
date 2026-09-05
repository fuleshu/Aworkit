// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { render } from "../test/renderWithNotifications";
import { afterEach, describe, expect, it, vi } from "vitest";
import { projectAppearancePreference } from "./appearance";
import type {
  ExtensionConfiguration,
  SettingsConfigurationV2,
  SettingsV2Snapshot,
} from "./configuration";
import { SettingsScreen } from "./SettingsScreen";
import { ToolsSection } from "./settings-v2/CredentialsToolsSection";
import type { SettingsLeaveGuard } from "../shell/settingsNavigation";
import type {
  CredentialDeleteCommand,
  CredentialStoreCommand,
  CredentialStoreReceipt,
  ExtensionRegisterCommand,
  ExternalAgentProbeRequest,
  ExternalAgentProbeResult,
  McpProbeRequest,
  McpProbeResult,
  ModelDiscoveryRequest,
  ModelDiscoveryResult,
  ProjectProbeRequest,
  ProjectProbeResult,
  ProviderProbeRequest,
  ProviderProbeResult,
  SettingsV2Commit,
  SettingsV2CorePort,
  SettingsV2Receipt,
  ToolProbeRequest,
  ToolProbeResult,
} from "./settingsV2Port";

afterEach(() => {
  cleanup();
  projectAppearancePreference("system", 1);
});

describe("Settings v2 workbench", () => {
  it.each(["conflict", "receipt", "snapshot", "content"])("Save and return preserves the draft after a %s failure", async failure => {
    const port = new RecordingSettingsV2Port();
    if (failure === "conflict") port.conflictOnce = true;
    if (failure === "receipt") port.mutationReceiptCommandIdOverride = "wrong-command";
    if (failure === "content") port.mutationSnapshotContentMismatch = true;
    if (failure === "snapshot") {
      port.mutationSnapshotVersionOffset = -1;
      port.mutationSnapshotFaultCount = 2;
    }
    let guard: SettingsLeaveGuard | null = null;
    const onLeave = vi.fn();
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} registerLeaveGuard={value => { guard = value; }} onBack={() => guard?.(onLeave)} returnLabel="Back to Chat" />);
    const url = await screen.findByLabelText("Base URL");
    await user.clear(url);
    await user.type(url, "https://changed.example/v1");
    await user.click(screen.getByRole("button", { name: "Back to Chat" }));
    await user.click(screen.getByRole("button", { name: "Save and return" }));
    await waitFor(() => expect(port.commits).toHaveLength(1));
    await waitFor(() => expect(screen.getByRole("button", { name: "Stay in Settings" })).toBeEnabled());
    expect(onLeave).not.toHaveBeenCalled();
    expect(url).toHaveValue("https://changed.example/v1");
    expect(screen.getByRole("dialog")).toBeVisible();
  });

  it("Back waits for the existing Save and its canonical refresh before leaving", async () => {
    const port = new RecordingSettingsV2Port();
    const gate = deferred();
    port.commitGate = gate.promise;
    let guard: SettingsLeaveGuard | null = null;
    const onLeave = vi.fn();
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} registerLeaveGuard={value => { guard = value; }} onBack={() => guard?.(onLeave)} returnLabel="Back to Chat" />);
    await user.type(await screen.findByLabelText("Base URL"), "/changed");
    await user.click(screen.getByRole("button", { name: "Save configuration" }));
    await user.click(screen.getByRole("button", { name: "Back to Chat" }));
    expect(onLeave).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(port.commits).toHaveLength(1);
    gate.resolve();
    await waitFor(() => expect(onLeave).toHaveBeenCalledOnce());
    expect(port.commits).toHaveLength(1);
    expect(port.snapshotCalls).toBeGreaterThan(1);
  });

  it("picks an MCP executable and does not expose a working-directory field", async () => {
    const port = new RecordingSettingsV2Port();
    const picker = presentation({
      pickFile: async () => "C:\\Program Files\\MCP Server\\server.exe",
    });
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={picker} />);

    await screen.findByLabelText("Base URL");
    await user.click(screen.getByRole("button", { name: /MCP servers/ }));
    await user.click(screen.getByRole("button", { name: "Add server" }));
    expect(screen.queryByLabelText("Working directory")).toBeNull();

    await user.click(screen.getByRole("button", { name: /Browse/ }));
    expect(picker.pickFile).toHaveBeenCalledTimes(1);
    expect(screen.getByLabelText("Command")).toHaveValue(
      "C:\\Program Files\\MCP Server\\server.exe",
    );
  });

  it("edits and atomically saves the complete configuration across ten real sections", async () => {
    const port = new RecordingSettingsV2Port();
    const user = userEvent.setup();
    const { container } = render(
      <SettingsScreen settingsPort={port} presentation={presentation()} />,
    );

    const baseUrl = await screen.findByLabelText("Base URL");
    await user.clear(baseUrl);
    await user.type(baseUrl, "https://changed.example/v1");

    const navigation = screen.getByRole("navigation", {
      name: "Settings sections",
    });
    expect(within(navigation).getAllByRole("button")).toHaveLength(11);
    await user.click(within(navigation).getByRole("button", { name: /Approvals/ }));
    await user.selectOptions(screen.getByLabelText("Default approval mode"), "approve_for_me");
    expect(screen.queryByText(/unsupported in this build/i)).toBeNull();
    const save = screen.getByRole("button", { name: "Save configuration" });
    expect(save).toBeEnabled();
    await user.click(save);

    await waitFor(() => expect(port.commits).toHaveLength(1));
    expect(port.commits[0]).toMatchObject({
      expectedVersion: 1,
      settings: {
        approvals: { defaultMode: "approve_for_me" },
        schemaVersion: 2,
        providers: [
          expect.objectContaining({ baseUrl: "https://changed.example/v1" }),
        ],
      },
    });
    await waitFor(() => expect(save).toBeDisabled());
    expect(screen.getByText(/Version 2 · saved/)).toBeVisible();
    for (const field of container.querySelectorAll("input, select, textarea"))
      expect(field.getAttribute("title"), field.outerHTML).toBeTruthy();
  });

  it("shows provider runtime defaults and saves explicit timeout and tool-output limits", async () => {
    const port = new RecordingSettingsV2Port();
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    const timeout = await screen.findByLabelText("Request timeout (seconds)");
    const toolOutput = screen.getByLabelText("Maximum tool output (bytes)");
    expect(timeout).toHaveValue(300);
    expect(toolOutput).toHaveValue(65_536);

    fireEvent.change(timeout, { target: { value: "180" } });
    fireEvent.change(toolOutput, { target: { value: "32768" } });
    await user.click(screen.getByRole("button", { name: "Save configuration" }));

    await waitFor(() => expect(port.commits).toHaveLength(1));
    expect(port.commits[0]?.settings.providers[0]?.configuration).toEqual({
      requestTimeoutSeconds: 180,
      maximumToolOutputBytes: 32_768,
    });
  });

  it("keeps legacy provider metadata recoverable until the user clears it", async () => {
    const settings = configuration();
    settings.providers[0]!.configuration = { apiStyle: "responses" };
    const port = new RecordingSettingsV2Port(settings);
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    const unsupported = await screen.findByLabelText(
      "Unsupported provider configuration",
    );
    expect(unsupported).toHaveValue('{\n  "apiStyle": "responses"\n}');

    fireEvent.change(unsupported, { target: { value: "{}" } });
    await user.click(screen.getByRole("button", { name: "Save configuration" }));

    await waitFor(() => expect(port.commits).toHaveLength(1));
    expect(port.commits[0]?.settings.providers[0]?.configuration).toEqual({});
  });

  it("fails closed when an accepted Save cannot recover its receipt version", async () => {
    const port = new RecordingSettingsV2Port();
    port.mutationSnapshotVersionOffset = -1;
    port.mutationSnapshotFaultCount = 2;
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    const providerName = await screen.findByLabelText("Provider name");
    await user.type(providerName, " pending proof");
    await user.click(screen.getByRole("button", { name: "Save configuration" }));

    expect(
      await screen.findByText(/canonical Settings snapshot is stale/),
    ).toBeVisible();
    expect(screen.getByText(/Version 1 · 1 unsaved section/)).toBeVisible();
    expect(providerName).toHaveValue("Local provider pending proof");
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Save configuration" }),
      ).toBeEnabled(),
    );
  });

  it.each([
    {
      label: "the wrong command ID",
      commandIdOverride: "settings.unrelated-receipt",
      versionOffset: 0,
      message: /exact command ID/,
    },
    {
      label: "a too-low version",
      commandIdOverride: null,
      versionOffset: -1,
      message: /exact expected version transition/,
    },
  ])(
    "fails closed and preserves the Save retry when an accepted receipt has $label",
    async ({ commandIdOverride, versionOffset, message }) => {
      const port = new RecordingSettingsV2Port();
      port.mutationReceiptCommandIdOverride = commandIdOverride;
      port.mutationReceiptVersionOffset = versionOffset;
      const user = userEvent.setup();
      render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

      const providerName = await screen.findByLabelText("Provider name");
      await user.type(providerName, " pending exact proof");
      const save = screen.getByRole("button", { name: "Save configuration" });
      await user.click(save);

      expect(await screen.findByText(message)).toBeVisible();
      expect(screen.getByText(/Version 1 · 1 unsaved section/)).toBeVisible();
      expect(providerName).toHaveValue("Local provider pending exact proof");
      await waitFor(() => expect(save).toBeEnabled());
      const firstCommandId = port.commits[0]?.commandId;

      await user.click(save);
      await waitFor(() => expect(port.commits).toHaveLength(2));
      expect(port.commits[1]?.commandId).toBe(firstCommandId);
    },
  );

  it("tests and discovers models against the exact current provider draft", async () => {
    const port = new RecordingSettingsV2Port();
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    await screen.findByLabelText("Base URL");
    await user.click(screen.getByRole("button", { name: "Test" }));
    await waitFor(() => expect(port.probes).toHaveLength(1));
    expect(port.probes[0]).toMatchObject({
      provider: { id: "provider.local" },
      modelId: "model.chat",
      useStoredCredential: false,
    });
    expect(await screen.findByText("Connection succeeded.")).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Discover models" }));
    await waitFor(() => expect(port.discoveries).toHaveLength(1));
    expect(port.discoveries[0]?.provider.baseUrl).toBe(
      "http://127.0.0.1:11434/v1",
    );
    expect(await screen.findByDisplayValue("remote-model-2")).toBeVisible();
  });

  it("does not let a delayed provider diagnostic snapshot downgrade an accepted Save", async () => {
    const diagnosticSnapshot = deferred();
    const port = new RecordingSettingsV2Port();
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    const providerName = await screen.findByLabelText("Provider name");
    await user.type(providerName, " saved");
    port.snapshotGateOnce = diagnosticSnapshot.promise;
    const snapshotCallsBeforeTest = port.snapshotCalls;
    await user.click(screen.getByRole("button", { name: "Test" }));
    await waitFor(() =>
      expect(port.snapshotCalls).toBe(snapshotCallsBeforeTest + 1),
    );

    await user.click(screen.getByRole("button", { name: "Save configuration" }));
    await waitFor(() => expect(port.commits).toHaveLength(1));
    expect(await screen.findByText(/Version 2 · saved/)).toBeVisible();
    expect(providerName).toHaveValue("Local provider saved");

    diagnosticSnapshot.resolve();
    await waitFor(() => expect(port.snapshotGateCompletions).toBe(1));
    expect(screen.getByText(/Version 2 · saved/)).toBeVisible();
    expect(providerName).toHaveValue("Local provider saved");
  });

  it("preserves an unrelated provider draft while model discovery is in flight", async () => {
    const discoveryGate = deferred();
    const port = new RecordingSettingsV2Port();
    port.modelDiscoveryGate = discoveryGate.promise;
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    await screen.findByLabelText("Base URL");
    await user.click(screen.getByRole("button", { name: "Discover models" }));
    await waitFor(() => expect(port.discoveries).toHaveLength(1));

    await user.click(screen.getByRole("button", { name: "Add" }));
    await user.type(screen.getByLabelText("Provider name"), " unrelated edit");
    discoveryGate.resolve();
    await waitFor(() => expect(port.modelDiscoveryCompletions).toBe(1));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Discover models" }),
      ).toBeEnabled(),
    );

    await user.click(screen.getByRole("button", { name: "Save configuration" }));
    await waitFor(() => expect(port.commits).toHaveLength(1));
    const committedProviders = port.commits[0]!.settings.providers;
    expect(committedProviders).toHaveLength(2);
    expect(
      committedProviders
        .find(({ id }) => id === "provider.local")
        ?.models.some(({ remoteId }) => remoteId === "remote-model-2"),
    ).toBe(true);
    expect(
      committedProviders.find(({ id }) => id !== "provider.local")?.name,
    ).toContain("unrelated edit");
  });

  it("ignores provider discovery that completes after an accepted Discard", async () => {
    const discoveryGate = deferred();
    const port = new RecordingSettingsV2Port();
    port.modelDiscoveryGate = discoveryGate.promise;
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    await screen.findByLabelText("Base URL");
    await user.click(screen.getByRole("button", { name: "Discover models" }));
    await waitFor(() => expect(port.discoveries).toHaveLength(1));
    await user.click(screen.getByRole("button", { name: /Appearance/ }));
    await user.click(screen.getByRole("radio", { name: /Dark/ }));
    await user.click(screen.getByRole("button", { name: "Discard" }));
    await waitFor(() =>
      expect(screen.getByRole("radio", { name: /System/ })).toBeChecked(),
    );

    discoveryGate.resolve();
    await waitFor(() => expect(port.modelDiscoveryCompletions).toBe(1));
    await user.click(
      screen.getByRole("button", { name: /Providers & models/ }),
    );
    expect(screen.queryByDisplayValue("remote-model-2")).toBeNull();
    expect(screen.getByText(/Version 1 · saved/)).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Save configuration" }),
    ).toBeDisabled();
  });

  it("reconciles deferred project add and remove against the latest draft", async () => {
    const initial = configuration();
    initial.projects = [
      projectConfiguration("project.a", "Project A", "local_directory"),
      projectConfiguration("project.b", "Project B", "local_directory"),
    ];
    const folder = deferredValue<string | null>();
    const confirmation = deferredValue<boolean>();
    const port = new RecordingSettingsV2Port(initial);
    const user = userEvent.setup();
    render(
      <SettingsScreen
        settingsPort={port}
        presentation={presentation({
          pickFolder: () => folder.promise,
          confirm: () => confirmation.promise,
        })}
      />,
    );

    await screen.findByLabelText("Base URL");
    await user.click(screen.getByRole("button", { name: /Projects/ }));
    await user.click(screen.getByRole("button", { name: "Add folder…" }));
    const projectB = screen
      .getByRole("heading", { name: "Project B" })
      .closest("section");
    expect(projectB).not.toBeNull();
    await user.type(
      within(projectB!).getByLabelText("Project name"),
      " edited",
    );
    folder.resolve("/tmp/Added Project");
    expect(
      await screen.findByRole("heading", { name: "Added Project" }),
    ).toBeVisible();
    expect(
      screen.getByRole("heading", { name: "Project B edited" }),
    ).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Remove Project A" }));
    const addedProject = screen
      .getByRole("heading", { name: "Added Project" })
      .closest("section");
    expect(addedProject).not.toBeNull();
    await user.type(
      within(addedProject!).getByLabelText("Project name"),
      " edited",
    );
    confirmation.resolve(true);
    await waitFor(() =>
      expect(
        screen.queryByRole("heading", { name: "Project A" }),
      ).toBeNull(),
    );
    expect(
      screen.getByRole("heading", { name: "Added Project edited" }),
    ).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Save configuration" }));
    await waitFor(() => expect(port.commits).toHaveLength(1));
    expect(
      port.commits[0]?.settings.projects.map(({ name }) => name),
    ).toEqual(["Project B edited", "Added Project edited"]);
  });

  it(
    "invalidates every diagnostic when its exact Settings draft changes",
    async () => {
      const port = new RecordingSettingsV2Port();
      const user = userEvent.setup();
      render(
        <SettingsScreen
          settingsPort={port}
          presentation={presentation({
            pickFolder: async () => "/tmp/Exact Project",
          })}
        />,
      );

      const baseUrl = await screen.findByLabelText("Base URL");
      await user.click(screen.getByRole("button", { name: "Test" }));
      expect(await screen.findByText("Connection succeeded.")).toBeVisible();
      await user.type(baseUrl, "/changed");
      expect(screen.queryByText("Connection succeeded.")).toBeNull();

      await user.click(screen.getByRole("button", { name: /MCP servers/ }));
      await user.click(screen.getByRole("button", { name: "Add server" }));
      await user.type(screen.getByLabelText("Command"), "fixture-mcp");
      await user.click(screen.getByRole("button", { name: "Discover and test" }));
      expect(
        await screen.findByText(/Connected using MCP 2026-07-28/),
      ).toBeVisible();
      await user.type(screen.getByLabelText("Server name"), " changed");
      expect(screen.queryByText(/Connected using MCP 2026-07-28/)).toBeNull();

      await user.click(screen.getByRole("button", { name: /External agents/ }));
      await user.click(screen.getByRole("button", { name: "Add agent" }));
      const agentCard = screen
        .getByRole("heading", { name: "External agent" })
        .closest("section");
      expect(agentCard).not.toBeNull();
      expect(
        within(agentCard!).queryByRole("option", { name: "Streamable HTTP" }),
      ).toBeNull();
      await user.click(screen.getByRole("button", { name: "Start handshake" }));
      expect(
        await screen.findByText(/Codex App Server handshake completed/),
      ).toBeVisible();
      expect(within(agentCard!).getByText("progress: yes")).toBeVisible();
      await user.type(within(agentCard!).getByLabelText("Command"), "-changed");
      expect(
        screen.queryByText(/Codex App Server handshake completed/),
      ).toBeNull();
      expect(
        within(agentCard!).queryByLabelText("Capabilities from current handshake"),
      ).toBeNull();
      expect(
        within(agentCard!).getByText(/No capabilities have been reported/),
      ).toBeVisible();

      await user.click(screen.getByRole("button", { name: /Projects/ }));
      await user.click(screen.getByRole("button", { name: "Add folder…" }));
      await user.click(screen.getByRole("button", { name: "Test workspace" }));
      expect(await screen.findByText(/Workspace resolved/)).toBeVisible();
      await user.type(screen.getByLabelText("Project name"), " changed");
      expect(screen.queryByText(/Workspace resolved/)).toBeNull();

      await user.click(screen.getByRole("button", { name: /Tools/ }));
      await user.click(screen.getByRole("button", { name: "Probe adapter only" }));
      expect(await screen.findByText(/Tool adapter ready/)).toBeVisible();
      await user.click(
        screen.getByRole("checkbox", { name: "Available to workflows" }),
      );
      expect(screen.queryByText(/Tool adapter ready/)).toBeNull();
    },
    15_000,
  );

  it(
    "navigates to validation issues and preserves a local draft across a version conflict",
    async () => {
      const port = new RecordingSettingsV2Port();
      const native = presentation({
        pickFolder: async () => "/tmp/Project Atlas",
      });
      const user = userEvent.setup();
      render(<SettingsScreen settingsPort={port} presentation={native} />);

    await screen.findByLabelText("Base URL");
    await user.click(screen.getByRole("button", { name: /Projects/ }));
    await user.click(screen.getByRole("button", { name: "Add folder…" }));
    const projectName = screen.getByLabelText("Project name");
    await user.clear(projectName);
    await user.click(screen.getByRole("button", { name: /Providers & models/ }));

    const summary = screen.getByLabelText(/validation/i, {
      selector: "section",
    });
    const projectIssue = within(summary).getByRole("button", {
      name: /Projects/i,
    });
    expect(screen.getByRole("button", { name: "Save configuration" })).toBeDisabled();
    await user.click(projectIssue);
    await waitFor(() => expect(screen.getByLabelText("Project name")).toHaveFocus());

    await user.type(screen.getByLabelText("Project name"), "Project Atlas");
    await user.click(screen.getByRole("button", { name: /Providers & models/ }));
    const baseUrl = screen.getByLabelText("Base URL");
    await user.clear(baseUrl);
    await user.type(baseUrl, "https://local-draft.example/v1");
    port.conflictOnce = true;
    await user.click(screen.getByRole("button", { name: "Save configuration" }));

    expect(
      await screen.findByText(/newer canonical version was loaded/i),
    ).toBeVisible();
    expect(screen.getByLabelText("Base URL")).toHaveValue(
      "https://local-draft.example/v1",
    );
    expect(screen.getByText(/Version 2 ·/)).toBeVisible();
      expect(
        screen.getByRole("button", { name: "Save configuration" }),
      ).toBeEnabled();
    },
    10_000,
  );

  it("discards appearance previews only after native confirmation", async () => {
    const port = new RecordingSettingsV2Port();
    const native = presentation();
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={native} />);

    await screen.findByLabelText("Base URL");
    const timeout = screen.getByLabelText("Request timeout (seconds)");
    fireEvent.change(timeout, { target: { value: "120" } });
    await user.click(screen.getByRole("button", { name: /Appearance/ }));
    await user.click(screen.getByRole("radio", { name: /Dark/ }));
    expect(document.documentElement.dataset.appearance).toBe("dark");
    await user.click(screen.getByRole("button", { name: "Discard" }));

    expect(native.confirm).toHaveBeenCalledWith(
      "Discard unsaved settings?",
      expect.stringContaining("latest canonical version"),
    );
    expect(screen.getByRole("radio", { name: /System/ })).toBeChecked();
    expect(document.documentElement.dataset.appearance).not.toBe("dark");
    await user.click(screen.getByRole("button", { name: /Providers & models/ }));
    expect(screen.getByLabelText("Request timeout (seconds)")).toHaveValue(300);
  });

  it("stores and deletes write-only credentials through dedicated versioned commands", async () => {
    const port = new RecordingSettingsV2Port();
    const native = presentation();
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={native} />);

    await screen.findByLabelText("Base URL");
    await user.click(screen.getByRole("button", { name: /Credentials/ }));
    await user.click(screen.getByRole("button", { name: "Add credential" }));
    await user.type(screen.getByLabelText("Label"), "Hosted API");
    await user.type(screen.getByLabelText("New secret value"), "write-only-value");
    await user.click(screen.getByRole("button", { name: "Store credential" }));

    await waitFor(() => expect(port.credentialStores).toHaveLength(1));
    expect(port.credentialStores[0]).toMatchObject({
      expectedVersion: 1,
      label: "Hosted API",
      fields: { api_key: "write-only-value" },
    });
    expect(
      await screen.findByRole("heading", { name: "Hosted API" }),
    ).toBeVisible();
    expect(screen.queryByDisplayValue("write-only-value")).toBeNull();

    await user.click(screen.getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(port.credentialDeletes).toHaveLength(1));
    expect(port.credentialDeletes[0]).toMatchObject({
      expectedVersion: 2,
      credentialRef: "credential.recorded",
    });
    await waitFor(() =>
      expect(screen.queryByRole("heading", { name: "Hosted API" })).toBeNull(),
    );
  });

  it("keeps external-agent capabilities as exact ephemeral probe evidence", async () => {
    const initial = configuration();
    initial.externalAgents = [externalAgentConfiguration({ progress: true })];
    const port = new RecordingSettingsV2Port(initial);
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    await screen.findByLabelText("Base URL");
    const navigation = screen.getByRole("navigation", {
      name: "Settings sections",
    });
    await user.click(
      within(navigation).getByRole("button", { name: /External agents/ }),
    );
    expect(screen.queryByText("progress: yes")).toBeNull();
    expect(screen.getByText("progress: saved true (ignored)")).toBeVisible();

    await user.click(
      screen.getByRole("button", {
        name: "Clear unsupported capability metadata",
      }),
    );
    expect(screen.queryByText("progress: saved true (ignored)")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Start handshake" }));
    expect(await screen.findByText("progress: yes")).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Save configuration" }));
    await waitFor(() => expect(port.commits).toHaveLength(1));
    expect(port.commits[0]?.settings.externalAgents[0]?.capabilities).toEqual({
      progress: false,
      continuation: false,
      cancellation: false,
      approvals: false,
    });
    await waitFor(() => expect(screen.queryByText("progress: yes")).toBeNull());
  });

  it(
    "invalidates provider, MCP, and external-agent diagnostics after credential mutations",
    async () => {
      const port = new RecordingSettingsV2Port();
      const user = userEvent.setup();
      render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

      await screen.findByLabelText("Base URL");
      const navigation = screen.getByRole("navigation", {
        name: "Settings sections",
      });
      await user.click(screen.getByRole("button", { name: "Test" }));
      expect(await screen.findByText("Connection succeeded.")).toBeVisible();

      await user.click(
        within(navigation).getByRole("button", { name: /MCP servers/ }),
      );
      await user.click(screen.getByRole("button", { name: "Add server" }));
      await user.type(screen.getByLabelText("Command"), "fixture-mcp");
      await user.click(screen.getByRole("button", { name: "Discover and test" }));
      expect(await screen.findByText(/Connected using MCP 2026-07-28/)).toBeVisible();

      await user.click(
        within(navigation).getByRole("button", { name: /External agents/ }),
      );
      await user.click(screen.getByRole("button", { name: "Add agent" }));
      await user.click(screen.getByRole("button", { name: "Start handshake" }));
      expect(
        await screen.findByText(/Codex App Server handshake completed/),
      ).toBeVisible();

      await user.click(
        within(navigation).getByRole("button", { name: /Credentials/ }),
      );
      await user.click(screen.getByRole("button", { name: "Add credential" }));
      await user.type(screen.getByLabelText("Label"), "Ephemeral invalidator");
      await user.type(screen.getByLabelText("New secret value"), "secret-value");
      await user.click(screen.getByRole("button", { name: "Store credential" }));
      await screen.findByRole("heading", { name: "Ephemeral invalidator" });

      await user.click(
        within(navigation).getByRole("button", { name: /Providers & models/ }),
      );
      expect(screen.queryByText("Connection succeeded.")).toBeNull();
      await user.click(
        within(navigation).getByRole("button", { name: /MCP servers/ }),
      );
      expect(screen.queryByText(/Connected using MCP 2026-07-28/)).toBeNull();
      await user.click(
        within(navigation).getByRole("button", { name: /External agents/ }),
      );
      expect(
        screen.queryByText(/Codex App Server handshake completed/),
      ).toBeNull();

      await user.click(
        within(navigation).getByRole("button", { name: /Providers & models/ }),
      );
      await user.click(screen.getByRole("button", { name: "Test" }));
      await screen.findByText("Connection succeeded.");
      await user.click(
        within(navigation).getByRole("button", { name: /MCP servers/ }),
      );
      await user.click(screen.getByRole("button", { name: "Discover and test" }));
      await screen.findByText(/Connected using MCP 2026-07-28/);
      await user.click(
        within(navigation).getByRole("button", { name: /External agents/ }),
      );
      await user.click(screen.getByRole("button", { name: "Start handshake" }));
      await screen.findByText(/Codex App Server handshake completed/);

      await user.click(
        within(navigation).getByRole("button", { name: /Credentials/ }),
      );
      await user.click(screen.getByRole("button", { name: "Delete" }));
      await waitFor(() => expect(port.credentialDeletes).toHaveLength(1));
      await user.click(
        within(navigation).getByRole("button", { name: /Providers & models/ }),
      );
      expect(screen.queryByText("Connection succeeded.")).toBeNull();
      await user.click(
        within(navigation).getByRole("button", { name: /MCP servers/ }),
      );
      expect(screen.queryByText(/Connected using MCP 2026-07-28/)).toBeNull();
      await user.click(
        within(navigation).getByRole("button", { name: /External agents/ }),
      );
      expect(
        screen.queryByText(/Codex App Server handshake completed/),
      ).toBeNull();
    },
    20_000,
  );

  it(
    "replays an exact lost-response credential replacement and rewires every dirty consumer",
    async () => {
      const oldRef = "credential.shared-old";
      const initial = configurationWithSharedCredential(oldRef);
      initial.externalAgents[0]!.capabilities.progress = true;
      const port = new RecordingSettingsV2Port(initial);
      port.credentialLostResponseOnce = true;
      const user = userEvent.setup();
      render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

      const providerName = await screen.findByLabelText("Provider name");
      await user.type(providerName, " dirty");
      const navigation = screen.getByRole("navigation", {
        name: "Settings sections",
      });
      await user.click(
        within(navigation).getByRole("button", { name: /MCP servers/ }),
      );
      await user.type(screen.getByLabelText("Server name"), " dirty");
      await user.click(
        within(navigation).getByRole("button", { name: /External agents/ }),
      );
      await user.type(screen.getByLabelText("Agent name"), " dirty");

      await user.click(
        within(navigation).getByRole("button", { name: /Credentials/ }),
      );
      await user.click(screen.getByRole("button", { name: "Replace…" }));
      await user.type(screen.getByLabelText("New secret value"), "replacement-secret");
      await user.click(
        screen.getByRole("button", { name: "Replace credential" }),
      );
      await waitFor(() => expect(port.credentialStores).toHaveLength(2));
      expect(port.credentialStores[0]?.replaceCredentialRef).toBe(oldRef);
      expect(port.credentialStores[1]?.commandId).toBe(
        port.credentialStores[0]?.commandId,
      );
      expect(port.credentialStores[1]).toEqual(port.credentialStores[0]);
      await waitFor(() =>
        expect(
          screen.queryByRole("heading", { name: "Replace Shared API key" }),
        ).toBeNull(),
      );

      const save = screen.getByRole("button", { name: "Save configuration" });
      await waitFor(() => expect(save).toBeEnabled());
      expect(screen.queryByLabelText(/validation/i, { selector: "section" })).toBeNull();
      await user.click(save);
      await waitFor(() => expect(port.commits).toHaveLength(1));

      const committed = port.commits[0]!.settings;
      const replacementRef = committed.credentials.find(
        ({ credentialRef }) => credentialRef !== oldRef,
      )!.credentialRef;
      expect(committed.providers[0]).toMatchObject({
        name: "Local provider dirty",
        credentialRef: replacementRef,
      });
      expect(committed.mcpServers[0]).toMatchObject({
        name: "Fixture MCP dirty",
        transport: {
          env: [expect.objectContaining({ credentialRef: replacementRef })],
        },
      });
      expect(committed.externalAgents[0]).toMatchObject({
        name: "Fixture external agent dirty",
        connection: {
          env: [expect.objectContaining({ credentialRef: replacementRef })],
        },
        credentialBindings: [
          expect.objectContaining({ credentialRef: replacementRef }),
        ],
        capabilities: {
          progress: false,
          continuation: false,
          cancellation: false,
          approvals: false,
        },
      });
      expect(JSON.stringify(committed)).not.toContain(oldRef);
    },
    15_000,
  );

  it("resets legacy capabilities when dirty agents removed old canonical bindings", async () => {
    const oldRef = "credential.shared-old";
    const initial = configurationWithSharedCredential(oldRef);
    const sharedAgent = initial.externalAgents[0]!;
    if (sharedAgent.connection.transport !== "stdio")
      throw new Error("Expected the fixture external agent to use stdio.");
    const legacyCapabilities = {
      progress: true,
      continuation: true,
      cancellation: true,
      approvals: true,
    };
    initial.externalAgents = [
      {
        ...sharedAgent,
        id: "agent.connection-binding",
        name: "Connection binding agent",
        credentialBindings: [],
        capabilities: { ...legacyCapabilities },
      },
      {
        ...sharedAgent,
        id: "agent.direct-binding",
        name: "Direct binding agent",
        connection: { ...sharedAgent.connection, env: [] },
        capabilities: { ...legacyCapabilities },
      },
    ];
    const port = new RecordingSettingsV2Port(initial);
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    await screen.findByLabelText("Provider name");
    const navigation = screen.getByRole("navigation", {
      name: "Settings sections",
    });
    await user.click(
      within(navigation).getByRole("button", { name: /External agents/ }),
    );
    await user.click(
      screen.getByRole("button", { name: "Remove CODEX_API_KEY" }),
    );
    await user.click(
      screen.getByRole("button", { name: "Remove AWORKIT_API_KEY" }),
    );

    await user.click(
      within(navigation).getByRole("button", { name: /Credentials/ }),
    );
    await user.click(screen.getByRole("button", { name: "Replace…" }));
    await user.type(
      screen.getByLabelText("New secret value"),
      "replacement-secret",
    );
    await user.click(screen.getByRole("button", { name: "Replace credential" }));
    await waitFor(() =>
      expect(
        screen.queryByRole("heading", { name: "Replace Shared API key" }),
      ).toBeNull(),
    );

    const save = screen.getByRole("button", { name: "Save configuration" });
    expect(save).toBeEnabled();
    await user.click(save);
    await waitFor(() => expect(port.commits).toHaveLength(1));
    expect(port.commits[0]?.settings.externalAgents).toEqual([
      expect.objectContaining({
        id: "agent.connection-binding",
        connection: expect.objectContaining({ env: [] }),
        credentialBindings: [],
        capabilities: {
          progress: false,
          continuation: false,
          cancellation: false,
          approvals: false,
        },
      }),
      expect.objectContaining({
        id: "agent.direct-binding",
        connection: expect.objectContaining({ env: [] }),
        credentialBindings: [],
        capabilities: {
          progress: false,
          continuation: false,
          cancellation: false,
          approvals: false,
        },
      }),
    ]);
  });

  it("does not rewire dirty refs after a concurrent unrelated credential conflict", async () => {
    const oldRef = "credential.shared-old";
    const initial = configurationWithSharedCredential(oldRef);
    const port = new RecordingSettingsV2Port(initial);
    port.credentialConflictOnce = true;
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    await user.type(await screen.findByLabelText("Provider name"), " dirty");
    const navigation = screen.getByRole("navigation", {
      name: "Settings sections",
    });
    await user.click(
      within(navigation).getByRole("button", { name: /Credentials/ }),
    );
    await user.click(screen.getByRole("button", { name: "Replace…" }));
    await user.type(screen.getByLabelText("New secret value"), "replacement-secret");
    await user.click(screen.getByRole("button", { name: "Replace credential" }));

    await waitFor(() => expect(port.credentialStores).toHaveLength(2));
    expect(port.credentialStores[1]).toEqual(port.credentialStores[0]);
    expect(
      await screen.findByText(/settings version conflict/, {
        selector: ".field-error",
      }),
    ).toBeVisible();
    const save = screen.getByRole("button", { name: "Save configuration" });
    await waitFor(() => expect(save).toBeEnabled());
    await user.click(save);
    await waitFor(() => expect(port.commits).toHaveLength(1));

    const committed = port.commits[0]!.settings;
    expect(committed.providers[0]).toMatchObject({
      name: "Local provider dirty",
      credentialRef: oldRef,
    });
    expect(JSON.stringify(committed.mcpServers)).toContain(oldRef);
    expect(JSON.stringify(committed.externalAgents)).toContain(oldRef);
    expect(JSON.stringify(committed)).not.toContain("credential.replacement");
    expect(
      committed.credentials.some(
        ({ credentialRef }) =>
          credentialRef === "credential.concurrent-unrelated",
      ),
    ).toBe(true);
  });

  it("refuses to rewire from a mismatched credential receipt", async () => {
    const oldRef = "credential.shared-old";
    const port = new RecordingSettingsV2Port(
      configurationWithSharedCredential(oldRef),
    );
    port.credentialMismatchedReceiptOnce = true;
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    await user.type(await screen.findByLabelText("Provider name"), " dirty");
    const navigation = screen.getByRole("navigation", {
      name: "Settings sections",
    });
    await user.click(
      within(navigation).getByRole("button", { name: /Credentials/ }),
    );
    await user.click(screen.getByRole("button", { name: "Replace…" }));
    await user.type(screen.getByLabelText("New secret value"), "replacement-secret");
    await user.click(screen.getByRole("button", { name: "Replace credential" }));

    expect(
      await screen.findByText(/receipt did not match the exact command ID/, {
        selector: ".field-error",
      }),
    ).toBeVisible();
    expect(port.credentialStores).toHaveLength(1);
    expect(
      screen.getByRole("button", { name: "Save configuration" }),
    ).toBeDisabled();
    expect(screen.getByLabelText(/validation/i, { selector: "section" })).toBeVisible();
    expect(port.commits).toHaveLength(0);
  });

  it("ignores an in-flight provider probe snapshot after credential mutation starts", async () => {
    const oldRef = "credential.shared-old";
    const port = new RecordingSettingsV2Port(
      configurationWithSharedCredential(oldRef),
    );
    const probeGate = deferred();
    port.providerProbeGate = probeGate.promise;
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    await screen.findByLabelText("Provider name");
    await user.click(screen.getByRole("button", { name: "Test" }));
    await waitFor(() => expect(port.probes).toHaveLength(1));
    const navigation = screen.getByRole("navigation", {
      name: "Settings sections",
    });
    await user.click(
      within(navigation).getByRole("button", { name: /Credentials/ }),
    );
    await user.click(screen.getByRole("button", { name: "Replace…" }));
    await user.type(screen.getByLabelText("New secret value"), "replacement-secret");
    await user.click(screen.getByRole("button", { name: "Replace credential" }));
    await waitFor(() => expect(port.credentialStores).toHaveLength(1));
    const snapshotCallsAfterMutation = port.snapshotCalls;

    probeGate.resolve();
    await waitFor(() => expect(port.providerProbeCompletions).toBe(1));
    await waitFor(() =>
      expect(port.snapshotCalls).toBe(snapshotCallsAfterMutation),
    );
    expect(screen.queryByText("Connection succeeded.")).toBeNull();
  });

  it("locks Settings edits while credential deletion is in flight", async () => {
    const credentialRef = "credential.pending-delete";
    const initial = configuration();
    initial.credentials = [credentialMetadata(credentialRef)];
    const port = new RecordingSettingsV2Port(initial);
    const deleteGate = deferred();
    port.credentialDeleteGate = deleteGate.promise;
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    await screen.findByLabelText("Base URL");
    const navigation = screen.getByRole("navigation", {
      name: "Settings sections",
    });
    await user.click(
      within(navigation).getByRole("button", { name: /Credentials/ }),
    );
    await user.click(screen.getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(port.credentialDeletes).toHaveLength(1));

    await user.click(
      within(navigation).getByRole("button", { name: /Providers & models/ }),
    );
    const providerCredential = screen.getByLabelText("Credential");
    expect(providerCredential).toBeDisabled();
    fireEvent.change(providerCredential, { target: { value: credentialRef } });
    expect(providerCredential).toHaveValue("");

    deleteGate.resolve();
    await waitFor(() => expect(providerCredential).toBeEnabled());
    expect((await port.snapshot()).settings.providers[0]?.credentialRef).toBeNull();
  });

  it("locks Settings edits while an ordinary canonical Save is in flight", async () => {
    const port = new RecordingSettingsV2Port();
    const commitGate = deferred();
    port.commitGate = commitGate.promise;
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    const providerName = await screen.findByLabelText("Provider name");
    await user.type(providerName, " saved");
    await user.click(screen.getByRole("button", { name: "Save configuration" }));
    await waitFor(() => expect(port.commits).toHaveLength(1));

    expect(providerName).toBeDisabled();
    fireEvent.change(providerName, { target: { value: "must not overwrite" } });
    expect(providerName).toHaveValue("Local provider saved");

    commitGate.resolve();
    await waitFor(() => expect(providerName).toBeEnabled());
    expect((await port.snapshot()).settings.providers[0]?.name).toBe(
      "Local provider saved",
    );
  });

  it("replays an exact lost-response credential deletion and closes normally", async () => {
    const credentialRef = "credential.delete-lost-response";
    const initial = configuration();
    initial.credentials = [credentialMetadata(credentialRef)];
    const port = new RecordingSettingsV2Port(initial);
    port.credentialDeleteLostResponseOnce = true;
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    await screen.findByLabelText("Base URL");
    const navigation = screen.getByRole("navigation", {
      name: "Settings sections",
    });
    await user.click(
      within(navigation).getByRole("button", { name: /Credentials/ }),
    );
    await user.click(screen.getByRole("button", { name: "Delete" }));

    await waitFor(() => expect(port.credentialDeletes).toHaveLength(2));
    expect(port.credentialDeletes[1]).toEqual(port.credentialDeletes[0]);
    await waitFor(() =>
      expect(
        screen.queryByRole("heading", { name: "Shared API key" }),
      ).toBeNull(),
    );
    expect(screen.queryByText(/deletion response was lost/)).toBeNull();
  });

  it("clears the credential editor after an accepted replacement snapshot is recovered", async () => {
    const oldRef = "credential.shared-old";
    const port = new RecordingSettingsV2Port(
      configurationWithSharedCredential(oldRef),
    );
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    await user.type(await screen.findByLabelText("Provider name"), " dirty");
    const navigation = screen.getByRole("navigation", {
      name: "Settings sections",
    });
    await user.click(
      within(navigation).getByRole("button", { name: /Credentials/ }),
    );
    await user.click(screen.getByRole("button", { name: "Replace…" }));
    await user.type(screen.getByLabelText("New secret value"), "replacement-secret");
    port.snapshotFailureOnce = true;
    await user.click(screen.getByRole("button", { name: "Replace credential" }));

    await waitFor(() =>
      expect(
        screen.queryByRole("heading", { name: "Replace Shared API key" }),
      ).toBeNull(),
    );
    const save = screen.getByRole("button", { name: "Save configuration" });
    expect(save).toBeEnabled();
    await user.click(save);
    await waitFor(() => expect(port.commits).toHaveLength(1));
    expect(port.commits[0]?.settings.providers[0]?.credentialRef).toBe(
      "credential.replacement",
    );
  });

  it.each([
    ["stale_version", /canonical credential snapshot is stale/],
    [
      "missing_reference",
      /canonical credential snapshot did not contain the exact fresh credential reference/,
    ],
  ] as const)(
    "fails closed when replacement refresh and recovery return %s",
    async (failureMode, expectedError) => {
      const oldRef = "credential.shared-old";
      const port = new RecordingSettingsV2Port(
        configurationWithSharedCredential(oldRef),
      );
      const user = userEvent.setup();
      render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

      await user.type(await screen.findByLabelText("Provider name"), " dirty");
      const navigation = screen.getByRole("navigation", {
        name: "Settings sections",
      });
      await user.click(
        within(navigation).getByRole("button", { name: /Credentials/ }),
      );
      await user.click(screen.getByRole("button", { name: "Replace…" }));
      const secret = screen.getByLabelText("New secret value");
      await user.type(secret, "replacement-secret");
      port.credentialSnapshotPostconditionFailure = failureMode;
      port.credentialSnapshotPostconditionFailuresRemaining = 2;
      await user.click(screen.getByRole("button", { name: "Replace credential" }));

      expect(
        await screen.findByText(expectedError, { selector: ".field-error" }),
      ).toBeVisible();
      expect(
        screen.getByRole("heading", { name: "Replace Shared API key" }),
      ).toBeVisible();
      expect(secret).toHaveValue("replacement-secret");
      await user.click(
        within(navigation).getByRole("button", {
          name: /Providers & models/,
        }),
      );
      expect(
        screen.getByTitle(/Only credentials with an api_key field/),
      ).toHaveValue(oldRef);
    },
  );

  it("blocks deletion when an unsaved draft newly references the credential", async () => {
    const oldRef = "credential.unsaved-reference";
    const initial = configuration();
    initial.credentials = [credentialMetadata(oldRef)];
    const port = new RecordingSettingsV2Port(initial);
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    const credential = await screen.findByLabelText("Credential");
    await user.selectOptions(credential, oldRef);
    const navigation = screen.getByRole("navigation", {
      name: "Settings sections",
    });
    await user.click(
      within(navigation).getByRole("button", { name: /Credentials/ }),
    );
    await user.click(screen.getByRole("button", { name: "Delete" }));

    expect(
      await screen.findByText(
        /Remove this credential from provider Local provider, then Save configuration before deleting it/,
      ),
    ).toBeVisible();
    expect(port.credentialDeletes).toHaveLength(0);
    await user.click(
      within(navigation).getByRole("button", { name: /Providers & models/ }),
    );
    expect(screen.getByLabelText("Credential")).toHaveValue(oldRef);
    expect(
      screen.getByRole("button", { name: "Save configuration" }),
    ).toBeEnabled();
  });

  it("shows exact tool and project failures and omits unsupported remote tool roots", async () => {
    const tool = {
      ...configuration().tools[0]!,
      credentialBindings: [
        {
          name: "legacy_api_key",
          credentialRef: "credential.legacy",
          field: "api_key",
        },
      ],
    };
    const localProject = projectConfiguration("project.local", "Local project", "local_directory");
    const remoteProject = projectConfiguration("project.remote", "Remote project", "remote");
    const user = userEvent.setup();
    const { unmount } = render(
      <ToolsSection
        credentials={[]}
        projects={[localProject, remoteProject]}
        tools={[tool]}
        onChange={() => undefined}
        onProbe={async () => {
          throw new Error("Exact tool probe failed.");
        }}
      />,
    );

    expect(screen.queryByRole("option", { name: "Remote project" })).toBeNull();
    expect(screen.getByText(/Remote prepared workspaces are omitted/)).toBeVisible();
    expect(
      screen.getByText(/This saved draft contains unsupported credential bindings/),
    ).toBeVisible();
    expect(screen.queryByText("bindable in Simple Chat")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Probe adapter only" }));
    expect(await screen.findByText(/Exact tool probe failed/)).toBeVisible();
    unmount();

    const port = new RecordingSettingsV2Port();
    vi.spyOn(port, "probeProject").mockRejectedValue(
      new Error("Exact project probe failed."),
    );
    render(
      <SettingsScreen
        settingsPort={port}
        presentation={presentation({ pickFolder: async () => "/tmp/Project" })}
      />,
    );
    await screen.findByLabelText("Base URL");
    await user.click(screen.getByRole("button", { name: /Projects/ }));
    await user.click(screen.getByRole("button", { name: "Add folder…" }));
    await user.click(screen.getByRole("button", { name: "Test workspace" }));
    expect(await screen.findByText("Exact project probe failed.")).toBeVisible();
    await user.type(screen.getByLabelText("Project name"), " changed");
    expect(screen.queryByText("Exact project probe failed.")).toBeNull();
  });

  it("lists every built-in tool as an enableable workflow plugin", () => {
    const base = configuration().tools[0]!;
    const tools = [
      { ...base, id: "tool.files.read", name: "Project file read", enabled: false },
      { ...base, id: "tool.files.search", name: "Project file search", enabled: false },
      { ...base, id: "tool.files.edit", name: "Project file edit", enabled: false },
      { ...base, id: "tool.shell.host", name: "Host shell", enabled: false, requiresProject: false },
      { ...base, id: "tool.python.host", name: "Host Python", enabled: false, requiresProject: false },
    ];
    render(
      <ToolsSection
        credentials={[]}
        projects={[]}
        tools={tools}
        onChange={() => undefined}
        onProbe={async (tool) => ({
          ok: true,
          toolId: tool.id,
          adapter: "fixture",
          message: "fixture probe",
          draftFingerprint: "fixture",
        })}
      />,
    );

    for (const name of [
      "Project file read",
      "Project file search",
      "Project file edit",
      "Host shell",
      "Host Python",
    ]) {
      const card = screen.getByRole("heading", { name }).closest("section");
      expect(card).not.toBeNull();
      // Every built-in tool is enableable; the workflow decides usage.
      expect(
        within(card!).getByRole("checkbox", {
          name: "Available to workflows",
        }),
      ).toBeEnabled();
    }
    expect(screen.queryByText(/Simple Chat/)).toBeNull();
  });

  it("keeps implemented domains editable, disables inactive policies, and routes MCP probing through the native port", async () => {
    const port = new RecordingSettingsV2Port();
    const native = presentation({
      pickFile: async () => "/tmp/example.aworkit-extension.json",
      pickFolder: async () => "/tmp/Project One",
    });
    const user = userEvent.setup();
    const { container } = render(
      <SettingsScreen settingsPort={port} presentation={native} />,
    );

    await screen.findByLabelText("Base URL");
    await user.click(screen.getByRole("button", { name: /Model tiers/ }));
    await user.selectOptions(screen.getAllByLabelText("Resolution")[0]!, "unconfigured");

    await user.click(screen.getByRole("button", { name: /Tools/ }));
    const readBinding = screen.getByRole("checkbox", {
      name: "Available to workflows",
    });
    expect(readBinding).toBeEnabled();
    await user.click(readBinding);

    await user.click(screen.getByRole("button", { name: /MCP servers/ }));
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await user.type(screen.getByLabelText("Command"), "aworkit-mcp-example");
    expect(
      screen.queryByRole("checkbox", {
        name: "Connect at launch (not available)",
      }),
    ).toBeNull();
    expect(
      screen.queryByRole("checkbox", {
        name: "Workflow execution not available",
      }),
    ).toBeNull();
    expect(screen.queryByText(/Diagnostic only/)).toBeNull();
    await user.click(screen.getByRole("button", { name: "Discover and test" }));
    expect(
      await screen.findByText(/Connected using MCP 2026-07-28/),
    ).toBeVisible();
    expect(port.mcpProbes).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: /External agents/ }));
    await user.click(screen.getByRole("button", { name: "Add agent" }));
    expect(
      screen.getByRole("checkbox", {
        name: "Workflow execution not available",
      }),
    ).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Start handshake" }));
    expect(
      await screen.findByText(/Codex App Server handshake completed/),
    ).toBeVisible();
    expect(port.externalAgentProbes).toHaveLength(1);
    expect(screen.getByText("progress: yes")).toBeVisible();

    await user.click(screen.getByRole("button", { name: /Data & sessions/ }));
    expect(
      screen.getByRole("checkbox", {
        name: "Portable project sessions (not available)",
      }),
    ).toBeDisabled();
    expect(screen.getByLabelText("Local history retention")).toBeDisabled();

    await user.click(screen.getByRole("button", { name: /Projects/ }));
    await user.click(screen.getByRole("button", { name: "Add folder…" }));
    expect(await screen.findByLabelText("Project name")).toHaveValue("Project One");
    await user.click(screen.getByRole("button", { name: "Test workspace" }));
    expect(await screen.findByText(/Workspace resolved/)).toBeVisible();

    await user.click(screen.getByRole("button", { name: /Extensions/ }));
    await user.click(
      screen.getByRole("button", { name: "Discover manifest…" }),
    );
    expect(await screen.findByRole("heading", { name: "extension.fixture" })).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Register installed package" }),
    ).toBeDisabled();

    await user.click(screen.getByRole("button", { name: /Appearance/ }));
    await user.click(screen.getByRole("radio", { name: /Dark/ }));
    for (const field of container.querySelectorAll("input, select, textarea"))
      expect(field.getAttribute("title"), field.outerHTML).toBeTruthy();
    const save = screen.getByRole("button", { name: "Save configuration" });
    expect(save).toBeEnabled();
    await user.click(save);

    await waitFor(() => expect(port.commits).toHaveLength(1));
    expect(port.commits[0]?.settings.modelTiers[0]?.resolution).toEqual({
      strategy: "unconfigured",
    });
    expect(port.commits[0]?.settings).toMatchObject({
      tools: [expect.objectContaining({ enabled: false })],
      mcpServers: [
        expect.objectContaining({
          transport: expect.objectContaining({ command: "aworkit-mcp-example" }),
        }),
      ],
      externalAgents: [expect.objectContaining({ adapter: "codex_app_server" })],
      data: expect.objectContaining({ portableHistoryEnabled: false }),
      projects: [expect.objectContaining({ name: "Project One" })],
      appearance: { mode: "dark", fontScale: 1 },
    });

  }, 10_000);

  it("reconciles out-of-order extension inspections without losing either result", async () => {
    const firstPath = "/tmp/first.aworkit-extension.json";
    const secondPath = "/tmp/second.aworkit-extension.json";
    const paths = [firstPath, secondPath];
    const firstInspection = deferredValue<ExtensionConfiguration>();
    const secondInspection = deferredValue<ExtensionConfiguration>();
    const port = new RecordingSettingsV2Port();
    port.extensionInspection = (path) =>
      path === firstPath ? firstInspection.promise : secondInspection.promise;
    const user = userEvent.setup();
    render(
      <SettingsScreen
        settingsPort={port}
        presentation={presentation({
          pickFile: async () => paths.shift() ?? null,
        })}
      />,
    );

    await screen.findByLabelText("Base URL");
    await user.click(screen.getByRole("button", { name: /Extensions/ }));
    const discover = screen.getByRole("button", {
      name: "Discover manifest…",
    });
    await user.click(discover);
    await user.click(discover);
    await waitFor(() => expect(port.extensionInspections).toHaveLength(2));

    secondInspection.resolve(
      discoveredExtension("extension.second", secondPath),
    );
    expect(
      await screen.findByRole("heading", { name: "extension.second" }),
    ).toBeVisible();
    firstInspection.resolve(discoveredExtension("extension.first", firstPath));
    expect(
      await screen.findByRole("heading", { name: "extension.first" }),
    ).toBeVisible();
    expect(port.extensionInspectionCompletions).toBe(2);

    await user.click(screen.getByRole("button", { name: "Save configuration" }));
    await waitFor(() => expect(port.commits).toHaveLength(1));
    expect(
      port.commits[0]?.settings.extensions.map(({ id }) => id),
    ).toEqual(["extension.second", "extension.first"]);
  });

  it("keeps the latest requested duplicate extension discovery deterministically", async () => {
    const firstPath = "/tmp/same-old.aworkit-extension.json";
    const secondPath = "/tmp/same-new.aworkit-extension.json";
    const paths = [firstPath, secondPath];
    const firstInspection = deferredValue<ExtensionConfiguration>();
    const secondInspection = deferredValue<ExtensionConfiguration>();
    const port = new RecordingSettingsV2Port();
    port.extensionInspection = (path) =>
      path === firstPath ? firstInspection.promise : secondInspection.promise;
    const user = userEvent.setup();
    render(
      <SettingsScreen
        settingsPort={port}
        presentation={presentation({
          pickFile: async () => paths.shift() ?? null,
        })}
      />,
    );

    await screen.findByLabelText("Base URL");
    await user.click(screen.getByRole("button", { name: /Extensions/ }));
    const discover = screen.getByRole("button", {
      name: "Discover manifest…",
    });
    await user.click(discover);
    await user.click(discover);
    await waitFor(() => expect(port.extensionInspections).toHaveLength(2));

    secondInspection.resolve(
      discoveredExtension("extension.same", secondPath, "2.0.0"),
    );
    expect(await screen.findByText(/Version 2\.0\.0/)).toBeVisible();
    firstInspection.resolve(
      discoveredExtension("extension.same", firstPath, "1.0.0"),
    );
    await waitFor(() => expect(port.extensionInspectionCompletions).toBe(2));
    expect(
      screen.getAllByRole("heading", { name: "extension.same" }),
    ).toHaveLength(1);
    expect(screen.getByText(/Version 2\.0\.0/)).toBeVisible();
    expect(screen.queryByText(/Version 1\.0\.0/)).toBeNull();

    await user.click(screen.getByRole("button", { name: "Save configuration" }));
    await waitFor(() => expect(port.commits).toHaveLength(1));
    expect(port.commits[0]?.settings.extensions).toEqual([
      expect.objectContaining({
        id: "extension.same",
        version: "2.0.0",
        manifestPath: secondPath,
      }),
    ]);
  });

  it("does not resurrect an extension removed while re-inspection is in flight", async () => {
    const existing = discoveredExtension(
      "extension.existing",
      "/tmp/existing.aworkit-extension.json",
    );
    const initial = configuration();
    initial.extensions = [existing];
    const inspection = deferredValue<ExtensionConfiguration>();
    const port = new RecordingSettingsV2Port(initial);
    port.extensionInspection = () => inspection.promise;
    const user = userEvent.setup();
    render(
      <SettingsScreen
        settingsPort={port}
        presentation={presentation({
          pickFile: async () => existing.manifestPath,
        })}
      />,
    );

    await screen.findByLabelText("Base URL");
    await user.click(screen.getByRole("button", { name: /Extensions/ }));
    await user.click(
      screen.getByRole("button", { name: "Discover manifest…" }),
    );
    await waitFor(() => expect(port.extensionInspections).toHaveLength(1));
    await user.click(
      screen.getByRole("button", { name: "Remove extension.existing" }),
    );
    expect(
      screen.queryByRole("heading", { name: "extension.existing" }),
    ).toBeNull();

    inspection.resolve(
      discoveredExtension(
        "extension.existing",
        existing.manifestPath,
        "2.0.0",
      ),
    );
    await waitFor(() => expect(port.extensionInspectionCompletions).toBe(1));
    expect(
      screen.queryByRole("heading", { name: "extension.existing" }),
    ).toBeNull();

    await user.click(screen.getByRole("button", { name: "Save configuration" }));
    await waitFor(() => expect(port.commits).toHaveLength(1));
    expect(port.commits[0]?.settings.extensions).toEqual([]);
  });

  it("ignores extension inspection that completes after an accepted Discard", async () => {
    const inspection = deferredValue<ExtensionConfiguration>();
    const port = new RecordingSettingsV2Port();
    port.extensionInspection = () => inspection.promise;
    const user = userEvent.setup();
    render(
      <SettingsScreen
        settingsPort={port}
        presentation={presentation({
          pickFile: async () => "/tmp/stale.aworkit-extension.json",
        })}
      />,
    );

    await screen.findByLabelText("Base URL");
    await user.click(screen.getByRole("button", { name: /Extensions/ }));
    await user.click(
      screen.getByRole("button", { name: "Discover manifest…" }),
    );
    await waitFor(() => expect(port.extensionInspections).toHaveLength(1));
    await user.click(screen.getByRole("button", { name: /Appearance/ }));
    await user.click(screen.getByRole("radio", { name: /Dark/ }));
    await user.click(screen.getByRole("button", { name: "Discard" }));
    await waitFor(() =>
      expect(screen.getByRole("radio", { name: /System/ })).toBeChecked(),
    );

    inspection.resolve(
      discoveredExtension(
        "extension.stale",
        "/tmp/stale.aworkit-extension.json",
      ),
    );
    await waitFor(() => expect(port.extensionInspectionCompletions).toBe(1));
    await user.click(screen.getByRole("button", { name: /Extensions/ }));
    expect(
      screen.queryByRole("heading", { name: "extension.stale" }),
    ).toBeNull();
    expect(screen.getByText(/Version 1 · saved/)).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Save configuration" }),
    ).toBeDisabled();
  });

  it("registers a saved extension without claiming workflow execution readiness", async () => {
    const port = new RecordingSettingsV2Port();
    const user = userEvent.setup();
    render(
      <SettingsScreen
        settingsPort={port}
        presentation={presentation({
          pickFile: async () => "/tmp/example.aworkit-extension.json",
        })}
      />,
    );

    await screen.findByLabelText("Base URL");
    await user.click(screen.getByRole("button", { name: /Extensions/ }));
    await user.click(
      screen.getByRole("button", { name: "Discover manifest…" }),
    );
    expect(await screen.findByText("discovered")).toBeVisible();
    expect(screen.getByRole("checkbox", { name: "I trust this code" })).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Register installed package" }),
    ).toBeDisabled();

    await user.click(screen.getByRole("button", { name: "Save configuration" }));
    await waitFor(() => expect(port.commits).toHaveLength(1));
    const register = screen.getByRole("button", {
      name: "Register installed package",
    });
    expect(register).toBeEnabled();
    await user.click(register);

    await waitFor(() => expect(port.extensionRegistrations).toHaveLength(1));
    expect(port.extensionRegistrations[0]).toMatchObject({
      expectedVersion: 2,
      extensionId: "extension.fixture",
    });
    expect(await screen.findByText("installed")).toBeVisible();
    expect(screen.getByText(/Verified entry-point digest/)).toBeVisible();
    expect(screen.getByText(/Registration verified this package/)).toBeVisible();
    expect(screen.queryByRole("button", { name: "Remove extension.fixture" })).toBeNull();
    expect(screen.getByRole("checkbox", { name: "I trust this code" })).toBeEnabled();
    expect(
      screen.getByRole("checkbox", {
        name: "Workflow execution not available",
      }),
    ).toBeDisabled();

    await user.click(screen.getByRole("checkbox", { name: "I trust this code" }));
    expect(
      screen.getByRole("checkbox", {
        name: "Workflow execution not available",
      }),
    ).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Save configuration" }));
    await waitFor(() => expect(port.commits).toHaveLength(2));
    expect(port.commits[1]?.settings.extensions[0]).toMatchObject({
      status: "installed",
      trustAccepted: true,
      enabled: false,
    });
  });

  it("fails closed when extension registration cannot recover its receipt version", async () => {
    const initial = configuration();
    initial.extensions = [
      discoveredExtension(
        "extension.receipt-floor",
        "/tmp/receipt-floor.aworkit-extension.json",
      ),
    ];
    const port = new RecordingSettingsV2Port(initial);
    port.mutationSnapshotVersionOffset = -1;
    port.mutationSnapshotFaultCount = 2;
    const user = userEvent.setup();
    render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

    await screen.findByLabelText("Base URL");
    await user.click(screen.getByRole("button", { name: /Extensions/ }));
    await user.click(
      screen.getByRole("button", { name: "Register installed package" }),
    );

    expect(
      await screen.findByText(/canonical Settings snapshot is stale/, {
        selector: ".field-error",
      }),
    ).toBeVisible();
    expect(screen.getByText(/Version 1 · saved/)).toBeVisible();
    expect(screen.getByText("discovered")).toBeVisible();
    expect(screen.queryByText("installed")).toBeNull();
  });

  it.each([
    {
      label: "the wrong command ID",
      commandIdOverride: "settings.unrelated-registration-receipt",
      versionOffset: 0,
      message: /exact command ID/,
    },
    {
      label: "a too-low version",
      commandIdOverride: null,
      versionOffset: -1,
      message: /exact expected version transition/,
    },
  ])(
    "keeps registration uncommitted locally when an accepted receipt has $label",
    async ({ commandIdOverride, versionOffset, message }) => {
      const initial = configuration();
      initial.extensions = [
        discoveredExtension(
          "extension.exact-receipt",
          "/tmp/exact-receipt.aworkit-extension.json",
        ),
      ];
      const port = new RecordingSettingsV2Port(initial);
      port.mutationReceiptCommandIdOverride = commandIdOverride;
      port.mutationReceiptVersionOffset = versionOffset;
      const user = userEvent.setup();
      render(<SettingsScreen settingsPort={port} presentation={presentation()} />);

      await screen.findByLabelText("Base URL");
      await user.click(screen.getByRole("button", { name: /Extensions/ }));
      const register = screen.getByRole("button", {
        name: "Register installed package",
      });
      await user.click(register);

      expect(
        await screen.findByText(message, { selector: ".field-error" }),
      ).toBeVisible();
      expect(port.extensionRegistrations).toHaveLength(1);
      expect(screen.getByText(/Version 1 · saved/)).toBeVisible();
      expect(screen.getByText("discovered")).toBeVisible();
      expect(screen.queryByText("installed")).toBeNull();
      await waitFor(() => expect(register).toBeEnabled());
    },
  );
});

function presentation(
  overrides: Partial<{
    confirm: () => Promise<boolean>;
    pickFile: () => Promise<string | null>;
    pickFolder: () => Promise<string | null>;
  }> = {},
) {
  return {
    confirm: vi.fn(overrides.confirm ?? (async () => true)),
    pickFile: vi.fn(overrides.pickFile ?? (async () => null)),
    pickFolder: vi.fn(overrides.pickFolder ?? (async () => null)),
  };
}

function deferred(): { readonly promise: Promise<void>; readonly resolve: () => void } {
  let resolvePromise: (() => void) | undefined;
  const promise = new Promise<void>((resolve) => {
    resolvePromise = resolve;
  });
  return {
    promise,
    resolve: () => resolvePromise?.(),
  };
}

function deferredValue<T>(): {
  readonly promise: Promise<T>;
  readonly resolve: (value: T) => void;
} {
  let resolvePromise: ((value: T) => void) | undefined;
  const promise = new Promise<T>((resolve) => {
    resolvePromise = resolve;
  });
  return {
    promise,
    resolve: (value) => resolvePromise?.(value),
  };
}

class RecordingSettingsV2Port implements SettingsV2CorePort {
  public readonly commits: SettingsV2Commit[] = [];
  public readonly probes: ProviderProbeRequest[] = [];
  public readonly discoveries: ModelDiscoveryRequest[] = [];
  public readonly credentialStores: CredentialStoreCommand[] = [];
  public readonly credentialDeletes: CredentialDeleteCommand[] = [];
  public readonly mcpProbes: McpProbeRequest[] = [];
  public readonly projectProbes: ProjectProbeRequest[] = [];
  public readonly toolProbes: ToolProbeRequest[] = [];
  public readonly externalAgentProbes: ExternalAgentProbeRequest[] = [];
  public readonly extensionInspections: string[] = [];
  public readonly extensionRegistrations: ExtensionRegisterCommand[] = [];
  public extensionInspectionCompletions = 0;
  public extensionInspection:
    | ((path: string) => Promise<ExtensionConfiguration>)
    | null = null;
  public conflictOnce = false;
  public mutationReceiptVersionOffset = 0;
  public mutationReceiptCommandIdOverride: string | null = null;
  public mutationSnapshotVersionOffset = 0;
  public mutationSnapshotFaultCount = 0;
  public mutationSnapshotContentMismatch = false;
  public credentialConflictOnce = false;
  public credentialLostResponseOnce = false;
  public credentialMismatchedReceiptOnce = false;
  public credentialDeleteLostResponseOnce = false;
  public credentialDeleteGate: Promise<void> | null = null;
  public commitGate: Promise<void> | null = null;
  public snapshotGateOnce: Promise<void> | null = null;
  public snapshotGateCompletions = 0;
  public providerProbeGate: Promise<void> | null = null;
  public modelDiscoveryGate: Promise<void> | null = null;
  public providerProbeCompletions = 0;
  public modelDiscoveryCompletions = 0;
  public snapshotCalls = 0;
  public snapshotFailureOnce = false;
  public credentialSnapshotPostconditionFailure:
    | "stale_version"
    | "missing_reference"
    | null = null;
  public credentialSnapshotPostconditionFailuresRemaining = 0;
  private state: SettingsV2Snapshot;
  private mutationSnapshotFaultsRemaining = 0;
  private readonly processedCredentialStores = new Map<
    string,
    CredentialStoreReceipt
  >();
  private readonly processedCredentialDeletes = new Map<
    string,
    SettingsV2Receipt
  >();

  public constructor(initialSettings: SettingsConfigurationV2 = configuration()) {
    this.state = {
      ...snapshot(),
      settings: structuredClone(initialSettings),
    };
  }

  public async snapshot(): Promise<SettingsV2Snapshot> {
    this.snapshotCalls += 1;
    if (this.snapshotFailureOnce) {
      this.snapshotFailureOnce = false;
      throw new Error("settings snapshot response was lost");
    }
    const latest = structuredClone(this.state);
    if (this.mutationSnapshotContentMismatch && this.commits.length > 0) {
      latest.settings.appearance = { mode: "dark", fontScale: 1.5 };
    }
    const snapshotGate = this.snapshotGateOnce;
    if (snapshotGate !== null) {
      this.snapshotGateOnce = null;
      await snapshotGate;
      this.snapshotGateCompletions += 1;
    }
    if (this.mutationSnapshotFaultsRemaining > 0) {
      this.mutationSnapshotFaultsRemaining -= 1;
      return {
        ...latest,
        version: latest.version + this.mutationSnapshotVersionOffset,
      };
    }
    const processedReceipts = [...this.processedCredentialStores.values()];
    const credentialReceipt = processedReceipts[processedReceipts.length - 1];
    if (
      this.credentialSnapshotPostconditionFailure === null ||
      this.credentialSnapshotPostconditionFailuresRemaining <= 0 ||
      credentialReceipt === undefined
    )
      return latest;
    this.credentialSnapshotPostconditionFailuresRemaining -= 1;
    if (this.credentialSnapshotPostconditionFailure === "stale_version") {
      return {
        ...latest,
        version: credentialReceipt.currentVersion - 1,
      };
    }
    return {
      ...latest,
      settings: {
        ...latest.settings,
        credentials: latest.settings.credentials.filter(
          ({ credentialRef }) =>
            credentialRef !==
            credentialReceipt.credentialMutation.freshCredentialRef,
        ),
      },
    };
  }

  public async commit(command: SettingsV2Commit): Promise<SettingsV2Receipt> {
    this.commits.push(structuredClone(command));
    if (this.commitGate !== null) await this.commitGate;
    if (this.conflictOnce) {
      this.conflictOnce = false;
      this.state = {
        ...this.state,
        version: this.state.version + 1,
        settings: {
          ...this.state.settings,
          appearance: { mode: "light", fontScale: 1.15 },
        },
      };
      throw new Error(
        `settings version conflict: expected ${command.expectedVersion}, actual ${this.state.version}`,
      );
    }
    this.state = {
      ...this.state,
      version: this.state.version + 1,
      settings: structuredClone(command.settings),
    };
    this.mutationSnapshotFaultsRemaining = this.mutationSnapshotFaultCount;
    return receipt(
      this.mutationReceiptCommandIdOverride ?? command.commandId,
      this.state.version + this.mutationReceiptVersionOffset,
    );
  }

  public async storeCredential(
    command: CredentialStoreCommand,
  ): Promise<CredentialStoreReceipt> {
    this.credentialStores.push(structuredClone(command));
    const processed = this.processedCredentialStores.get(command.commandId);
    if (processed !== undefined) return structuredClone(processed);
    if (this.credentialConflictOnce) {
      this.credentialConflictOnce = false;
      this.state = {
        ...this.state,
        version: this.state.version + 1,
        settings: {
          ...this.state.settings,
          credentials: [
            ...this.state.settings.credentials,
            {
              credentialRef: "credential.concurrent-unrelated",
              label: "Concurrent unrelated credential",
              kind: "api_key",
              fieldNames: ["api_key"],
              revision: 1,
              boundProviderId: null,
              boundEndpoint: null,
            },
          ],
        },
      };
      throw new Error(
        `settings version conflict: expected ${command.expectedVersion}, actual ${this.state.version}`,
      );
    }
    if (command.expectedVersion !== this.state.version) {
      throw new Error(
        `settings version conflict: expected ${command.expectedVersion}, actual ${this.state.version}`,
      );
    }
    const replacementRef =
      command.replaceCredentialRef === null
        ? "credential.recorded"
        : "credential.replacement";
    const previousMetadata =
      command.replaceCredentialRef === null
        ? undefined
        : this.state.settings.credentials.find(
            ({ credentialRef }) =>
              credentialRef === command.replaceCredentialRef,
          );
    let settings = structuredClone(this.state.settings);
    if (command.replaceCredentialRef !== null) {
      settings = replaceCredentialReferencesForTest(
        settings,
        command.replaceCredentialRef,
        replacementRef,
      );
      settings.credentials = settings.credentials.filter(
        ({ credentialRef }) => credentialRef !== command.replaceCredentialRef,
      );
    }
    settings.credentials.push({
      credentialRef: replacementRef,
      label: command.label,
      kind: command.kind,
      fieldNames: Object.keys(command.fields),
      revision: (previousMetadata?.revision ?? 0) + 1,
      boundProviderId: command.boundProviderId,
      boundEndpoint: command.boundEndpoint,
    });
    this.state = {
      ...this.state,
      version: this.state.version + 1,
      settings,
    };
    const committedReceipt: CredentialStoreReceipt = {
      ...receipt(command.commandId, this.state.version),
      credentialMutation: {
        operation:
          command.replaceCredentialRef === null ? "create" : "replace",
        previousCredentialRef: command.replaceCredentialRef,
        freshCredentialRef: replacementRef,
      },
    };
    this.processedCredentialStores.set(command.commandId, committedReceipt);
    if (this.credentialLostResponseOnce) {
      this.credentialLostResponseOnce = false;
      throw new Error("credential response was lost after commit");
    }
    if (this.credentialMismatchedReceiptOnce) {
      this.credentialMismatchedReceiptOnce = false;
      return {
        ...committedReceipt,
        commandId: "settings.mismatched-receipt",
      };
    }
    return structuredClone(committedReceipt);
  }

  public async deleteCredential(
    command: CredentialDeleteCommand,
  ): Promise<SettingsV2Receipt> {
    this.credentialDeletes.push(structuredClone(command));
    const processed = this.processedCredentialDeletes.get(command.commandId);
    if (processed !== undefined) return structuredClone(processed);
    if (command.expectedVersion !== this.state.version) {
      throw new Error(
        `settings version conflict: expected ${command.expectedVersion}, actual ${this.state.version}`,
      );
    }
    if (this.credentialDeleteGate !== null) await this.credentialDeleteGate;
    this.state = {
      ...this.state,
      version: this.state.version + 1,
      settings: {
        ...this.state.settings,
        credentials: this.state.settings.credentials.filter(
          ({ credentialRef }) => credentialRef !== command.credentialRef,
        ),
      },
    };
    const committedReceipt = receipt(command.commandId, this.state.version);
    this.processedCredentialDeletes.set(command.commandId, committedReceipt);
    if (this.credentialDeleteLostResponseOnce) {
      this.credentialDeleteLostResponseOnce = false;
      throw new Error("credential deletion response was lost after commit");
    }
    return structuredClone(committedReceipt);
  }

  public async testProvider(
    request: ProviderProbeRequest,
  ): Promise<ProviderProbeResult> {
    this.probes.push(structuredClone(request));
    if (this.providerProbeGate !== null) await this.providerProbeGate;
    this.providerProbeCompletions += 1;
    return {
      ok: true,
      message: "Connection succeeded.",
      providerId: request.provider.id,
      modelId: request.modelId,
      remoteModelId: request.provider.models.find(({ id }) => id === request.modelId)?.remoteId ?? null,
      latencyMillis: 12,
      draftFingerprint: request.draftFingerprint,
    };
  }

  public async discoverModels(
    request: ModelDiscoveryRequest,
  ): Promise<ModelDiscoveryResult> {
    this.discoveries.push(structuredClone(request));
    if (this.modelDiscoveryGate !== null) await this.modelDiscoveryGate;
    this.modelDiscoveryCompletions += 1;
    return {
      providerId: request.provider.id,
      draftFingerprint: request.draftFingerprint,
      message: "Discovered one additional model.",
      models: [
        {
          remoteId: "remote-model-2",
          name: "Remote model 2",
          contextWindow: 32_000,
          maxOutputTokens: 4_096,
          capabilities: ["text"],
        },
      ],
    };
  }

  public async probeMcp(request: McpProbeRequest): Promise<McpProbeResult> {
    this.mcpProbes.push(structuredClone(request));
    return {
      serverId: request.server.id,
      protocolVersion: "2026-07-28",
      features: {
        tools: true,
        resources: true,
        prompts: true,
        progress: true,
        cancellation: true,
      },
      toolNames: ["fixture.read"],
      resourceNames: ["fixture://resource"],
      promptNames: ["fixture-prompt"],
      bindingHash: `sha256:${"1".repeat(64)}`,
      catalogHash: `sha256:${"2".repeat(64)}`,
      latencyMillis: 7,
      draftFingerprint: request.draftFingerprint,
      message: "Connected using MCP 2026-07-28; discovered 3 catalog item(s).",
    };
  }

  public async probeProject(
    request: ProjectProbeRequest,
  ): Promise<ProjectProbeResult> {
    this.projectProbes.push(structuredClone(request));
    return {
      ok: true,
      projectId: request.project.id,
      workspaceKind: request.project.workspace.kind,
      resolvedLocation: request.project.workspace.location,
      message: "Workspace resolved.",
      draftFingerprint: request.draftFingerprint,
    };
  }

  public async probeTool(request: ToolProbeRequest): Promise<ToolProbeResult> {
    this.toolProbes.push(structuredClone(request));
    return {
      ok: true,
      toolId: request.tool.id,
      adapter: "fixture-adapter",
      message: "Tool adapter ready.",
      draftFingerprint: request.draftFingerprint,
    };
  }

  public async probeExternalAgent(
    request: ExternalAgentProbeRequest,
  ): Promise<ExternalAgentProbeResult> {
    this.externalAgentProbes.push(structuredClone(request));
    return {
      agentId: request.agent.id,
      protocol: "codex-app-server-jsonrpc-stdio",
      serverIdentity: "codex-cli/fixture",
      platformFamily: "unix",
      platformOs: "linux",
      accountType: "chatgpt",
      requiresOpenaiAuth: true,
      modelIds: ["gpt-fixture"],
      capabilities: {
        progress: true,
        continuation: true,
        cancellation: true,
        approvals: true,
      },
      latencyMillis: 9,
      draftFingerprint: request.draftFingerprint,
      message: "Codex App Server handshake completed; 1 model available.",
    };
  }

  public async inspectExtension(path: string): Promise<ExtensionConfiguration> {
    this.extensionInspections.push(path);
    const inspected =
      this.extensionInspection === null
        ? discoveredExtension("extension.fixture", path)
        : await this.extensionInspection(path);
    this.extensionInspectionCompletions += 1;
    return inspected;
  }

  public async registerExtension(
    command: ExtensionRegisterCommand,
  ): Promise<SettingsV2Receipt> {
    this.extensionRegistrations.push(structuredClone(command));
    this.state = {
      ...this.state,
      version: this.state.version + 1,
      settings: {
        ...this.state.settings,
        extensions: this.state.settings.extensions.map((extension) =>
          extension.id === command.extensionId
            ? {
                ...extension,
                status: "installed" as const,
                enabled: false,
                trustAccepted: false,
                entryPoint: "/tmp/fixture-extension",
                compatibility: "compatible with this build",
                provenance: "verified without execution",
                configuration: {
                  ...extension.configuration,
                  installationState: "registered_inert",
                  integrityState: "verified_entry_point_content",
                },
              }
            : extension,
        ),
      },
    };
    this.mutationSnapshotFaultsRemaining = this.mutationSnapshotFaultCount;
    return {
      ...receipt(
        this.mutationReceiptCommandIdOverride ?? command.commandId,
        this.state.version + this.mutationReceiptVersionOffset,
      ),
      reason: "Registered while disabled and untrusted.",
    };
  }
}

function receipt(commandId: string, currentVersion: number): SettingsV2Receipt {
  return { commandId, accepted: true, currentVersion, reason: null };
}

function discoveredExtension(
  id: string,
  manifestPath: string,
  version = "1.0.0",
): ExtensionConfiguration {
  return {
    id,
    name: id,
    version,
    status: "discovered",
    enabled: false,
    trustAccepted: false,
    manifestPath,
    entryPoint: "fixture-extension",
    contentHash: `sha256:${"3".repeat(64)}`,
    compatibility: "compatible",
    provenance: "inert local manifest inspection",
    configuration: {},
  };
}

function credentialMetadata(
  credentialRef: string,
): SettingsConfigurationV2["credentials"][number] {
  return {
    credentialRef,
    label: "Shared API key",
    kind: "api_key",
    fieldNames: ["api_key"],
    revision: 1,
    boundProviderId: null,
    boundEndpoint: null,
  };
}

function configurationWithSharedCredential(
  credentialRef: string,
): SettingsConfigurationV2 {
  const settings = configuration();
  settings.credentials = [credentialMetadata(credentialRef)];
  settings.providers = settings.providers.map((provider) => ({
    ...provider,
    credentialRef,
  }));
  settings.mcpServers = [
    {
      id: "mcp.fixture",
      name: "Fixture MCP",
      enabled: false,
      autoConnect: false,
      transport: {
        transport: "stdio",
        // A valid launcher shape on every platform: absolute on Unix, one
        // bare PATH command name on Windows.
        command:
          process.platform === "win32" ? "fixture-mcp" : "/usr/bin/fixture-mcp",
        args: [],
        cwd: null,
        env: [
          {
            name: "MCP_API_KEY",
            credentialRef,
            field: "api_key",
          },
        ],
      },
    },
  ];
  settings.externalAgents = [
    {
      ...externalAgentConfiguration(),
      id: "agent.fixture",
      name: "Fixture external agent",
      connection: {
        transport: "stdio",
        command: "codex",
        args: ["app-server"],
        cwd: null,
        env: [
          {
            name: "CODEX_API_KEY",
            credentialRef,
            field: "api_key",
          },
        ],
      },
      credentialBindings: [
        {
          name: "AWORKIT_API_KEY",
          credentialRef,
          field: "api_key",
        },
      ],
    },
  ];
  return settings;
}

function replaceCredentialReferencesForTest(
  settings: SettingsConfigurationV2,
  previousRef: string,
  replacementRef: string,
): SettingsConfigurationV2 {
  const replaceBindings = <T extends {
    readonly credentialRef: string;
  }>(bindings: readonly T[]): T[] =>
    bindings.map((binding) =>
      binding.credentialRef === previousRef
        ? { ...binding, credentialRef: replacementRef }
        : binding,
    );
  const replaceConnection = (
    connection: SettingsConfigurationV2["mcpServers"][number]["transport"],
  ): SettingsConfigurationV2["mcpServers"][number]["transport"] =>
    connection.transport === "http"
      ? { ...connection, headers: replaceBindings(connection.headers) }
      : { ...connection, env: replaceBindings(connection.env) };
  return {
    ...settings,
    providers: settings.providers.map((provider) => ({
      ...provider,
      credentialRef:
        provider.credentialRef === previousRef
          ? replacementRef
          : provider.credentialRef,
    })),
    tools: settings.tools.map((tool) => ({
      ...tool,
      credentialBindings: replaceBindings(tool.credentialBindings),
    })),
    mcpServers: settings.mcpServers.map((server) => ({
      ...server,
      transport: replaceConnection(server.transport),
    })),
    externalAgents: settings.externalAgents.map((agent) => {
      const bindings = [
        ...(agent.connection.transport === "http"
          ? agent.connection.headers
          : agent.connection.env),
        ...agent.credentialBindings,
      ];
      const invalidatesCapabilities = bindings.some(
        ({ credentialRef }) => credentialRef === previousRef,
      );
      return {
        ...agent,
        connection: replaceConnection(agent.connection),
        credentialBindings: replaceBindings(agent.credentialBindings),
        capabilities: invalidatesCapabilities
          ? {
              progress: false,
              continuation: false,
              cancellation: false,
              approvals: false,
            }
          : agent.capabilities,
      };
    }),
  };
}

function externalAgentConfiguration(
  capabilities: Partial<
    SettingsConfigurationV2["externalAgents"][number]["capabilities"]
  > = {},
): SettingsConfigurationV2["externalAgents"][number] {
  return {
    id: "agent.legacy",
    name: "Legacy external agent",
    adapter: "codex_app_server",
    enabled: false,
    connection: {
      transport: "stdio",
      command: "codex",
      args: ["app-server"],
      cwd: null,
      env: [],
    },
    credentialBindings: [],
    mcpServerIds: [],
    capabilities: {
      progress: false,
      continuation: false,
      cancellation: false,
      approvals: false,
      ...capabilities,
    },
    configuration: {},
  };
}

function projectConfiguration(
  id: string,
  name: string,
  kind: SettingsConfigurationV2["projects"][number]["workspace"]["kind"],
): SettingsConfigurationV2["projects"][number] {
  return {
    id,
    name,
    workspace: {
      kind,
      location: kind === "remote" ? "workspace://remote" : "/tmp/project",
    },
    defaultWorkflowId: "workflow.simple-chat",
    portableHistoryEnabled: false,
  };
}

function snapshot(): SettingsV2Snapshot {
  return {
    version: 1,
    schemaVersion: 2,
    settings: configuration(),
    providerHealth: [
      {
        providerId: "provider.local",
        state: "configured",
        detail: "Saved locally; not tested yet.",
      },
    ],
  };
}

function configuration(): SettingsConfigurationV2 {
  return {
    approvals: { defaultMode: "ask_for_approval" },
    schemaVersion: 2,
    providers: [
      {
        id: "provider.local",
        name: "Local provider",
        kind: "openai_compatible",
        baseUrl: "http://127.0.0.1:11434/v1",
        enabled: true,
        credentialRef: null,
        models: [
          {
            id: "model.chat",
            name: "Chat model",
            remoteId: "chat-model",
            enabled: true,
            contextWindow: 16_000,
            maxOutputTokens: 2_048,
            capabilities: ["text", "tools"],
            parameters: {},
          },
        ],
        configuration: {},
      },
    ],
    modelTiers: ["fast", "simple", "balanced", "quality"].map((name) => ({
      id: `tier:${name}`,
      name: name.replace(/^./u, (letter) => letter.toUpperCase()),
      kind: "standard" as const,
      resolution: {
        strategy: "exact" as const,
        target: { providerId: "provider.local", modelId: "model.chat" },
      },
    })),
    credentials: [],
    tools: [
      {
        id: "tool.files.read",
        name: "Read project file",
        enabled: true,
        requiresProject: true,
        credentialBindings: [],
        configuration: {
          authorityMode: "project_files",
          effect: "read",
          maximumBytes: 65_536,
        },
      },
    ],
    extensions: [],
    mcpServers: [],
    externalAgents: [],
    data: {
      portableHistoryEnabled: false,
      detailedCaptureEnabled: false,
      portableDirectory: ".aworkit/sessions",
    },
    projects: [],
    appearance: { mode: "system", fontScale: 1 },
  };
}

// @vitest-environment jsdom
import { useState } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { McpServerConfiguration, SettingsV2Snapshot } from "./configuration";
import { NodeConfigurationForm } from "./NodeConfigurationForm";
import { McpServersSection, type IntegrationProbeResult } from "./settings-v2/IntegrationSections";

afterEach(cleanup);
const tools = ["read", "write"].map(name => ({ name, description: name, inputSchema: { type: "object" }, enabled: true }));
const server: McpServerConfiguration = { id: "adashi", name: "Adashi", enabled: false, autoConnect: false,
  transport: { transport: "stdio", command: "adashi-mcp", args: [], cwd: null, env: [] } };
const success: IntegrationProbeResult = { ok: true, message: "Connected", draftFingerprint: "fixture", tools };

function setup(initial: McpServerConfiguration, probe: (server: McpServerConfiguration) => Promise<IntegrationProbeResult>) {
  let latest = initial;
  function Editor() {
    const [servers, setServers] = useState([initial]);
    latest = servers[0];
    return <McpServersSection servers={servers} credentials={[]} onPickCommand={async () => null} onProbe={probe}
      onChange={next => setServers([...next])} />;
  }
  render(<Editor />);
  return () => latest;
}

it("connects and loads functions when the enable checkbox is checked", async () => {
  let finish!: (value: IntegrationProbeResult) => void;
  const probe = vi.fn(() => new Promise<IntegrationProbeResult>(resolve => { finish = resolve; }));
  const latest = setup(server, probe);
  expect(screen.getByText(/Setup required/)).toBeVisible();
  fireEvent.click(screen.getByRole("checkbox", { name: "Enable MCP server" }));
  expect(screen.getByText("Connecting and loading functions…")).toBeVisible();
  expect(latest().enabled).toBe(false);
  finish(success);
  await waitFor(() => expect(latest().enabled).toBe(true));
  expect(latest().tools).toHaveLength(2);
  expect(screen.getByText("Available to workflows · 2 functions")).toBeVisible();
  expect(screen.getByText(/Save configuration to use/)).toBeVisible();
  fireEvent.click(screen.getByRole("checkbox", { name: "Enable MCP server" }));
  expect(latest().enabled).toBe(false);
  expect(probe).toHaveBeenCalledTimes(1);
});

it.each([
  { ok: false, message: "Connection refused", tools: undefined },
  { ok: true, message: "Connected", tools: [] },
])("keeps setup incomplete after an unsuccessful connection or empty catalog: %s", async result => {
  const latest = setup(server, async () => ({ ...result, draftFingerprint: "fixture" }));
  fireEvent.click(screen.getByRole("button", { name: "Connect and enable" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "Connect and enable" })).toBeEnabled());
  expect(latest().enabled).toBe(false);
  expect(screen.getByRole("checkbox", { name: "Enable MCP server" })).not.toBeChecked();
  expect(screen.getByText(result.ok ? /returned no functions/ : "Connection refused")).toBeVisible();
});

it("shows setup required for an older enabled record with no catalog", async () => {
  const latest = setup({ ...server, enabled: true }, async () => success);
  expect(screen.getByRole("checkbox", { name: "Enable MCP server" })).not.toBeChecked();
  fireEvent.click(screen.getByRole("button", { name: "Connect and enable" }));
  await waitFor(() => expect(latest().tools).toHaveLength(2));
  expect(screen.getByRole("checkbox", { name: "Enable MCP server" })).toBeChecked();
});

it("ignores a late connection after the transport changes and requires re-enabling", async () => {
  let finish!: (value: IntegrationProbeResult) => void;
  const latest = setup({ ...server, enabled: true, tools }, () => new Promise(resolve => { finish = resolve; }));
  fireEvent.click(screen.getByRole("button", { name: "Refresh functions" }));
  fireEvent.change(screen.getByLabelText("Command"), { target: { value: "another-mcp" } });
  finish({ ...success, tools: [...tools, { ...tools[0], name: "stale" }] });
  await waitFor(() => expect(screen.getByRole("button", { name: "Connect and enable" })).toBeEnabled());
  expect(latest().enabled).toBe(false);
  expect(latest().tools).toHaveLength(2);
  expect(screen.queryByText(/Connection successful/)).toBeNull();
});

function agent(initial: string[], configured = [{ ...server, enabled: true, tools }]) {
  let latest = initial;
  function Editor() {
    const [selected, setSelected] = useState(initial);
    latest = selected;
    return <NodeConfigurationForm nodeType="agent" editable configuration={{ modelTierId: "tier:balanced", toolIds: selected }}
      settings={{ settings: { tools: [], mcpServers: configured, modelTiers: [] } } as unknown as SettingsV2Snapshot}
      onChange={patch => setSelected(patch.toolIds as string[])} />;
  }
  render(<Editor />);
  return () => latest;
}

it("uses one server checkbox for all functions and saves a server binding", () => {
  const latest = agent([]);
  expect(screen.getAllByRole("checkbox")).toHaveLength(1);
  fireEvent.click(screen.getByRole("checkbox", { name: /Adashi/ }));
  expect(latest()).toEqual(["mcp:adashi"]);
  fireEvent.click(screen.getByRole("checkbox", { name: /Adashi/ }));
  expect(latest()).toEqual([]);
});

it("preserves old partial selections until the user selects the whole server", () => {
  const latest = agent(["mcp://adashi/read", "mcp://missing/keep"]);
  expect(screen.getByRole("checkbox", { name: /Adashi/ })).toBePartiallyChecked();
  expect(latest()).toEqual(["mcp://adashi/read", "mcp://missing/keep"]);
  fireEvent.click(screen.getByRole("checkbox", { name: /Adashi/ }));
  expect(latest()).toEqual(["mcp://missing/keep", "mcp:adashi"]);
});

it("shows unavailable servers with a setup hint, while allowing old bindings to be removed", () => {
  const latest = agent(["mcp:adashi"], [{ ...server, enabled: false, tools: [] }]);
  const checkbox = screen.getByRole("checkbox", { name: /Adashi/ });
  expect(checkbox).toBeEnabled();
  fireEvent.click(checkbox);
  expect(latest()).toEqual([]);
  expect(checkbox).toBeDisabled();
});

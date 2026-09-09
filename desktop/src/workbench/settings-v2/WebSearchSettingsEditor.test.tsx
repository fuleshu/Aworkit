// @vitest-environment jsdom

import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, it } from "vitest";

import type {
  BuiltInToolConfiguration,
  CredentialMetadataConfiguration,
} from "../configuration";
import { WebSearchSettingsEditor } from "./WebSearchSettingsEditor";

afterEach(cleanup);

const configuration = {
  backend: "keyless",
  credentialBackend: "deepseek",
  providerTier: "automatic",
  maximumResults: 10,
  requestTimeoutSeconds: 30,
  maximumRetries: 1,
  keylessFallback: true,
  keylessRescue: true,
  cacheEnabled: true,
  cacheTtlMinutes: 20,
  searxngBaseUrl: "",
  providerBaseUrl: "",
  parallelSearchMode: "agentic",
  xaiModel: "grok-build-0.1",
  xaiAllowedDomains: [],
  xaiExcludedDomains: [],
  deepseekBaseUrl: "https://api.deepseek.com",
  deepseekModel: "deepseek-v4-flash",
  deepseekMaximumOutputTokens: 4_096,
};

const credential: CredentialMetadataConfiguration = {
  credentialRef: "credential.search",
  label: "Search API key",
  kind: "api_key",
  fieldNames: ["api_key"],
  revision: 1,
};

function Fixture({ withBinding = false, overrides = {} }: {
  readonly withBinding?: boolean;
  readonly overrides?: Partial<typeof configuration>;
}) {
  const [tool, setTool] = useState<BuiltInToolConfiguration>({
    id: "tool.web_search",
    name: "Web search",
    enabled: true,
    requiresProject: false,
    credentialBindings: withBinding
      ? [
          {
            name: "api_key",
            credentialRef: credential.credentialRef,
            field: "api_key",
          },
        ]
      : [],
    configuration: { ...configuration, ...overrides },
  });
  return (
    <>
      <WebSearchSettingsEditor
        tool={tool}
        credentials={[credential]}
        onChange={setTool}
      />
      <output data-testid="saved-tool">{JSON.stringify(tool)}</output>
    </>
  );
}

function editedTool(): BuiltInToolConfiguration {
  return JSON.parse(screen.getByTestId("saved-tool").textContent!);
}

describe("WebSearchSettingsEditor", () => {
  it("starts with grouped free automatic search and only shows relevant controls", () => {
    render(<Fixture />);
    const provider = screen.getByLabelText("Search provider");
    expect(provider).toHaveValue("keyless");
    expect(within(provider).getAllByRole("group").map(group => group.getAttribute("label")))
      .toEqual(["Automatic", "Free providers", "Self-hosted", "Paid providers"]);
    expect(within(provider).getAllByRole("option")[0]).toHaveTextContent("Automatic (free only)");
    expect(screen.queryByLabelText("Provider tier")).toBeNull();
    expect(screen.queryByLabelText("API credential")).toBeNull();
    expect(screen.queryByLabelText("Try free search if the paid provider fails")).toBeNull();
    expect(screen.getByLabelText("Freshness validation")).toBeChecked();
    expect(screen.getByLabelText("Current-result age (days)")).toHaveValue(45);
    expect(screen.getByLabelText("Bypass cache for live data")).toBeChecked();
  });

  it("preserves existing automatic paid-preferred settings", () => {
    render(<Fixture withBinding overrides={{ backend: "automatic" }} />);
    expect(screen.getByLabelText("Search provider")).toHaveValue("automatic");
    expect(screen.getByLabelText("Preferred API provider")).toHaveValue("deepseek");
    expect(screen.getByLabelText("SearXNG base URL")).toHaveAttribute("title");
    expect(screen.getByLabelText("Search model")).toHaveValue("deepseek-v4-flash");
    expect(screen.getByText(/incur model token charges/)).toBeVisible();
    expect(editedTool().configuration.backend).toBe("automatic");
    expect(editedTool().credentialBindings).toHaveLength(1);
  });

  it.each([
    ["keyless", "keyless", "automatic"],
    ["duckduckgo", "duckduckgo", "automatic"],
    ["exa:free", "exa", "free"],
    ["parallel:free", "parallel", "free"],
    ["firecrawl:free", "firecrawl", "free"],
    ["tavily:free", "tavily", "free"],
    ["keenable:free", "keenable", "free"],
  ])("selects %s without retaining a paid credential or disabled free routing", async (value, backend, tier) => {
    const user = userEvent.setup();
    render(<Fixture withBinding overrides={{
      backend: "deepseek", keylessFallback: false, keylessRescue: false,
    }} />);
    await user.selectOptions(screen.getByLabelText("Search provider"), value);
    expect(screen.queryByLabelText("API credential")).toBeNull();
    const tool = editedTool();
    expect(tool.credentialBindings).toEqual([]);
    expect(tool.configuration).toMatchObject({ backend, providerTier: tier, keylessFallback: true });
  });

  it("selects explicit paid billing and persists the single fallback control", async () => {
    const user = userEvent.setup();
    render(<Fixture />);
    await user.selectOptions(screen.getByLabelText("Search provider"), "exa:paid");
    expect(screen.getByText("Exa requires an API key for this route.")).toBeVisible();
    await user.selectOptions(screen.getByLabelText("API credential"), credential.credentialRef);
    expect(editedTool().configuration).toMatchObject({ backend: "exa", providerTier: "paid" });
    expect(editedTool().credentialBindings[0].credentialRef).toBe(credential.credentialRef);
    const fallback = screen.getByLabelText("Try free search if the paid provider fails");
    await user.click(fallback);
    expect(editedTool().configuration).toMatchObject({ keylessFallback: false, keylessRescue: false });
    await user.click(fallback);
    expect(editedTool().configuration).toMatchObject({ keylessFallback: true, keylessRescue: true });
    await user.selectOptions(screen.getByLabelText("Search provider"), "exa:free");
    expect(screen.queryByLabelText("API base URL override")).toBeNull();
    expect(editedTool().credentialBindings).toEqual([]);
  });

  it.each([true, false])("displays a legacy automatic tier by its credential binding (%s) without rewriting it", (withBinding) => {
    render(<Fixture withBinding={withBinding} overrides={{ backend: "exa" }} />);
    expect(screen.getByLabelText("Search provider")).toHaveValue(withBinding ? "exa:paid" : "exa:free");
    expect(editedTool().configuration.providerTier).toBe("automatic");
    expect(editedTool().credentialBindings).toHaveLength(withBinding ? 1 : 0);
  });

  it("selects self-hosted search without a provider credential", async () => {
    const user = userEvent.setup();
    render(<Fixture withBinding overrides={{ backend: "deepseek" }} />);
    await user.selectOptions(screen.getByLabelText("Search provider"), "searxng");
    expect(screen.getByLabelText("SearXNG base URL")).toHaveAttribute("title");
    expect(screen.queryByLabelText("API credential")).toBeNull();
    expect(editedTool().credentialBindings).toEqual([]);
  });

  it("shows xAI model and domain filters for the xAI paid route", async () => {
    const user = userEvent.setup();
    render(<Fixture />);
    await user.selectOptions(screen.getByLabelText("Search provider"), "xai");
    expect(screen.getByLabelText("xAI search model")).toBeVisible();
    expect(screen.getByLabelText("Allowed domains 1")).toHaveAttribute("title");
    expect(screen.getByLabelText("API credential")).toBeVisible();
  });
});

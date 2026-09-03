// @vitest-environment jsdom

import { cleanup, render, screen } from "@testing-library/react";
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
  backend: "automatic",
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

function Fixture({ withBinding = false }: { readonly withBinding?: boolean }) {
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
    configuration,
  });
  return (
    <WebSearchSettingsEditor
      tool={tool}
      credentials={[credential]}
      onChange={setTool}
    />
  );
}

describe("WebSearchSettingsEditor", () => {
  it("exposes Hermes-compatible routing and the paid DeepSeek controls", () => {
    render(<Fixture />);
    expect(screen.getByLabelText("Search backend")).toHaveValue("automatic");
    expect(screen.getByText(/rotates through Exa, Parallel, Firecrawl/)).toBeVisible();
    expect(screen.getByLabelText("SearXNG base URL")).toHaveAttribute("title");
    expect(screen.getByLabelText("Search model")).toHaveValue("deepseek-v4-flash");
    expect(screen.getByLabelText("Freshness validation")).toBeChecked();
    expect(screen.getByLabelText("Current-result age (days)")).toHaveValue(45);
    expect(screen.getByLabelText("Bypass cache for live data")).toBeChecked();
    expect(screen.getByText(/incur model token charges/)).toBeVisible();
  });

  it("pins a free provider without retaining an unnecessary secret binding", async () => {
    const user = userEvent.setup();
    render(<Fixture withBinding />);
    await user.selectOptions(screen.getByLabelText("Search backend"), "exa");
    await user.selectOptions(screen.getByLabelText("Provider tier"), "free");
    expect(screen.queryByLabelText("API credential")).toBeNull();
    expect(screen.queryByLabelText("API base URL override")).toBeNull();
  });

  it("shows xAI model and domain filters for the xAI paid route", async () => {
    const user = userEvent.setup();
    render(<Fixture />);
    await user.selectOptions(screen.getByLabelText("Search backend"), "xai");
    expect(screen.getByLabelText("xAI search model")).toBeVisible();
    expect(screen.getByLabelText("Allowed domains 1")).toHaveAttribute("title");
    expect(screen.getByLabelText("API credential")).toBeVisible();
  });
});

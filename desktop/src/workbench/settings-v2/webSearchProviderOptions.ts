export type SearchBackend =
  | "automatic" | "keyless" | "duckduckgo" | "searxng"
  | "exa" | "parallel" | "firecrawl" | "tavily" | "keenable"
  | "brave" | "xai" | "deepseek";
export type CredentialBackend = Exclude<
  SearchBackend, "automatic" | "keyless" | "duckduckgo" | "searxng"
>;
export type ProviderTier = "automatic" | "free" | "paid";

const DUAL_TIER_BACKENDS = new Set<SearchBackend>([
  "exa", "parallel", "firecrawl", "tavily", "keenable",
]);
export const NO_CREDENTIAL_BACKENDS = new Set<SearchBackend>([
  "keyless", "duckduckgo", "searxng",
]);

/** One visible choice selects both the existing backend and its billing mode. */
type SearchProviderOption = {
  readonly value: string;
  readonly label: string;
  readonly backend: SearchBackend;
  readonly providerTier: ProviderTier;
  readonly allowsCredential: boolean;
};

function option(
  backend: SearchBackend,
  label: string,
  providerTier: ProviderTier = "automatic",
): SearchProviderOption {
  return {
    value: DUAL_TIER_BACKENDS.has(backend) ? `${backend}:${providerTier}` : backend,
    label,
    backend,
    providerTier,
    allowsCredential: !NO_CREDENTIAL_BACKENDS.has(backend) && providerTier !== "free",
  };
}

export const SEARCH_PROVIDER_GROUPS = [
  {
    label: "Automatic",
    options: [
      option("keyless", "Automatic (free only)"),
      option("automatic", "Automatic (paid preferred)"),
    ],
  },
  {
    label: "Free providers",
    options: [
      option("duckduckgo", "DuckDuckGo (free)"),
      option("exa", "Exa (free)", "free"),
      option("parallel", "Parallel (free)", "free"),
      option("firecrawl", "Firecrawl (free)", "free"),
      option("tavily", "Tavily (free)", "free"),
      option("keenable", "Keenable (free)", "free"),
    ],
  },
  {
    label: "Self-hosted",
    options: [option("searxng", "SearXNG (self-hosted)")],
  },
  {
    label: "Paid providers",
    options: [
      option("exa", "Exa (paid)", "paid"),
      option("parallel", "Parallel (paid)", "paid"),
      option("firecrawl", "Firecrawl (paid)", "paid"),
      option("tavily", "Tavily (paid)", "paid"),
      option("keenable", "Keenable (paid)", "paid"),
      option("brave", "Brave Search (paid)"),
      option("xai", "Grok by xAI (paid)"),
      option("deepseek", "DeepSeek (paid)"),
    ],
  },
];

export const SEARCH_PROVIDER_OPTIONS = SEARCH_PROVIDER_GROUPS.flatMap(group => group.options);

/** Display legacy automatic tiers by their current route without rewriting saved settings. */
export function selectedSearchProvider(
  configuration: { readonly backend: SearchBackend; readonly providerTier: ProviderTier },
  hasCredential: boolean,
): SearchProviderOption {
  const tier = configuration.providerTier === "automatic"
    ? (hasCredential ? "paid" : "free")
    : configuration.providerTier;
  const value = DUAL_TIER_BACKENDS.has(configuration.backend)
    ? `${configuration.backend}:${tier}`
    : configuration.backend;
  return SEARCH_PROVIDER_OPTIONS.find(candidate => candidate.value === value)!;
}

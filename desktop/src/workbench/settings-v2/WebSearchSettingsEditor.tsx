import type {
  BuiltInToolConfiguration,
  CredentialMetadataConfiguration,
} from "../configuration";

type SearchBackend =
  | "automatic"
  | "keyless"
  | "duckduckgo"
  | "searxng"
  | "exa"
  | "parallel"
  | "firecrawl"
  | "tavily"
  | "brave"
  | "keenable"
  | "xai"
  | "deepseek";
type CredentialBackend = Exclude<
  SearchBackend,
  "automatic" | "keyless" | "duckduckgo" | "searxng"
>;
type ProviderTier = "automatic" | "free" | "paid";
type ParallelSearchMode = "fast" | "one-shot" | "agentic";

const DUAL_TIER_BACKENDS = new Set<SearchBackend>([
  "exa",
  "parallel",
  "firecrawl",
  "tavily",
  "keenable",
]);
const NO_CREDENTIAL_BACKENDS = new Set<SearchBackend>([
  "keyless",
  "duckduckgo",
  "searxng",
]);

const DEFAULT_CONFIGURATION = {
  backend: "automatic" as SearchBackend,
  credentialBackend: "deepseek" as CredentialBackend,
  providerTier: "automatic" as ProviderTier,
  maximumResults: 10,
  requestTimeoutSeconds: 30,
  maximumRetries: 1,
  keylessFallback: true,
  keylessRescue: true,
  cacheEnabled: true,
  cacheTtlMinutes: 20,
  searxngBaseUrl: "",
  providerBaseUrl: "",
  parallelSearchMode: "agentic" as ParallelSearchMode,
  xaiModel: "grok-build-0.1",
  xaiAllowedDomains: [] as string[],
  xaiExcludedDomains: [] as string[],
  deepseekBaseUrl: "https://api.deepseek.com",
  deepseekModel: "deepseek-v4-flash",
  deepseekMaximumOutputTokens: 4_096,
};

type WebSearchConfiguration = typeof DEFAULT_CONFIGURATION;

/** Purpose-built editor for the exact native web-search adapter contract. */
export function WebSearchSettingsEditor({
  tool,
  credentials,
  onChange,
}: {
  readonly tool: BuiltInToolConfiguration;
  readonly credentials: readonly CredentialMetadataConfiguration[];
  readonly onChange: (tool: BuiltInToolConfiguration) => void;
}): React.JSX.Element {
  const configuration = readConfiguration(tool.configuration);
  const binding = tool.credentialBindings[0];
  const availableCredentials = credentials.filter(
    ({ boundProviderId }) => boundProviderId == null,
  );
  const selectedCredential = credentials.find(
    ({ credentialRef }) => credentialRef === binding?.credentialRef,
  );
  const effectiveProvider =
    configuration.backend === "automatic"
      ? configuration.credentialBackend
      : configuration.backend;
  const dualTier = DUAL_TIER_BACKENDS.has(configuration.backend);
  const credentialForbidden =
    NO_CREDENTIAL_BACKENDS.has(configuration.backend) ||
    (dualTier && configuration.providerTier === "free");
  const credentialRequired =
    effectiveProvider === "brave" ||
    effectiveProvider === "xai" ||
    effectiveProvider === "deepseek" ||
    (dualTier && configuration.providerTier === "paid");
  const showCredential = !credentialForbidden;
  const showProviderEndpoint =
    effectiveProvider !== "deepseek" &&
    !NO_CREDENTIAL_BACKENDS.has(effectiveProvider) &&
    !(
      (effectiveProvider === "exa" || effectiveProvider === "parallel") &&
      configuration.providerTier === "free"
    );
  const updateConfiguration = (patch: Partial<WebSearchConfiguration>) =>
    onChange({
      ...tool,
      configuration: { ...configuration, ...patch },
    });

  return (
    <div className="settings-section-stack web-search-settings">
      <p className="provider-detail diagnostic">
        Automatic mode prefers a bound credential, then configured SearXNG, then rotates
        through Exa, Parallel, Firecrawl, and Keenable anonymous services. DuckDuckGo is
        the final no-account fallback.
      </p>

      <div className="settings-grid two-columns">
        <label className="settings-field" htmlFor={`${tool.id}-backend`}>
          Search backend
          <select
            id={`${tool.id}-backend`}
            title="Choose automatic routing, the anonymous provider ring, DuckDuckGo, SearXNG, or a specific Hermes-compatible provider"
            value={configuration.backend}
            onChange={(event) => {
              const backend = event.target.value as SearchBackend;
              const forbidsCredential = NO_CREDENTIAL_BACKENDS.has(backend);
              onChange({
                ...tool,
                credentialBindings: forbidsCredential
                  ? []
                  : tool.credentialBindings.slice(0, 1),
                configuration: {
                  ...configuration,
                  backend,
                  providerTier: "automatic",
                  providerBaseUrl: "",
                },
              });
            }}
          >
            <option value="automatic">Automatic · credential → SearXNG → keyless ring</option>
            <option value="keyless">Keyless ring · rotating and free</option>
            <option value="duckduckgo">DuckDuckGo · keyless HTML</option>
            <option value="searxng">SearXNG · self-hosted</option>
            <option value="exa">Exa</option>
            <option value="parallel">Parallel</option>
            <option value="firecrawl">Firecrawl</option>
            <option value="tavily">Tavily</option>
            <option value="brave">Brave Search</option>
            <option value="keenable">Keenable</option>
            <option value="xai">xAI Grok · paid API</option>
            <option value="deepseek">DeepSeek · paid API</option>
          </select>
        </label>
        {configuration.backend === "automatic" && (
          <label className="settings-field" htmlFor={`${tool.id}-credential-backend`}>
            Bound credential provider
            <select
              id={`${tool.id}-credential-backend`}
              title="Identifies which provider owns the optional API key used first by automatic routing"
              value={configuration.credentialBackend}
              onChange={(event) =>
                updateConfiguration({
                  credentialBackend: event.target.value as CredentialBackend,
                  providerBaseUrl: "",
                })
              }
            >
              <option value="exa">Exa</option>
              <option value="parallel">Parallel</option>
              <option value="firecrawl">Firecrawl</option>
              <option value="tavily">Tavily</option>
              <option value="brave">Brave Search</option>
              <option value="keenable">Keenable</option>
              <option value="xai">xAI Grok</option>
              <option value="deepseek">DeepSeek</option>
            </select>
          </label>
        )}
        {dualTier && (
          <label className="settings-field" htmlFor={`${tool.id}-provider-tier`}>
            Provider tier
            <select
              id={`${tool.id}-provider-tier`}
              title="Automatic uses a bound API key when present; Free pins the anonymous route; Paid requires a credential"
              value={configuration.providerTier}
              onChange={(event) => {
                const providerTier = event.target.value as ProviderTier;
                onChange({
                  ...tool,
                  credentialBindings:
                    providerTier === "free" ? [] : tool.credentialBindings.slice(0, 1),
                  configuration: { ...configuration, providerTier },
                });
              }}
            >
              <option value="automatic">Automatic tier</option>
              <option value="free">Free · anonymous</option>
              <option value="paid">Paid · API key</option>
            </select>
          </label>
        )}
        <NumberField
          id={`${tool.id}-maximum-results`}
          label="Maximum results"
          title="Maximum ranked result tuples returned to the model, from 1 through 100; cache requests are bucketed to 10, 20, 50, or 100"
          min={1}
          max={100}
          value={configuration.maximumResults}
          onChange={(maximumResults) => updateConfiguration({ maximumResults })}
        />
        <NumberField
          id={`${tool.id}-request-timeout`}
          label="Request timeout (seconds)"
          title="Deadline for each provider HTTP request, from 5 through 120 seconds"
          min={5}
          max={120}
          value={configuration.requestTimeoutSeconds}
          onChange={(requestTimeoutSeconds) =>
            updateConfiguration({ requestTimeoutSeconds })
          }
        />
        <NumberField
          id={`${tool.id}-maximum-retries`}
          label="Retries per backend"
          title="Retry count for temporary transport, rate-limit, and upstream failures, from 0 through 3"
          min={0}
          max={3}
          value={configuration.maximumRetries}
          onChange={(maximumRetries) => updateConfiguration({ maximumRetries })}
        />
      </div>

      <div className="settings-grid two-columns">
        <BooleanField
          id={`${tool.id}-keyless-fallback`}
          label="Keyless fallback"
          title="Allow anonymous provider routing when no configured provider is available"
          checked={configuration.keylessFallback}
          onChange={(keylessFallback) =>
            updateConfiguration({
              keylessFallback,
              keylessRescue: keylessFallback ? configuration.keylessRescue : false,
            })
          }
        />
        <BooleanField
          id={`${tool.id}-keyless-rescue`}
          label="One-shot keyless rescue"
          title="Retry one failed configured-provider call through the anonymous ring without making the fallback sticky"
          checked={configuration.keylessRescue}
          disabled={!configuration.keylessFallback}
          onChange={(keylessRescue) => updateConfiguration({ keylessRescue })}
        />
        <BooleanField
          id={`${tool.id}-cache-enabled`}
          label="Memory cache"
          title="Cache successful primary responses and coalesce matching concurrent requests"
          checked={configuration.cacheEnabled}
          onChange={(cacheEnabled) => updateConfiguration({ cacheEnabled })}
        />
        <NumberField
          id={`${tool.id}-cache-ttl`}
          label="Cache lifetime (minutes)"
          title="Freshness window for successful search responses, from 1 through 1440 minutes"
          min={1}
          max={1_440}
          value={configuration.cacheTtlMinutes}
          disabled={!configuration.cacheEnabled}
          onChange={(cacheTtlMinutes) => updateConfiguration({ cacheTtlMinutes })}
        />
      </div>

      {(configuration.backend === "automatic" ||
        configuration.backend === "searxng") && (
        <section className="settings-subsection" aria-labelledby={`${tool.id}-searxng-heading`}>
          <h4 id={`${tool.id}-searxng-heading`}>SearXNG</h4>
          <label className="settings-field" htmlFor={`${tool.id}-searxng-url`}>
            SearXNG base URL
            <input
              id={`${tool.id}-searxng-url`}
              spellCheck={false}
              title="HTTPS SearXNG base URL, or an HTTP localhost URL for a self-hosted instance with JSON output enabled"
              type="url"
              placeholder="http://127.0.0.1:8888/"
              value={configuration.searxngBaseUrl}
              onChange={(event) =>
                updateConfiguration({ searxngBaseUrl: event.target.value })
              }
            />
          </label>
        </section>
      )}

      {showProviderEndpoint && (
        <section className="settings-subsection" aria-labelledby={`${tool.id}-provider-heading`}>
          <h4 id={`${tool.id}-provider-heading`}>{providerName(effectiveProvider)}</h4>
          <label className="settings-field" htmlFor={`${tool.id}-provider-url`}>
            API base URL override
            <input
              id={`${tool.id}-provider-url`}
              spellCheck={false}
              title={`Optional HTTPS or loopback HTTP API base URL override for ${providerName(effectiveProvider)}; leave blank for the official service`}
              type="url"
              placeholder={providerBaseUrlPlaceholder(effectiveProvider)}
              value={configuration.providerBaseUrl}
              onChange={(event) =>
                updateConfiguration({ providerBaseUrl: event.target.value })
              }
            />
          </label>
          {effectiveProvider === "parallel" && (
            <label className="settings-field" htmlFor={`${tool.id}-parallel-mode`}>
              Search mode
              <select
                id={`${tool.id}-parallel-mode`}
                title="Parallel search execution mode: fast, comprehensive one-shot, or concise agentic"
                value={configuration.parallelSearchMode}
                onChange={(event) =>
                  updateConfiguration({
                    parallelSearchMode: event.target.value as ParallelSearchMode,
                  })
                }
              >
                <option value="fast">Fast</option>
                <option value="one-shot">One-shot</option>
                <option value="agentic">Agentic</option>
              </select>
            </label>
          )}
          {effectiveProvider === "xai" && (
            <div className="settings-grid two-columns">
              <label className="settings-field" htmlFor={`${tool.id}-xai-model`}>
                xAI search model
                <input
                  id={`${tool.id}-xai-model`}
                  spellCheck={false}
                  title="xAI Responses API model used for server-side web search"
                  type="text"
                  value={configuration.xaiModel}
                  onChange={(event) => updateConfiguration({ xaiModel: event.target.value })}
                />
              </label>
              <DomainListField
                id={`${tool.id}-xai-allowed-domains`}
                label="Allowed domains"
                title="Optional comma-separated xAI allowlist of at most five domain names; cannot be combined with excluded domains"
                values={configuration.xaiAllowedDomains}
                onChange={(xaiAllowedDomains) => updateConfiguration({ xaiAllowedDomains })}
              />
              <DomainListField
                id={`${tool.id}-xai-excluded-domains`}
                label="Excluded domains"
                title="Optional comma-separated xAI blocklist of at most five domain names; cannot be combined with allowed domains"
                values={configuration.xaiExcludedDomains}
                onChange={(xaiExcludedDomains) => updateConfiguration({ xaiExcludedDomains })}
              />
            </div>
          )}
        </section>
      )}

      {effectiveProvider === "deepseek" && (
        <section className="settings-subsection" aria-labelledby={`${tool.id}-deepseek-heading`}>
          <h4 id={`${tool.id}-deepseek-heading`}>DeepSeek server-side search</h4>
          <p className="field-warning">
            This sends the query to DeepSeek&apos;s paid Responses API. Results are
            model-generated from its server-side web search and incur model token charges.
          </p>
          <div className="settings-grid two-columns">
            <label className="settings-field" htmlFor={`${tool.id}-deepseek-url`}>
              API base URL
              <input
                id={`${tool.id}-deepseek-url`}
                spellCheck={false}
                title="HTTPS base URL whose responses endpoint provides DeepSeek server-side web search"
                type="url"
                value={configuration.deepseekBaseUrl}
                onChange={(event) =>
                  updateConfiguration({ deepseekBaseUrl: event.target.value })
                }
              />
            </label>
            <label className="settings-field" htmlFor={`${tool.id}-deepseek-model`}>
              Search model
              <input
                id={`${tool.id}-deepseek-model`}
                spellCheck={false}
                title="DeepSeek Responses API model used to execute and structure the paid web search"
                type="text"
                value={configuration.deepseekModel}
                onChange={(event) =>
                  updateConfiguration({ deepseekModel: event.target.value })
                }
              />
            </label>
            <NumberField
              id={`${tool.id}-deepseek-output-tokens`}
              label="Maximum output tokens"
              title="Maximum DeepSeek output tokens used to structure search results, from 256 through 16384"
              min={256}
              max={16_384}
              value={configuration.deepseekMaximumOutputTokens}
              onChange={(deepseekMaximumOutputTokens) =>
                updateConfiguration({ deepseekMaximumOutputTokens })
              }
            />
          </div>
        </section>
      )}

      {showCredential && (
        <section className="settings-subsection" aria-labelledby={`${tool.id}-credential-heading`}>
          <h4 id={`${tool.id}-credential-heading`}>Provider credential</h4>
          {(credentialRequired || binding !== undefined) && (
            <p className="field-warning">
              {credentialRequired
                ? `${providerName(effectiveProvider)} requires an API key for this route.`
                : `A bound key makes automatic or automatic-tier routing use ${providerName(effectiveProvider)}'s API.`}
            </p>
          )}
          <div className="settings-grid two-columns">
            <label className="settings-field" htmlFor={`${tool.id}-provider-credential`}>
              API credential
              <select
                id={`${tool.id}-provider-credential`}
                title={`Unbound operating-system credential leased only for an admitted ${providerName(effectiveProvider)} search invocation`}
                value={binding?.credentialRef ?? ""}
                onChange={(event) => {
                  const credential = availableCredentials.find(
                    ({ credentialRef }) => credentialRef === event.target.value,
                  );
                  onChange({
                    ...tool,
                    credentialBindings:
                      credential === undefined
                        ? []
                        : [
                            {
                              name: "api_key",
                              credentialRef: credential.credentialRef,
                              field: credential.fieldNames[0] ?? "api_key",
                            },
                          ],
                    configuration,
                  });
                }}
              >
                <option value="">No credential</option>
                {availableCredentials.map((credential) => (
                  <option key={credential.credentialRef} value={credential.credentialRef}>
                    {credential.label}
                  </option>
                ))}
              </select>
              {availableCredentials.length === 0 && (
                <span className="field-warning">
                  Create an unbound API-key credential in Credentials first.
                </span>
              )}
            </label>
            {binding !== undefined && (
              <label className="settings-field" htmlFor={`${tool.id}-provider-field`}>
                Secret field
                <select
                  id={`${tool.id}-provider-field`}
                  title="Exact field from the selected credential leased as the provider API key"
                  value={binding.field}
                  onChange={(event) =>
                    onChange({
                      ...tool,
                      credentialBindings: [
                        { ...binding, name: "api_key", field: event.target.value },
                      ],
                      configuration,
                    })
                  }
                >
                  {(selectedCredential?.fieldNames ?? []).map((field) => (
                    <option key={field} value={field}>
                      {field}
                    </option>
                  ))}
                </select>
              </label>
            )}
          </div>
        </section>
      )}
    </div>
  );
}

function NumberField({
  id,
  label,
  title,
  min,
  max,
  value,
  disabled = false,
  onChange,
}: {
  readonly id: string;
  readonly label: string;
  readonly title: string;
  readonly min: number;
  readonly max: number;
  readonly value: number;
  readonly disabled?: boolean;
  readonly onChange: (value: number) => void;
}): React.JSX.Element {
  return (
    <label className="settings-field" htmlFor={id}>
      {label}
      <input
        id={id}
        title={title}
        type="number"
        min={min}
        max={max}
        disabled={disabled}
        value={value}
        onChange={(event) => {
          if (Number.isFinite(event.target.valueAsNumber)) onChange(event.target.valueAsNumber);
        }}
      />
    </label>
  );
}

function BooleanField({
  id,
  label,
  title,
  checked,
  disabled = false,
  onChange,
}: {
  readonly id: string;
  readonly label: string;
  readonly title: string;
  readonly checked: boolean;
  readonly disabled?: boolean;
  readonly onChange: (checked: boolean) => void;
}): React.JSX.Element {
  return (
    <label className="switch-label" htmlFor={id}>
      <input
        id={id}
        title={title}
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
      />
      {label}
    </label>
  );
}

function DomainListField({
  id,
  label,
  title,
  values,
  onChange,
}: {
  readonly id: string;
  readonly label: string;
  readonly title: string;
  readonly values: readonly string[];
  readonly onChange: (values: string[]) => void;
}): React.JSX.Element {
  const rows = values.length === 0 ? [""] : [...values];
  return (
    <fieldset className="settings-field web-search-domain-list">
      <legend>{label}</legend>
      {rows.map((domain, index) => (
        <div className="web-search-domain-row" key={`${id}-${index}`}>
          <input
            id={`${id}-${index}`}
            aria-label={`${label} ${index + 1}`}
            title={title}
            type="text"
            spellCheck={false}
            placeholder="example.com"
            value={domain}
            onChange={(event) => {
              const next = [...rows];
              next[index] = event.target.value.trim();
              onChange(next.length === 1 && next[0] === "" ? [] : next);
            }}
          />
          {values.length > 0 && (
            <button
              type="button"
              title={`Remove ${label.toLowerCase()} entry ${index + 1}`}
              aria-label={`Remove ${label.toLowerCase()} ${index + 1}`}
              onClick={() => onChange(values.filter((_, row) => row !== index))}
            >
              Remove
            </button>
          )}
        </div>
      ))}
      {values.length > 0 && values.length < 5 && values.every(Boolean) && (
        <button
          type="button"
          title={`Add another ${label.toLowerCase()} entry, up to five domains`}
          onClick={() => onChange([...values, ""])}
        >
          Add domain
        </button>
      )}
    </fieldset>
  );
}

function readConfiguration(
  value: Readonly<Record<string, unknown>>,
): WebSearchConfiguration {
  const merged = { ...DEFAULT_CONFIGURATION, ...value };
  return {
    backend: isBackend(merged.backend) ? merged.backend : "automatic",
    credentialBackend: isCredentialBackend(merged.credentialBackend)
      ? merged.credentialBackend
      : "deepseek",
    providerTier: isProviderTier(merged.providerTier)
      ? merged.providerTier
      : "automatic",
    maximumResults: numberOr(merged.maximumResults, 10),
    requestTimeoutSeconds: numberOr(merged.requestTimeoutSeconds, 30),
    maximumRetries: numberOr(merged.maximumRetries, 1),
    keylessFallback: booleanOr(merged.keylessFallback, true),
    keylessRescue: booleanOr(merged.keylessRescue, true),
    cacheEnabled: booleanOr(merged.cacheEnabled, true),
    cacheTtlMinutes: numberOr(merged.cacheTtlMinutes, 20),
    searxngBaseUrl: stringOr(merged.searxngBaseUrl, ""),
    providerBaseUrl: stringOr(merged.providerBaseUrl, ""),
    parallelSearchMode: isParallelMode(merged.parallelSearchMode)
      ? merged.parallelSearchMode
      : "agentic",
    xaiModel: stringOr(merged.xaiModel, "grok-build-0.1"),
    xaiAllowedDomains: stringArrayOr(merged.xaiAllowedDomains, []),
    xaiExcludedDomains: stringArrayOr(merged.xaiExcludedDomains, []),
    deepseekBaseUrl: stringOr(merged.deepseekBaseUrl, "https://api.deepseek.com"),
    deepseekModel: stringOr(merged.deepseekModel, "deepseek-v4-flash"),
    deepseekMaximumOutputTokens: numberOr(merged.deepseekMaximumOutputTokens, 4_096),
  };
}

function isBackend(value: unknown): value is SearchBackend {
  return (
    value === "automatic" ||
    value === "keyless" ||
    value === "duckduckgo" ||
    value === "searxng" ||
    value === "exa" ||
    value === "parallel" ||
    value === "firecrawl" ||
    value === "tavily" ||
    value === "brave" ||
    value === "keenable" ||
    value === "xai" ||
    value === "deepseek"
  );
}

function isCredentialBackend(value: unknown): value is CredentialBackend {
  return isBackend(value) && !["automatic", "keyless", "duckduckgo", "searxng"].includes(value);
}

function isProviderTier(value: unknown): value is ProviderTier {
  return value === "automatic" || value === "free" || value === "paid";
}

function isParallelMode(value: unknown): value is ParallelSearchMode {
  return value === "fast" || value === "one-shot" || value === "agentic";
}

function providerName(backend: SearchBackend): string {
  const names: Record<SearchBackend, string> = {
    automatic: "Automatic provider",
    keyless: "Keyless ring",
    duckduckgo: "DuckDuckGo",
    searxng: "SearXNG",
    exa: "Exa",
    parallel: "Parallel",
    firecrawl: "Firecrawl",
    tavily: "Tavily",
    brave: "Brave Search",
    keenable: "Keenable",
    xai: "xAI Grok",
    deepseek: "DeepSeek",
  };
  return names[backend];
}

function providerBaseUrlPlaceholder(backend: SearchBackend): string {
  const defaults: Partial<Record<SearchBackend, string>> = {
    exa: "https://api.exa.ai",
    parallel: "https://api.parallel.ai",
    firecrawl: "https://api.firecrawl.dev",
    tavily: "https://api.tavily.com",
    brave: "https://api.search.brave.com",
    keenable: "https://api.keenable.ai",
    xai: "https://api.x.ai/v1",
  };
  return defaults[backend] ?? "https://provider.example/api";
}

function numberOr(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function booleanOr(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function stringOr(value: unknown, fallback: string): string {
  return typeof value === "string" ? value : fallback;
}

function stringArrayOr(value: unknown, fallback: string[]): string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string")
    ? value
    : fallback;
}

import { useMemo, useState } from "react";
import type {
  CredentialMetadataConfiguration,
  ModelConfiguration,
  ModelTarget,
  ModelTierConfiguration,
  ProviderConfiguration,
  ProviderHealthSnapshotV2,
} from "../configuration";
import {
  PROVIDER_PRESETS,
  providerPreset,
} from "../providerCatalog";
import type {
  DiscoveredModel,
  ModelDiscoveryResult,
  ProviderProbeResult,
} from "../settingsV2Port";
import { JsonObjectField } from "./SettingsFields";
import { providerDraftFingerprint } from "./settingsDraft";

type ProviderOperationResult = {
  readonly providerId: string;
  readonly draftFingerprint: string;
  readonly message: string;
  readonly detail?: string;
};

export function ProvidersModelsSection({
  providers,
  credentials,
  health,
  onChange,
  onDiscover,
  onProbe,
  confirm,
}: {
  readonly providers: readonly ProviderConfiguration[];
  readonly credentials: readonly CredentialMetadataConfiguration[];
  readonly health: readonly ProviderHealthSnapshotV2[];
  readonly onChange: (
    update: (
      current: readonly ProviderConfiguration[],
    ) => readonly ProviderConfiguration[],
  ) => void;
  readonly onDiscover: (provider: ProviderConfiguration) => Promise<ModelDiscoveryResult>;
  readonly onProbe: (
    provider: ProviderConfiguration,
    modelId: string,
  ) => Promise<ProviderProbeResult>;
  readonly confirm: (title: string, body: string) => Promise<boolean>;
}): React.JSX.Element {
  const [selectedId, setSelectedId] = useState<string | null>(providers[0]?.id ?? null);
  const [presetId, setPresetId] = useState(PROVIDER_PRESETS[0]?.id ?? "custom_openai");
  const [operation, setOperation] = useState<"discover" | string | null>(null);
  const [result, setResult] = useState<ProviderOperationResult | null>(null);
  const provider =
    providers.find(({ id }) => id === selectedId) ?? providers[0] ?? null;
  const currentResult =
    provider !== null &&
    result?.providerId === provider.id &&
    result.draftFingerprint === providerDraftFingerprint(provider)
      ? result
      : null;
  const currentHealth =
    provider === null
      ? null
      : health.find(({ providerId }) => providerId === provider.id) ?? null;
  const updateProvider = (next: ProviderConfiguration) => {
    if (provider !== null) {
      setResult(null);
      onChange((current) =>
        current.map((item) => (item.id === provider.id ? next : item)),
      );
    }
  };
  const addProvider = () => {
    const preset = providerPreset(presetId) ?? PROVIDER_PRESETS.at(-1);
    if (preset === undefined) return;
    const id = localId("provider");
    onChange((current) => [
      ...current,
      {
        id,
        name: preset.name,
        kind: preset.protocol,
        baseUrl: preset.baseUrl,
        enabled: false,
        credentialRef: null,
        models: [],
        configuration: {},
      },
    ]);
    setSelectedId(id);
    setResult(null);
  };
  return (
    <div className="provider-manager">
      <aside className="provider-list" aria-label="Configured providers">
        <div className="provider-add-row">
          <label className="settings-field" htmlFor="provider-preset">
            Provider preset
            <select
              id="provider-preset"
              title="Populate a new editable provider draft; this does not enable it or store credentials"
              value={presetId}
              onChange={(event) => setPresetId(event.target.value)}
            >
              {PROVIDER_PRESETS.map((preset) => (
                <option key={preset.id} value={preset.id}>
                  {preset.name}
                </option>
              ))}
            </select>
          </label>
          <button
            title="Add the selected preset as a disabled provider draft"
            type="button"
            onClick={addProvider}
          >
            Add
          </button>
        </div>
        {providers.length === 0 ? (
          <p className="settings-empty">No providers configured.</p>
        ) : (
          providers.map((item) => {
            const currentHealth = health.find(({ providerId }) => providerId === item.id);
            return (
              <button
                aria-current={item.id === provider?.id ? "page" : undefined}
                key={item.id}
                title={`Edit ${item.name} provider and concrete models`}
                type="button"
                onClick={() => {
                  setSelectedId(item.id);
                  setResult(null);
                }}
              >
                <span>{item.name}</span>
                <small>{currentHealth?.state ?? (item.enabled ? "configured" : "disabled")}</small>
              </button>
            );
          })
        )}
      </aside>
      <div className="provider-editor">
        {provider === null ? (
          <p className="settings-empty">
            Add a provider preset to configure an endpoint and concrete model.
          </p>
        ) : (
          <>
            <div className="settings-record-heading">
              <div>
                <h3>{provider.name}</h3>
                <code>{provider.id}</code>
              </div>
              <button
                className="danger-action"
                title={`Remove ${provider.name}; model-tier references must be repaired before Save`}
                type="button"
                onClick={() => {
                  void confirm(
                    `Remove ${provider.name}?`,
                    "This removes the provider and its models from the draft. Credentials remain in the operating-system store until separately deleted.",
                  ).then((accepted) => {
                    if (!accepted) return;
                    onChange((current) =>
                      current.filter(({ id }) => id !== provider.id),
                    );
                    setSelectedId(null);
                    setResult(null);
                  }).catch((failure: unknown) =>
                    setResult({
                      providerId: provider.id,
                      draftFingerprint: providerDraftFingerprint(provider),
                      message:
                        failure instanceof Error
                          ? failure.message
                          : String(failure),
                    }),
                  );
                }}
              >
                Remove
              </button>
            </div>
            <div className="settings-grid two-columns">
              <TextField
                id={`${provider.id}-name`}
                label="Provider name"
                title="Name shown in Aworkit settings and concrete model resolution evidence"
                value={provider.name}
                onChange={(name) => updateProvider({ ...provider, name })}
              />
              <label className="settings-field" htmlFor={`${provider.id}-kind`}>
                API protocol
                <select
                  id={`${provider.id}-kind`}
                  title="Request protocol implemented by the native provider adapter"
                  value={provider.kind}
                  onChange={(event) =>
                    updateProvider({ ...provider, kind: event.target.value })
                  }
                >
                  <option value="openai_compatible">OpenAI-compatible</option>
                  <option value="anthropic">Anthropic Messages</option>
                  <option value="gemini">Google Gemini</option>
                </select>
              </label>
            </div>
            <label className="settings-field" htmlFor={`${provider.id}-base-url`}>
              Base URL
              <input
                id={`${provider.id}-base-url`}
                autoCapitalize="none"
                spellCheck={false}
                title="Absolute HTTP(S) API base URL without embedded credentials, query, or fragment; stored credentials can be endpoint-bound"
                type="url"
                value={provider.baseUrl}
                onChange={(event) =>
                  updateProvider({ ...provider, baseUrl: event.target.value })
                }
              />
            </label>
            <div className="settings-grid two-columns">
              <label className="settings-field" htmlFor={`${provider.id}-credential`}>
                Credential
                {(() => {
                  const eligibleCredentials = credentials.filter(
                    (credential) =>
                      credential.fieldNames.includes("api_key") &&
                      (credential.boundProviderId == null ||
                        (credential.boundProviderId === provider.id &&
                          credential.boundEndpoint === provider.baseUrl)),
                  );
                  const selectedIsIneligible =
                    provider.credentialRef !== null &&
                    !eligibleCredentials.some(
                      ({ credentialRef }) =>
                        credentialRef === provider.credentialRef,
                    );
                  return (
                <select
                  id={`${provider.id}-credential`}
                  title="Only credentials with an api_key field that are unbound or bound to this exact provider and endpoint are eligible"
                  value={provider.credentialRef ?? ""}
                  onChange={(event) =>
                    updateProvider({
                      ...provider,
                      credentialRef: event.target.value || null,
                    })
                  }
                >
                  <option value="">No credential</option>
                  {selectedIsIneligible && (
                    <option disabled value={provider.credentialRef ?? ""}>
                      Incompatible saved credential — choose another
                    </option>
                  )}
                  {eligibleCredentials.map((credential) => (
                    <option key={credential.credentialRef} value={credential.credentialRef}>
                      {credential.label}
                    </option>
                  ))}
                </select>
                  );
                })()}
              </label>
              <label className="switch-label provider-enabled" htmlFor={`${provider.id}-enabled`}>
                <input
                  checked={provider.enabled}
                  id={`${provider.id}-enabled`}
                  title="Make enabled concrete models eligible for tier resolution after Save"
                  type="checkbox"
                  onChange={(event) =>
                    updateProvider({ ...provider, enabled: event.target.checked })
                  }
                />
                Provider enabled
              </label>
            </div>
            <JsonObjectField
              id={`${provider.id}-configuration`}
              label="Provider configuration"
              title="Reserved non-secret provider JSON; current Test and Discover operations require this object to be empty"
              value={provider.configuration}
              onChange={(configuration) => updateProvider({ ...provider, configuration })}
            />
            <p className="field-warning">
              Provider-specific JSON is preserved for future adapters. Current
              Test and Discover operations fail explicitly unless it is empty.
            </p>
            <div className="section-heading-row model-heading">
              <div>
                <h3>Concrete models</h3>
                <p>Each model has a stable local ID and an exact remote ID.</p>
              </div>
              <div className="section-actions">
                <button
                  disabled={operation !== null}
                  title="Fetch the real model catalog from this unsaved provider draft and merge every returned model without enabling it"
                  type="button"
                  onClick={() => {
                    const requestedFingerprint =
                      providerDraftFingerprint(provider);
                    setOperation("discover");
                    setResult(null);
                    void onDiscover(provider)
                      .then((discovery) => {
                        const discoveredProvider = {
                          ...provider,
                          models: mergeDiscoveredModels(provider.models, discovery.models),
                        };
                        onChange((current) => {
                          const currentProvider = current.find(
                            ({ id }) => id === provider.id,
                          );
                          if (
                            currentProvider === undefined ||
                            providerDraftFingerprint(currentProvider) !==
                              requestedFingerprint
                          )
                            return current;
                          return current.map((item) =>
                            item.id === provider.id
                              ? {
                                  ...item,
                                  models: mergeDiscoveredModels(
                                    item.models,
                                    discovery.models,
                                  ),
                                }
                              : item,
                          );
                        });
                        setResult({
                          providerId: provider.id,
                          draftFingerprint:
                            providerDraftFingerprint(discoveredProvider),
                          message: discovery.message,
                        });
                      })
                      .catch((failure: unknown) =>
                        setResult({
                          providerId: provider.id,
                          draftFingerprint: requestedFingerprint,
                          message:
                            failure instanceof Error
                              ? failure.message
                              : String(failure),
                        }),
                      )
                      .finally(() => setOperation(null));
                  }}
                >
                  {operation === "discover" ? "Discovering…" : "Discover models"}
                </button>
                <button
                  title="Add one concrete model manually"
                  type="button"
                  onClick={() =>
                    updateProvider({
                      ...provider,
                      models: [...provider.models, newModel()],
                    })
                  }
                >
                  Add model
                </button>
              </div>
            </div>
            {currentResult !== null && (
              <p className="provider-detail" role="status">
                <span>{currentResult.message}</span>
                {currentResult.detail !== undefined && (
                  <small>{currentResult.detail}</small>
                )}
              </p>
            )}
            {provider.models.length === 0 ? (
              <p className="settings-empty">No models configured.</p>
            ) : (
              <div className="settings-record-list model-list">
                {provider.models.map((model, modelIndex) => (
                  <ModelEditor
                    key={model.id}
                    provider={provider}
                    model={model}
                    busy={operation !== null}
                    onChange={(next) =>
                      updateProvider({
                        ...provider,
                        models: replaceAt(provider.models, modelIndex, next),
                      })
                    }
                    onRemove={() =>
                      updateProvider({
                        ...provider,
                        models: removeAt(provider.models, modelIndex),
                      })
                    }
                    onProbe={() => {
                      const requestedFingerprint =
                        providerDraftFingerprint(provider);
                      setOperation(model.id);
                      setResult(null);
                      void onProbe(provider, model.id)
                        .then((probe) =>
                          setResult({
                            providerId: provider.id,
                            draftFingerprint: requestedFingerprint,
                            message: probe.message,
                            detail: `${probe.ok ? "Ready" : "Failed"} · ${probe.remoteModelId ?? model.remoteId} · ${probe.latencyMillis} ms`,
                          }),
                        )
                        .catch((failure: unknown) =>
                          setResult({
                            providerId: provider.id,
                            draftFingerprint: requestedFingerprint,
                            message:
                              failure instanceof Error
                                ? failure.message
                                : String(failure),
                          }),
                        )
                        .finally(() => setOperation(null));
                    }}
                  />
                ))}
              </div>
            )}
            {currentHealth?.detail != null && (
              <p
                className={`provider-health-detail ${currentHealth.state}`}
                role="status"
              >
                <strong>Saved profile health: {currentHealth.state}.</strong>{" "}
                {currentHealth.detail}
              </p>
            )}
          </>
        )}
      </div>
    </div>
  );
}

function ModelEditor({
  provider,
  model,
  busy,
  onChange,
  onRemove,
  onProbe,
}: {
  readonly provider: ProviderConfiguration;
  readonly model: ModelConfiguration;
  readonly busy: boolean;
  readonly onChange: (model: ModelConfiguration) => void;
  readonly onRemove: () => void;
  readonly onProbe: () => void;
}): React.JSX.Element {
  return (
    <section className="settings-record model-record">
      <div className="settings-record-heading">
        <div>
          <h4>{model.name}</h4>
          <code>{model.id}</code>
        </div>
        <div className="section-actions">
          <button
            disabled={busy}
            title={`Send a bounded test request for ${model.remoteId} through ${provider.name}`}
            type="button"
            onClick={onProbe}
          >
            Test
          </button>
          <button
            className="danger-action"
            title={`Remove ${model.name} from this provider draft`}
            type="button"
            onClick={onRemove}
          >
            Remove
          </button>
        </div>
      </div>
      <div className="settings-grid two-columns">
        <TextField
          id={`${provider.id}-${model.id}-name`}
          label="Model name"
          title="Human-readable name shown in model-tier mapping"
          value={model.name}
          onChange={(name) => onChange({ ...model, name })}
        />
        <TextField
          id={`${provider.id}-${model.id}-remote`}
          label="Remote model ID"
          title="Exact model identifier sent to the provider API"
          value={model.remoteId}
          onChange={(remoteId) => onChange({ ...model, remoteId })}
        />
        <OptionalNumber
          id={`${provider.id}-${model.id}-context`}
          label="Context window"
          title="Optional advertised context-window token limit"
          value={model.contextWindow}
          onChange={(contextWindow) => onChange({ ...model, contextWindow })}
        />
        <OptionalNumber
          id={`${provider.id}-${model.id}-output`}
          label="Maximum output"
          title="Provider-advertised metadata only; the current execution adapters do not send this value as an output-token parameter"
          value={model.maxOutputTokens}
          onChange={(maxOutputTokens) => onChange({ ...model, maxOutputTokens })}
        />
      </div>
      <label className="settings-field" htmlFor={`${provider.id}-${model.id}-capabilities`}>
        Capabilities
        <input
          id={`${provider.id}-${model.id}-capabilities`}
          title="Comma-separated eligibility facts such as text, tools, vision, audio, or reasoning"
          type="text"
          value={model.capabilities.join(", ")}
          onChange={(event) =>
            onChange({
              ...model,
              capabilities: event.target.value
                .split(",")
                .map((value) => value.trim())
                .filter(Boolean),
            })
          }
        />
      </label>
      <label className="switch-label" htmlFor={`${provider.id}-${model.id}-enabled`}>
        <input
          checked={model.enabled}
          id={`${provider.id}-${model.id}-enabled`}
          title="Make this concrete model eligible for mapped tiers while its provider is enabled"
          type="checkbox"
          onChange={(event) => onChange({ ...model, enabled: event.target.checked })}
        />
        Model enabled
      </label>
      <JsonObjectField
        id={`${provider.id}-${model.id}-parameters`}
        label="Provider parameters"
        title="Reserved non-secret model JSON; current Test operations require this object to be empty"
        value={model.parameters}
        onChange={(parameters) => onChange({ ...model, parameters })}
      />
      <p className="field-warning">
        Provider parameters are preserved for future adapters but are not
        silently ignored: current Test operations require an empty object.
        Maximum output above is discovery metadata only.
      </p>
    </section>
  );
}

export function ModelTiersSection({
  tiers,
  providers,
  onChange,
}: {
  readonly tiers: readonly ModelTierConfiguration[];
  readonly providers: readonly ProviderConfiguration[];
  readonly onChange: (tiers: readonly ModelTierConfiguration[]) => void;
}): React.JSX.Element {
  const targets = useMemo(
    () =>
      providers.flatMap((provider) =>
        provider.models.map((model) => ({
          target: { providerId: provider.id, modelId: model.id },
          label: `${provider.name} / ${model.name}`,
          eligible: provider.enabled && model.enabled,
        })),
      ),
    [providers],
  );
  return (
    <div className="settings-section-stack">
      <div className="section-heading-row">
        <p className="section-intro">
          Workflows use portable tier IDs. Each tier resolves to exact models,
          an ordered fallback list, or an explicit subordinate policy. Current
          workflow execution resolves only Exact mappings; fallback and policy
          mappings remain editable but fail with an actionable runtime error.
        </p>
        <button
          title="Create a custom portable model tier"
          type="button"
          onClick={() =>
            onChange([
              ...tiers,
              {
                id: `tier:custom-${shortId()}`,
                name: "Custom tier",
                kind: "custom",
                resolution: { strategy: "unconfigured" },
              },
            ])
          }
        >
          Add custom tier
        </button>
      </div>
      <div className="settings-record-list tier-list">
        {tiers.map((tier, index) => (
          <TierEditor
            key={tier.id}
            tier={tier}
            targets={targets}
            onChange={(next) => onChange(replaceAt(tiers, index, next))}
            onRemove={
              tier.kind === "custom"
                ? () => onChange(removeAt(tiers, index))
                : undefined
            }
          />
        ))}
      </div>
    </div>
  );
}

type TargetOption = {
  readonly target: ModelTarget;
  readonly label: string;
  readonly eligible: boolean;
};

function TierEditor({
  tier,
  targets,
  onChange,
  onRemove,
}: {
  readonly tier: ModelTierConfiguration;
  readonly targets: readonly TargetOption[];
  readonly onChange: (tier: ModelTierConfiguration) => void;
  readonly onRemove?: () => void;
}): React.JSX.Element {
  const strategy = tier.resolution.strategy;
  const selectedTargets = resolutionTargets(tier.resolution);
  const policyPreference =
    tier.resolution.strategy === "policy"
      ? tier.resolution.preference
      : "quality";
  const policyCandidates =
    tier.resolution.strategy === "policy"
      ? tier.resolution.candidates
      : selectedTargets;
  const updateStrategy = (
    next: ModelTierConfiguration["resolution"]["strategy"],
  ) => {
    const first = selectedTargets[0] ?? targets[0]?.target;
    if (next === "unconfigured" || first === undefined) {
      onChange({ ...tier, resolution: { strategy: "unconfigured" } });
    } else if (next === "exact") {
      onChange({ ...tier, resolution: { strategy: "exact", target: first } });
    } else if (next === "fallback") {
      const second = selectedTargets[1] ?? targets[1]?.target;
      onChange({
        ...tier,
        resolution: {
          strategy: "fallback",
          targets: second === undefined ? [first] : [first, second],
        },
      });
    } else {
      onChange({
        ...tier,
        resolution: {
          strategy: "policy",
          candidates:
            selectedTargets.length > 0 ? [...selectedTargets] : [first],
          preference: "quality",
        },
      });
    }
  };
  return (
    <section className="settings-record tier-record">
      <div className="settings-record-heading">
        <div>
          <h3>{tier.name}</h3>
          <code>{tier.id}</code>
        </div>
        {onRemove !== undefined && (
          <button
            className="danger-action"
            title={`Remove custom tier ${tier.name} from the settings draft`}
            type="button"
            onClick={onRemove}
          >
            Remove
          </button>
        )}
      </div>
      <div className="settings-grid two-columns">
        <label className="settings-field" htmlFor={`${tier.id}-name`}>
          Tier name
          <input
            id={`${tier.id}-name`}
            title="Human-readable portable model-tier name"
            type="text"
            value={tier.name}
            onChange={(event) => onChange({ ...tier, name: event.target.value })}
          />
        </label>
        <label className="settings-field" htmlFor={`${tier.id}-strategy`}>
          Resolution
          <select
            id={`${tier.id}-strategy`}
            title="How this tier resolves to an eligible configured concrete model"
            value={strategy}
            onChange={(event) =>
              updateStrategy(
                event.target.value as ModelTierConfiguration["resolution"]["strategy"],
              )
            }
          >
            <option value="unconfigured">Unconfigured</option>
            <option value="exact">Exact model</option>
            <option value="fallback">Ordered fallback · not executable in workflows</option>
            <option value="policy">Selection policy · not executable in workflows</option>
          </select>
        </label>
      </div>
      {strategy === "exact" && (
        <label className="settings-field" htmlFor={`${tier.id}-exact`}>
          Concrete model
          <select
            id={`${tier.id}-exact`}
            title="Exact provider/model target recorded in Chat history when this tier resolves"
            value={targetKey(tier.resolution.target)}
            onChange={(event) => {
              const target = targets.find(
                (option) => targetKey(option.target) === event.target.value,
              )?.target;
              if (target !== undefined)
                onChange({ ...tier, resolution: { strategy: "exact", target } });
            }}
          >
            {targets.map((option) => (
              <option key={targetKey(option.target)} value={targetKey(option.target)}>
                {option.label}{option.eligible ? "" : " · disabled"}
              </option>
            ))}
          </select>
        </label>
      )}
      {(strategy === "fallback" || strategy === "policy") && (
        <TargetList
          id={tier.id}
          targets={targets}
          selected={selectedTargets}
          onChange={(selected) =>
            onChange({
              ...tier,
              resolution:
                strategy === "fallback"
                  ? { strategy: "fallback", targets: [...selected] }
                  : {
                      strategy: "policy",
                      candidates: [...selected],
                      preference: policyPreference,
                    },
            })
          }
        />
      )}
      {strategy === "policy" && (
        <label className="settings-field" htmlFor={`${tier.id}-preference`}>
          Policy preference
          <select
            id={`${tier.id}-preference`}
            title="Primary preference applied among eligible policy candidates"
            value={tier.resolution.preference}
            onChange={(event) =>
              onChange({
                ...tier,
                resolution: {
                  strategy: "policy",
                  candidates: [...policyCandidates],
                  preference: event.target.value as "quality" | "latency" | "cost",
                },
              })
            }
          >
            <option value="quality">Quality</option>
            <option value="latency">Latency</option>
            <option value="cost">Cost</option>
          </select>
        </label>
      )}
      {strategy === "unconfigured" && (
        <p className="field-warning">
          Unconfigured tiers remain visible and block workflows that require them.
        </p>
      )}
    </section>
  );
}

function TargetList({
  id,
  targets,
  selected,
  onChange,
}: {
  readonly id: string;
  readonly targets: readonly TargetOption[];
  readonly selected: readonly ModelTarget[];
  readonly onChange: (targets: readonly ModelTarget[]) => void;
}): React.JSX.Element {
  return (
    <fieldset className="settings-bindings tier-targets">
      <legend>Candidate order</legend>
      {targets.map((option) => {
        const key = targetKey(option.target);
        const index = selected.findIndex((target) => targetKey(target) === key);
        return (
          <div className="tier-target-row" key={key}>
            <label className="switch-label" htmlFor={`${id}-${key}`}>
              <input
                checked={index >= 0}
                id={`${id}-${key}`}
                title={`Include ${option.label} as a tier candidate`}
                type="checkbox"
                onChange={(event) =>
                  onChange(
                    event.target.checked
                      ? [...selected, option.target]
                      : selected.filter((target) => targetKey(target) !== key),
                  )
                }
              />
              {option.label}{option.eligible ? "" : " · disabled"}
            </label>
            {index >= 0 && (
              <div className="section-actions">
                <button
                  disabled={index === 0}
                  aria-label={`Move ${option.label} earlier`}
                  title="Move this target earlier in fallback or candidate order"
                  type="button"
                  onClick={() => onChange(move(selected, index, index - 1))}
                >
                  ↑
                </button>
                <button
                  disabled={index === selected.length - 1}
                  aria-label={`Move ${option.label} later`}
                  title="Move this target later in fallback or candidate order"
                  type="button"
                  onClick={() => onChange(move(selected, index, index + 1))}
                >
                  ↓
                </button>
              </div>
            )}
          </div>
        );
      })}
      {targets.length === 0 && (
        <p className="settings-empty">Configure at least one provider model first.</p>
      )}
    </fieldset>
  );
}

function mergeDiscoveredModels(
  current: readonly ModelConfiguration[],
  discovered: readonly DiscoveredModel[],
): ModelConfiguration[] {
  const result = [...current];
  for (const remote of discovered) {
    const existing = result.findIndex(({ remoteId }) => remoteId === remote.remoteId);
    const next: ModelConfiguration = {
      id: existing >= 0 ? result[existing]!.id : localId("model"),
      name: remote.name,
      remoteId: remote.remoteId,
      enabled: existing >= 0 ? result[existing]!.enabled : false,
      contextWindow: remote.contextWindow,
      maxOutputTokens: remote.maxOutputTokens,
      capabilities: [...remote.capabilities],
      parameters: existing >= 0 ? result[existing]!.parameters : {},
    };
    if (existing >= 0) result[existing] = next;
    else result.push(next);
  }
  return result;
}

function newModel(): ModelConfiguration {
  return {
    id: localId("model"),
    name: "Model",
    remoteId: "",
    enabled: false,
    contextWindow: null,
    maxOutputTokens: null,
    capabilities: ["text"],
    parameters: {},
  };
}

function resolutionTargets(
  resolution: ModelTierConfiguration["resolution"],
): readonly ModelTarget[] {
  switch (resolution.strategy) {
    case "unconfigured":
      return [];
    case "exact":
      return [resolution.target];
    case "fallback":
      return resolution.targets;
    case "policy":
      return resolution.candidates;
  }
}

function targetKey(target: ModelTarget): string {
  return `${target.providerId}/${target.modelId}`;
}

function move<T>(values: readonly T[], from: number, to: number): T[] {
  const next = [...values];
  const [value] = next.splice(from, 1);
  if (value !== undefined) next.splice(to, 0, value);
  return next;
}

function TextField({
  id,
  label,
  title,
  value,
  onChange,
}: {
  readonly id: string;
  readonly label: string;
  readonly title: string;
  readonly value: string;
  readonly onChange: (value: string) => void;
}): React.JSX.Element {
  return (
    <label className="settings-field" htmlFor={id}>
      {label}
      <input
        id={id}
        title={title}
        type="text"
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
    </label>
  );
}

function OptionalNumber({
  id,
  label,
  title,
  value,
  onChange,
}: {
  readonly id: string;
  readonly label: string;
  readonly title: string;
  readonly value: number | null | undefined;
  readonly onChange: (value: number | null) => void;
}): React.JSX.Element {
  return (
    <label className="settings-field" htmlFor={id}>
      {label}
      <input
        id={id}
        min={1}
        placeholder="Provider default"
        title={title}
        type="number"
        value={value ?? ""}
        onChange={(event) =>
          onChange(event.target.value === "" ? null : Number(event.target.value))
        }
      />
    </label>
  );
}

function replaceAt<T>(values: readonly T[], index: number, value: T): T[] {
  return values.map((item, itemIndex) => (itemIndex === index ? value : item));
}

function removeAt<T>(values: readonly T[], index: number): T[] {
  return values.filter((_, itemIndex) => itemIndex !== index);
}

function shortId(): string {
  return localId("value").replace(/^value\./u, "").slice(0, 12);
}

function localId(scope: string): string {
  const random = globalThis.crypto?.randomUUID?.().replaceAll("-", "");
  return `${scope}.${random ?? `${Date.now()}${Math.random().toString(16).slice(2)}`}`;
}

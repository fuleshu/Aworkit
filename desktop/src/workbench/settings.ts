/** Aworkit-owned projected settings and local versioned draft contracts. */
export type SettingsSection =
  | "providers"
  | "model_tiers"
  | "credentials"
  | "tools"
  | "extensions"
  | "mcp"
  | "external_agents"
  | "data"
  | "projects"
  | "appearance";
export interface CapabilityRequirement {
  readonly id: string;
  readonly label: string;
  readonly requiredVersion?: string;
}
export interface CapabilityRecord {
  readonly id: string;
  readonly label: string;
  readonly kind: string;
  readonly state:
    | "ready"
    | "missing"
    | "disabled"
    | "incompatible"
    | "drifted";
  readonly version?: string;
  readonly detail?: string;
}
export interface SettingsProjection {
  readonly version: number;
  readonly appearance: "system" | "light" | "dark";
  readonly portableHistoryEnabled: boolean;
  readonly provider: ProviderProjection;
}
export interface ProviderProjection {
  readonly baseUrl: string;
  readonly model: string;
  readonly credentialConfigured: boolean;
  readonly state: "unconfigured" | "configured" | "ready" | "error";
  readonly detail: string | null;
}
export interface ProviderDraft extends ProviderProjection {
  readonly credentialAction: "keep" | "replace" | "clear";
  readonly apiKey: string;
}
export interface SettingsDraft {
  readonly version: number;
  readonly appearance: "system" | "light" | "dark";
  readonly provider: ProviderDraft;
  readonly portableHistoryEnabled?: boolean;
  readonly dirtySections?: ReadonlySet<SettingsSection>;
}
export interface CapabilityResolution {
  readonly available: readonly CapabilityRequirement[];
  readonly missing: readonly CapabilityRequirement[];
  readonly disabled: readonly CapabilityRequirement[];
  readonly incompatible: readonly CapabilityRequirement[];
  readonly drifted: readonly CapabilityRequirement[];
}

export function resolveCapabilities(
  requirements: readonly CapabilityRequirement[],
  configured: ReadonlySet<string>,
  records: readonly CapabilityRecord[] = [],
): CapabilityResolution {
  const recordById = new Map(records.map((record) => [record.id, record]));
  const buckets: Record<
    "available" | "missing" | "disabled" | "incompatible" | "drifted",
    CapabilityRequirement[]
  > = {
    available: [],
    missing: [],
    disabled: [],
    incompatible: [],
    drifted: [],
  };
  for (const requirement of requirements) {
    const record = recordById.get(requirement.id);
    if (record?.state === "disabled") {
      buckets.disabled.push(requirement);
    } else if (record?.state === "incompatible") {
      buckets.incompatible.push(requirement);
    } else if (
      record?.state === "drifted" ||
      (record?.state === "ready" &&
        requirement.requiredVersion !== undefined &&
        record.version !== requirement.requiredVersion)
    ) {
      buckets.drifted.push(requirement);
    } else if (
      configured.has(requirement.id) &&
      (record?.state ?? "ready") === "ready"
    ) {
      buckets.available.push(requirement);
    } else {
      buckets.missing.push(requirement);
    }
  }
  return {
    ...buckets,
  };
}
export function updateDraft(
  draft: SettingsDraft,
  patch: Partial<
    Pick<
      SettingsDraft,
      "appearance" | "provider" | "portableHistoryEnabled"
    >
  >,
  dirtySection?: SettingsSection,
): SettingsDraft {
  return {
    ...draft,
    ...patch,
    dirtySections:
      dirtySection === undefined
        ? draft.dirtySections
        : new Set([...(draft.dirtySections ?? []), dirtySection]),
  };
}
export function canCommitDraft(
  draft: SettingsDraft,
  expectedVersion: number,
): boolean {
  return (
    draft.version === expectedVersion && (draft.dirtySections?.size ?? 0) > 0
  );
}
export function authorityPreview(
  capabilities: ReadonlySet<string>,
): readonly string[] {
  return [...capabilities]
    .sort()
    .map((id) =>
      id.startsWith("tool.")
        ? `${id}: workspace-scoped operation`
        : `${id}: provider or agent invocation`,
    );
}

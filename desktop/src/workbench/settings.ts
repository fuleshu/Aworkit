/** Settings drafts are local and versioned; no partial raw configuration is committed. */
export interface CapabilityRequirement { readonly id: string; readonly label: string; }
export interface SettingsDraft { readonly version: number; readonly appearance: "system" | "light" | "dark"; readonly configuredCapabilities: ReadonlySet<string>; }
export interface CapabilityResolution { readonly available: readonly CapabilityRequirement[]; readonly missing: readonly CapabilityRequirement[]; }
export function resolveCapabilities(requirements: readonly CapabilityRequirement[], configured: ReadonlySet<string>): CapabilityResolution { const available = requirements.filter((item) => configured.has(item.id)); return { available, missing: requirements.filter((item) => !configured.has(item.id)) }; }
export function updateDraft(draft: SettingsDraft, patch: Partial<Pick<SettingsDraft, "appearance" | "configuredCapabilities">>): SettingsDraft { return { ...draft, ...patch }; }
export function canCommitDraft(draft: SettingsDraft, expectedVersion: number): boolean { return draft.version === expectedVersion; }

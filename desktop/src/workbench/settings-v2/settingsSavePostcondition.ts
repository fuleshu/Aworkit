import type { SettingsConfigurationV2 } from "../configuration";

/** A matching receipt version alone does not prove the intended draft was committed. */
export function settingsSaveContentIssue(actual: SettingsConfigurationV2, attempted: SettingsConfigurationV2): string | null {
  const canonical = (value: unknown) => JSON.stringify(value, (_key, item: unknown) => {
    if (typeof item !== "object" || item === null || Array.isArray(item)) return item;
    return Object.fromEntries(Object.entries(item).sort(([left], [right]) => left.localeCompare(right)));
  });
  return canonical(actual) === canonical(attempted) ? null
    : "The canonical Settings snapshot does not match the submitted draft. The save outcome could not be verified.";
}

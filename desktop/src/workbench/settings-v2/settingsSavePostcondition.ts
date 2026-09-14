import type { SettingsConfigurationV2 } from "../configuration";

/**
 * Canonical content projection used to verify a committed Settings document.
 *
 * The editor draft and the trusted core encode the same absence differently: the
 * draft spells an unset optional as an explicit `null` or an empty collection
 * (a project added by the editor has `defaultWorkflowId: null`, an MCP server may
 * carry `tools: []`), while the core omits the key entirely for `Option::None`,
 * `Vec::is_empty` and defaulted option objects. Both encode the same content, so
 * absence, `null` and an empty collection compare equal here. Every other
 * difference — a changed scalar, a missing value the draft set, a different array
 * order or length, a non-empty object change — still fails the postcondition.
 */
function canonicalSettingsJson(value: unknown): string {
  return JSON.stringify(canonicalSettingsValue(value));
}

function canonicalSettingsValue(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonicalSettingsValue);
  if (value === null || typeof value !== "object") return value;
  return Object.fromEntries(
    Object.entries(value as Record<string, unknown>)
      .map(
        ([key, item]) =>
          [key, canonicalSettingsValue(item)] as const,
      )
      .filter(([, item]) => isPresentContent(item))
      .sort(([left], [right]) => left.localeCompare(right)),
  );
}

/** Reports whether a projected value carries content rather than an encoding of absence. */
function isPresentContent(value: unknown): boolean {
  if (value === null || value === undefined) return false;
  if (Array.isArray(value)) return value.length > 0;
  if (typeof value === "object") return Object.keys(value).length > 0;
  return true;
}

/**
 * Reports whether two projected settings documents describe the same canonical
 * configuration, independent of absent-versus-null-and-empty encoding.
 */
export function settingsDocumentsMatch(
  actual: unknown,
  attempted: unknown,
): boolean {
  return canonicalSettingsJson(actual) === canonicalSettingsJson(attempted);
}

/** A matching receipt version alone does not prove the intended draft was committed. */
export function settingsSaveContentIssue(actual: SettingsConfigurationV2, attempted: SettingsConfigurationV2): string | null {
  return settingsDocumentsMatch(actual, attempted) ? null
    : "The canonical Settings snapshot does not match the submitted draft. The save outcome could not be verified.";
}

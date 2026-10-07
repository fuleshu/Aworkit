/**
 * Canonical history retention bounds a Chat's store without deleting anything.
 * It removes one payload's heavy field and leaves this marker in the same
 * committed event, so the event, its sequence, its timing and its usage stay
 * exactly where they were. A payload that carries the marker is *released*.
 *
 * This module is the one reader of that marker: callers use it only to detect a
 * released payload, so no surface renders a null or an empty document in its
 * place. Retention is not narrated in the interface.
 */

/** The heavy field a release removed, in the runtime's own words. */
export type ReleasedPayloadKind = "context_checkpoint" | "model_call_input";

/** The retained evidence of a released payload; every optional field may be absent. */
export interface ReleasedPayload {
  readonly kind: ReleasedPayloadKind;
  /** Byte length of the released value, when the runtime reported one. */
  readonly bytes: number | null;
  /** Canonical digest taken before the release, when the runtime reported one. */
  readonly digestBefore: string | null;
  readonly prunedAt: string | null;
  readonly retainedTurns: number | null;
  readonly reason: string | null;
}

/**
 * The retention marker on one payload, or undefined for an intact payload.
 *
 * Only the two documented kinds are recognized, so a payload whose heavy field
 * is still present keeps rendering exactly as it did before.
 */
export function releasedPayloadOf(value: unknown): ReleasedPayload | undefined {
  const marker = asRecord(value).prunedPayload;
  if (!isRecord(marker)) return undefined;
  const kind = releasedKind(marker.kind);
  if (kind === undefined) return undefined;
  return {
    kind,
    bytes: finiteNumber(marker.bytes),
    digestBefore: nonEmptyString(marker.digestBefore),
    prunedAt: nonEmptyString(marker.prunedAt),
    retainedTurns: finiteNumber(marker.retainedTurns),
    reason: nonEmptyString(marker.reason),
  };
}

/** The release of a model call's compiled request body, or undefined. */
export function releasedModelCallInput(value: unknown): ReleasedPayload | undefined {
  const payload = releasedPayloadOf(value);
  return payload?.kind === "model_call_input" ? payload : undefined;
}

/** The release of a context checkpoint's snapshot, or undefined. */
export function releasedContextCheckpoint(value: unknown): ReleasedPayload | undefined {
  const payload = releasedPayloadOf(value);
  return payload?.kind === "context_checkpoint" ? payload : undefined;
}

function releasedKind(value: unknown): ReleasedPayloadKind | undefined {
  return value === "context_checkpoint" || value === "model_call_input"
    ? value
    : undefined;
}

function finiteNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value >= 0
    ? value
    : null;
}

function nonEmptyString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function asRecord(value: unknown): Record<string, unknown> {
  return isRecord(value) ? value : {};
}

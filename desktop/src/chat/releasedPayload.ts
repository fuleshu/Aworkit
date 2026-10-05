/**
 * Canonical history retention bounds a Chat's store without deleting anything.
 * It removes one payload's heavy field and leaves this marker in the same
 * committed event, so the event, its sequence, its timing and its usage stay
 * exactly where they were. A payload that carries the marker is *released*.
 *
 * This module is the one reader of that marker. Every surface states the same
 * omission from the same words, and a payload without the marker is untouched.
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

/** The released size in human units, or null when the runtime reported none. */
export function releasedBytesLabel(bytes: number | null): string | null {
  if (bytes === null || bytes < 0) return null;
  if (bytes >= 1_000_000) return `${compact(bytes / 1_000_000)} MB`;
  if (bytes >= 1_000) return `${compact(bytes / 1_000)} KB`;
  return bytes === 1 ? "1 byte" : `${bytes.toLocaleString()} bytes`;
}

/** The omission a released request body leaves in the product's words. */
export function releasedRequestStatement(payload: ReleasedPayload): string {
  return `Request body released — this turn's snapshot was superseded by newer turns, so its ${sized(payload, "request body")} was released to bound this Chat's store size. Its usage, timing and result are unaffected.`;
}

/** The omission a released context checkpoint leaves in the product's words. */
export function releasedSnapshotStatement(payload: ReleasedPayload): string {
  const turns =
    payload.retainedTurns === null
      ? "and the latest turns are kept"
      : `and the latest ${payload.retainedTurns.toLocaleString()} turns are kept`;
  return `Context snapshot released — this turn's snapshot was superseded by newer turns, so its ${sized(payload, "context snapshot")} was released to bound this Chat's store size. The newest snapshot of every context scope ${turns}.`;
}

/** Secondary evidence: when it was released and what the policy still keeps. */
export function releasedPayloadEvidence(payload: ReleasedPayload): string | null {
  const parts: string[] = [];
  if (payload.prunedAt !== null) parts.push(`Released ${payload.prunedAt}`);
  if (payload.retainedTurns !== null)
    parts.push(`${payload.retainedTurns.toLocaleString()} newest turns kept`);
  return parts.length === 0 ? null : parts.join(" · ");
}

/** The pre-release canonical digest, kept as evidence rather than as a headline. */
export function releasedPayloadDigest(payload: ReleasedPayload): string | null {
  return payload.digestBefore;
}

function sized(payload: ReleasedPayload, noun: string): string {
  const size = releasedBytesLabel(payload.bytes);
  return size === null ? noun : `${size} ${noun}`;
}

function compact(value: number): string {
  return value >= 10 ? value.toFixed(0) : value.toFixed(1).replace(/\.0$/u, "");
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

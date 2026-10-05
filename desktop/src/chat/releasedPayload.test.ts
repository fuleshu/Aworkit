import { describe, expect, it } from "vitest";
import {
  releasedBytesLabel,
  releasedContextCheckpoint,
  releasedModelCallInput,
  releasedPayloadDigest,
  releasedPayloadEvidence,
  releasedPayloadOf,
  releasedRequestStatement,
  releasedSnapshotStatement,
  type ReleasedPayload,
} from "./releasedPayload";

const digest = `sha256:${"a".repeat(64)}`;

/** The marker the runtime commits in place of a released heavy field. */
function marker(kind: string, bytes: unknown = 1_200_000): Record<string, unknown> {
  return {
    schemaVersion: 1,
    kind,
    bytes,
    digestBefore: digest,
    prunedAt: "2026-08-03 14:02:11",
    retainedTurns: 20,
    reason: "Superseded by newer turns.",
  };
}

describe("released payload marker", () => {
  it("reads the marker and leaves an intact payload alone", () => {
    expect(releasedPayloadOf({ prunedPayload: marker("model_call_input") })).toEqual({
      kind: "model_call_input",
      bytes: 1_200_000,
      digestBefore: digest,
      prunedAt: "2026-08-03 14:02:11",
      retainedTurns: 20,
      reason: "Superseded by newer turns.",
    });
    // Every intact shape stays untouched: the marker, not the missing field, is
    // what makes a payload released.
    expect(releasedPayloadOf({ input: { messages: [] }, hasInput: true })).toBeUndefined();
    expect(releasedPayloadOf(undefined)).toBeUndefined();
    expect(releasedPayloadOf("input")).toBeUndefined();
    expect(releasedPayloadOf({ prunedPayload: "released" })).toBeUndefined();
    expect(releasedPayloadOf({ prunedPayload: marker("something_else") })).toBeUndefined();
  });

  it("matches one heavy field at a time", () => {
    expect(releasedModelCallInput({ prunedPayload: marker("model_call_input") })?.kind).toBe(
      "model_call_input",
    );
    expect(releasedModelCallInput({ prunedPayload: marker("context_checkpoint") })).toBeUndefined();
    expect(
      releasedContextCheckpoint({ prunedPayload: marker("context_checkpoint") })?.kind,
    ).toBe("context_checkpoint");
    expect(releasedContextCheckpoint({ prunedPayload: marker("model_call_input") })).toBeUndefined();
  });

  it("states the released size in human units", () => {
    expect(releasedBytesLabel(1_200_000)).toBe("1.2 MB");
    expect(releasedBytesLabel(2_900_000)).toBe("2.9 MB");
    expect(releasedBytesLabel(15_000_000)).toBe("15 MB");
    expect(releasedBytesLabel(4_500)).toBe("4.5 KB");
    expect(releasedBytesLabel(999)).toBe("999 bytes");
    expect(releasedBytesLabel(1)).toBe("1 byte");
    expect(releasedBytesLabel(null)).toBeNull();
    expect(releasedBytesLabel(-1)).toBeNull();
  });

  it("keeps the digest as evidence and the size in the statement", () => {
    const request = releasedPayloadOf({ prunedPayload: marker("model_call_input") }) as ReleasedPayload;
    expect(releasedRequestStatement(request)).toBe(
      "Request body released — this turn's snapshot was superseded by newer turns, so its 1.2 MB request body was released to bound this Chat's store size. Its usage, timing and result are unaffected.",
    );
    expect(releasedRequestStatement(request)).not.toContain("sha256:");
    expect(releasedPayloadEvidence(request)).toBe(
      "Released 2026-08-03 14:02:11 · 20 newest turns kept",
    );
    expect(releasedPayloadDigest(request)).toBe(digest);
    expect(
      releasedSnapshotStatement({ ...request, kind: "context_checkpoint", bytes: 2_900_000 }),
    ).toBe(
      "Context snapshot released — this turn's snapshot was superseded by newer turns, so its 2.9 MB context snapshot was released to bound this Chat's store size. The newest snapshot of every context scope and the latest 20 turns are kept.",
    );
  });

  it("still states an omission when the optional evidence is missing", () => {
    const payload = releasedPayloadOf({ prunedPayload: { kind: "context_checkpoint" } });
    expect(payload).toEqual({
      kind: "context_checkpoint",
      bytes: null,
      digestBefore: null,
      prunedAt: null,
      retainedTurns: null,
      reason: null,
    });
    expect(releasedPayloadEvidence(payload as ReleasedPayload)).toBeNull();
    expect(releasedPayloadDigest(payload as ReleasedPayload)).toBeNull();
    expect(releasedSnapshotStatement(payload as ReleasedPayload)).toContain(
      "its context snapshot was released",
    );
    expect(releasedRequestStatement(payload as ReleasedPayload)).toContain(
      "its request body was released",
    );
  });

  it("ignores a marker whose evidence is the wrong shape", () => {
    const payload = releasedPayloadOf({
      prunedPayload: { kind: "model_call_input", bytes: "1.2 MB", prunedAt: "", retainedTurns: -3 },
    });
    expect(payload).toEqual({
      kind: "model_call_input",
      bytes: null,
      digestBefore: null,
      prunedAt: null,
      retainedTurns: null,
      reason: null,
    });
  });
});

import { describe, expect, it } from "vitest";
import {
  releasedContextCheckpoint,
  releasedModelCallInput,
  releasedPayloadOf,
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

  it("still detects a marker whose optional evidence is missing or the wrong shape", () => {
    expect(releasedPayloadOf({ prunedPayload: { kind: "context_checkpoint" } })).toEqual({
      kind: "context_checkpoint",
      bytes: null,
      digestBefore: null,
      prunedAt: null,
      retainedTurns: null,
      reason: null,
    });
    expect(
      releasedPayloadOf({
        prunedPayload: { kind: "model_call_input", bytes: "1.2 MB", prunedAt: "", retainedTurns: -3 },
      }),
    ).toEqual({
      kind: "model_call_input",
      bytes: null,
      digestBefore: null,
      prunedAt: null,
      retainedTurns: null,
      reason: null,
    });
  });
});

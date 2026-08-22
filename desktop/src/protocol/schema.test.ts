import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { describe, expect, it } from "vitest";
import {
  FrameDecoder,
  MAX_FRAME_BYTES,
  decodeFrame,
  encodeFrame,
} from "./frame";
import {
  AWORKIT_ENVELOPE_SCHEMA_SHA256,
  AWORKIT_MAX_SAFE_WIRE_INTEGER,
} from "./generated-schema-info";
import { baseEnvelopeSchema, commandEnvelopeSchema } from "./schema";

const fixtureRoot = new URL("../../../fixtures/protocol/v1/", import.meta.url);

async function fixture(name: string): Promise<unknown> {
  return JSON.parse(
    await readFile(new URL(`${name}-envelope.json`, fixtureRoot), "utf8"),
  ) as unknown;
}

describe("Aworkit V1 protocol", () => {
  it("accepts every Rust golden payload family and preserves framing", async () => {
    for (const name of ["command", "event", "request", "result", "error"]) {
      const parsed = baseEnvelopeSchema.parse(await fixture(name));
      expect(decodeFrame(encodeFrame(parsed))).toEqual(parsed);
    }
  });

  it("binds kind to payload and rejects unsupported or unknown data", () => {
    expect(() =>
      commandEnvelopeSchema.parse({
        schemaVersion: 2,
        messageId: "msg_01",
        generation: 7,
        kind: "command",
        payload: { name: "smoke.handshake", targetId: "trusted-core" },
      }),
    ).toThrow();
    expect(() =>
      commandEnvelopeSchema.parse({
        schemaVersion: 1,
        messageId: "msg_01",
        generation: 7,
        kind: "event",
        payload: { name: "smoke.handshake", targetId: "trusted-core" },
      }),
    ).toThrow();
    expect(() =>
      commandEnvelopeSchema.parse({
        schemaVersion: 1,
        messageId: "../escape",
        generation: 7,
        kind: "command",
        payload: { name: "smoke.handshake", targetId: "trusted-core" },
        injected: true,
      }),
    ).toThrow();
    expect(() =>
      commandEnvelopeSchema.parse({
        schemaVersion: 1,
        messageId: "msg_01",
        generation: AWORKIT_MAX_SAFE_WIRE_INTEGER + 1,
        kind: "command",
        payload: { name: "smoke.handshake", targetId: "trusted-core" },
      }),
    ).toThrow();
  });

  it("rejects malformed, over-limit, invalid UTF-8, and non-JSON frames", () => {
    expect(() => decodeFrame(new Uint8Array([0, 0, 0]))).toThrow("truncated");
    expect(() => decodeFrame(new Uint8Array([0, 16, 0, 1]))).toThrow(
      "exceeds limit",
    );
    expect(() =>
      decodeFrame(new Uint8Array([0, 0, 0, 3, 34, 255, 34])),
    ).toThrow();
    expect(() => decodeFrame(new Uint8Array([0, 0, 0, 1, 120]))).toThrow();
    expect(() => encodeFrame("x".repeat(MAX_FRAME_BYTES))).toThrow(
      "exceeds limit",
    );
    expect(() => encodeFrame(undefined)).toThrow("no JSON representation");
  });

  it("incrementally decodes split and coalesced frames without a total-chunk limit", async () => {
    const command = encodeFrame(
      baseEnvelopeSchema.parse(await fixture("command")),
    );
    const event = encodeFrame(baseEnvelopeSchema.parse(await fixture("event")));
    const combined = new Uint8Array(command.byteLength + event.byteLength);
    combined.set(command);
    combined.set(event, command.byteLength);
    const decoder = new FrameDecoder();
    expect(decoder.push(combined.subarray(0, 3))).toEqual([]);
    expect(decoder.push(combined.subarray(3))).toHaveLength(2);
  });

  it("keeps generated constraints tied to the canonical JSON Schema bytes", async () => {
    const schema = await readFile(
      new URL(
        "../../../protocol/schema/aworkit-envelope.v1.schema.json",
        import.meta.url,
      ),
    );
    expect(createHash("sha256").update(schema).digest("hex")).toBe(
      AWORKIT_ENVELOPE_SCHEMA_SHA256,
    );
  });

  it("counts Unicode scalar values according to JSON Schema maxLength", () => {
    const base = {
      schemaVersion: 1,
      messageId: "unicode_event",
      generation: 1,
      kind: "event",
      payload: { sequence: 1 },
    };
    expect(() =>
      baseEnvelopeSchema.parse({
        ...base,
        payload: { ...base.payload, name: "🦀".repeat(256) },
      }),
    ).not.toThrow();
    expect(() =>
      baseEnvelopeSchema.parse({
        ...base,
        payload: { ...base.payload, name: "🦀".repeat(257) },
      }),
    ).toThrow();
  });
});

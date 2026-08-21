import { readFile } from "node:fs/promises";
import { describe, expect, it } from "vitest";
import { decodeFrame, encodeFrame } from "./frame";
import { commandEnvelopeSchema } from "./schema";

const fixtureUrl = new URL("../../../fixtures/protocol/v1/command-envelope.json", import.meta.url);

describe("Aworkit V1 protocol fixture", () => {
  it("accepts the Rust golden JSON and preserves the compatible frame", async () => {
    const fixture = JSON.parse(await readFile(fixtureUrl, "utf8")) as unknown;
    const parsed = commandEnvelopeSchema.parse(fixture);
    expect(decodeFrame(encodeFrame(parsed))).toEqual(parsed);
  });

  it("rejects unknown schema versions and unknown envelope fields", () => {
    expect(() => commandEnvelopeSchema.parse({
      schemaVersion: 2,
      messageId: "msg_01",
      generation: 7,
      kind: "command",
      payload: { name: "smoke.handshake", targetId: "trusted-core" },
    })).toThrow();
    expect(() => commandEnvelopeSchema.parse({
      schemaVersion: 1,
      messageId: "msg_01",
      generation: 7,
      kind: "command",
      payload: { name: "smoke.handshake", targetId: "trusted-core" },
      injected: true,
    })).toThrow();
  });

  it("rejects malformed and over-limit frames", () => {
    expect(() => decodeFrame(new Uint8Array([0, 0, 0]))).toThrow("truncated");
    const oversized = new Uint8Array([0, 16, 0, 1]);
    expect(() => decodeFrame(oversized)).toThrow("exceeds limit");
  });
});

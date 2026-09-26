import {expect,it} from "vitest";

import { compactionReadout, describeCompaction } from "./compactionPlan";

it("derives the target and the split from a declared window", () => {
  const readout = compactionReadout(262_144, null, 0.382);
  expect(readout).not.toBeNull();
  expect(readout!.window).toBe(262_144);
  expect(readout!.target).toBe(65_536);
  expect(readout!.summaryPercent).toBe(38);
  expect(readout!.tailPercent).toBe(62);
  const description = describeCompaction(readout)!;
  expect(description).toContain("65,536");
  expect(description).toContain("38%");
  expect(description).toContain("62%");
  expect(description).toContain("Instructions and tool schemas");
});

it("takes the model's own output reservation out of the window first", () => {
  const readout = compactionReadout(262_144, 32_768, 0.382)!;
  expect(readout.reservation).toBe(32_768);
  expect(readout.window).toBe(229_376);
  expect(readout.target).toBe(57_344);
  expect(describeCompaction(readout)).toContain("reserves 32,768 tokens");
});

it("clamps a declared output to the target share", () => {
  expect(compactionReadout(32_768, 393_216, 0.382)!.reservation).toBe(8_192);
});

it("reports nothing for a model that declares no window", () => {
  expect(compactionReadout(null, 4_096, 0.382)).toBeNull();
  expect(compactionReadout(undefined, undefined, 0.382)).toBeNull();
  expect(describeCompaction(null)).toBeNull();
});

it("follows a configured share and falls back on the default", () => {
  expect(compactionReadout(100_000, null, 0.2)!.summaryPercent).toBe(20);
  expect(compactionReadout(100_000, null, 0.2)!.tailPercent).toBe(80);
  expect(compactionReadout(100_000, null, Number.NaN)!.summaryPercent).toBe(38);
});

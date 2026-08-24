import { describe, expect, it } from "vitest";
import { nextManagementCommandId } from "./management/corePort";
import { nextWorkbenchCommandId } from "./workbench/corePort";
import {
  ProjectionGateway,
  type ProjectionReducer,
} from "./workbench/projection";

const reducer: ProjectionReducer<null> = {
  initial: null,
  reduce: () => null,
};
const secureId =
  /^desktop\.(chat|settings|workflow|management)\.(?:[0-9a-f]{32}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/;

describe("durable frontend command IDs", () => {
  it("stays disjoint across independent ProjectionGateway app sessions", () => {
    const firstSession = new ProjectionGateway(reducer);
    const secondSession = new ProjectionGateway(reducer);
    const firstIds = Array.from({ length: 64 }, () =>
      firstSession.createCommandId("chat"),
    );
    const secondIds = Array.from({ length: 64 }, () =>
      secondSession.createCommandId("chat"),
    );

    const allIds = [...firstIds, ...secondIds];
    expect(new Set(allIds).size).toBe(allIds.length);
    expect(allIds.every((id) => secureId.test(id))).toBe(true);
  });

  it("uses secure scoped IDs for settings, workflow, and management", () => {
    const ids = [
      nextWorkbenchCommandId("settings"),
      nextWorkbenchCommandId("settings"),
      nextWorkbenchCommandId("workflow"),
      nextWorkbenchCommandId("workflow"),
      nextManagementCommandId(),
      nextManagementCommandId(),
    ];

    expect(new Set(ids).size).toBe(ids.length);
    expect(ids.every((id) => secureId.test(id))).toBe(true);
    expect(ids[0]).toMatch(/^desktop\.settings\./);
    expect(ids[2]).toMatch(/^desktop\.workflow\./);
    expect(ids[4]).toMatch(/^desktop\.management\./);
  });
});

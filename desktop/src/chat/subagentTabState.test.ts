import { describe, expect, it } from "vitest";
import {
  activateSubagentTab,
  autoCloseSubagentTab,
  closeSubagentTab,
  dropUnknownSubagentTabs,
  NO_SUBAGENT_TABS,
  openSubagentTab,
  openSubagentTabInBackground,
  SUBAGENT_TAB_LIMIT,
  SubagentTabMemory,
} from "./subagentTabState";

describe("per-Chat subagent tab state", () => {
  it("opens one tab per child and only activates an already open child", () => {
    const first = openSubagentTab(NO_SUBAGENT_TABS, "child.a");
    expect(first).toEqual({ open: ["child.a"], active: "child.a" });
    const second = openSubagentTab(first, "child.b");
    expect(second).toEqual({ open: ["child.a", "child.b"], active: "child.b" });
    expect(openSubagentTab(second, "child.a")).toEqual({
      open: ["child.a", "child.b"],
      active: "child.a",
    });
    expect(openSubagentTab(second, "child.a").open).toHaveLength(2);
  });

  it("opens a new child in the background without stealing focus", () => {
    const active = openSubagentTab(NO_SUBAGENT_TABS, "child.a");
    const background = openSubagentTabInBackground(active, "child.b");
    expect(background.open).toEqual(["child.a", "child.b"]);
    expect(background.active).toBe("child.a");
    // An already open child is never duplicated by the background path.
    expect(openSubagentTabInBackground(background, "child.a").open).toEqual([
      "child.a",
      "child.b",
    ]);
  });

  it("closing a tab hides it, hands focus to the parent, and never touches others", () => {
    const state = { open: ["child.a", "child.b"], active: "child.a" };
    expect(closeSubagentTab(state, "child.a")).toEqual({
      open: ["child.b"],
      active: null,
    });
    expect(closeSubagentTab(state, "child.b")).toEqual({
      open: ["child.a"],
      active: "child.a",
    });
    expect(closeSubagentTab(state, "child.zzz")).toBe(state);
  });

  it("auto-close skips the active tab and any closed tab", () => {
    const state = { open: ["child.a", "child.b"], active: "child.a" };
    expect(autoCloseSubagentTab(state, "child.a")).toBe(state);
    expect(autoCloseSubagentTab(state, "child.b")).toEqual({
      open: ["child.a"],
      active: "child.a",
    });
    expect(autoCloseSubagentTab(state, "child.zzz")).toBe(state);
  });

  it("bounds the strip by evicting the oldest non-active tab", () => {
    let state = NO_SUBAGENT_TABS;
    for (let index = 0; index < SUBAGENT_TAB_LIMIT + 2; index += 1) {
      state = openSubagentTabInBackground(state, `child.${index}`);
    }
    expect(state.open).toHaveLength(SUBAGENT_TAB_LIMIT);
    expect(state.open).not.toContain("child.0");
    expect(state.open).not.toContain("child.1");
    expect(state.open.at(-1)).toBe(`child.${SUBAGENT_TAB_LIMIT + 1}`);
  });

  it("never evicts the active tab while a background tab is added", () => {
    let state = openSubagentTab(NO_SUBAGENT_TABS, "child.active");
    for (let index = 0; index < SUBAGENT_TAB_LIMIT + 2; index += 1) {
      state = openSubagentTabInBackground(state, `child.bg.${index}`);
    }
    expect(state.open).toContain("child.active");
    expect(state.active).toBe("child.active");
  });

  it("activates the parent by passing null and drops unknown children", () => {
    const state = { open: ["child.a", "child.b"], active: "child.b" };
    expect(activateSubagentTab(state, null)).toEqual({
      open: ["child.a", "child.b"],
      active: null,
    });
    expect(activateSubagentTab(state, "child.b")).toBe(state);
    expect(dropUnknownSubagentTabs(state, new Set(["child.a"]))).toEqual({
      open: ["child.a"],
      active: null,
    });
    expect(dropUnknownSubagentTabs(state, new Set(["child.a", "child.b"]))).toBe(
      state,
    );
  });

  it("remembers each Chat's tab set for the session", () => {
    const memory = new SubagentTabMemory();
    expect(memory.state("chat.one")).toBe(NO_SUBAGENT_TABS);
    memory.set("chat.one", { open: ["child.a"], active: "child.a" });
    memory.set("chat.two", { open: ["child.b"], active: null });
    expect(memory.state("chat.one").active).toBe("child.a");
    expect(memory.state("chat.two")).toEqual({ open: ["child.b"], active: null });
    expect(memory.state("chat.three")).toBe(NO_SUBAGENT_TABS);
    memory.clear();
    expect(memory.state("chat.one")).toBe(NO_SUBAGENT_TABS);
  });
});

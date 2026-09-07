// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { expect, it } from "vitest";
import type { Node } from "@xyflow/react";
import { useMeasuredWorkflowNodes } from "./useMeasuredWorkflowNodes";

const project = (selected = ""): Node[] => ["input", "agent", "output"].map((id, index) => ({
  id, position: { x: index * 200, y: 100 }, data: { label: id }, selected: id === selected,
}));

it("retains every node's dimensions across list selections, edits, and parent refreshes", () => {
  const source = project();
  const { result, rerender } = renderHook(({ nodes }) => useMeasuredWorkflowNodes(nodes), { initialProps: { nodes: source } });
  act(() => result.current.onNodesChange(source.map(node => ({type:"dimensions",id:node.id,dimensions:{width:154,height:58}}))));
  for (const id of ["agent", "output", "input", ""]) {
    rerender({ nodes: project(id) });
    expect(result.current.nodes.map(node => node.measured)).toEqual(Array(3).fill({width:154,height:58}));
  }
  expect(source.every(node => node.measured === undefined)).toBe(true);
  const edited = project("agent"); edited[1].data.label = "A wider Agent label";
  rerender({nodes:edited});
  act(() => result.current.onNodesChange([{type:"dimensions",id:"agent",dimensions:{width:240,height:58}}]));
  expect(result.current.nodes.map(node => node.measured?.width)).toEqual([154,240,154]);
});

it("ignores zero sizes from hidden routes, duplicate notifications, and non-dimension events", () => {
  const { result } = renderHook(() => useMeasuredWorkflowNodes(project()));
  act(() => result.current.onNodesChange([{type:"dimensions",id:"agent",dimensions:{width:154,height:58}}]));
  act(() => result.current.onNodesChange([
    {type:"dimensions",id:"agent",dimensions:{width:0,height:0}},
    {type:"select",id:"agent",selected:true},
    {type:"dimensions",id:"agent",dimensions:{width:154,height:58}},
  ]));
  expect(result.current.nodes[1].measured).toEqual({width:154,height:58});
  expect(result.current.nodes[1].selected).toBe(false);
});

it("forgets removed nodes and does not apply late measurements to absent nodes", () => {
  const { result, rerender } = renderHook(({ nodes }) => useMeasuredWorkflowNodes(nodes), {initialProps:{nodes:project()}});
  act(() => result.current.onNodesChange([{type:"dimensions",id:"agent",dimensions:{width:154,height:58}}]));
  rerender({nodes:project().filter(node => node.id !== "agent")});
  act(() => result.current.onNodesChange([{type:"dimensions",id:"agent",dimensions:{width:999,height:999}}]));
  rerender({nodes:project()});
  expect(result.current.nodes[1].measured).toBeUndefined();
});

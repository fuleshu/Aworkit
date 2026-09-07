import { useCallback, useEffect, useMemo, useState } from "react";
import type { Node, NodeChange } from "@xyflow/react";

type Dimensions = { width: number; height: number };

/**
 * React Flow's controlled nodes must carry their measured dimensions forward.
 * These are renderer state only: never write them into the workflow document or
 * undo history. Hidden routes can report zero sizes, which must not erase the
 * last usable measurement.
 */
export function useMeasuredWorkflowNodes<N extends Node>(projected: N[]): {
  nodes: N[];
  onNodesChange: (changes: NodeChange<N>[]) => void;
} {
  const [measurements, setMeasurements] = useState<ReadonlyMap<string, Dimensions>>(() => new Map());
  const ids = useMemo(() => new Set(projected.map(node => node.id)), [projected]);
  useEffect(() => {
    setMeasurements(current => {
      if ([...current.keys()].every(id => ids.has(id))) return current;
      return new Map([...current].filter(([id]) => ids.has(id)));
    });
  }, [ids]);

  const onNodesChange = useCallback((changes: NodeChange<N>[]) => {
    setMeasurements(current => {
      let next: Map<string, Dimensions> | undefined;
      for (const change of changes) {
        if (change.type !== "dimensions" || !change.dimensions || !ids.has(change.id)) continue;
        const { width, height } = change.dimensions;
        if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) continue;
        const previous = (next ?? current).get(change.id);
        if (previous?.width === width && previous.height === height) continue;
        next ??= new Map(current);
        next.set(change.id, { width, height });
      }
      return next ?? current;
    });
  }, [ids]);

  const nodes = useMemo(() => projected.map(node => {
    const measured = measurements.get(node.id);
    return measured ? { ...node, measured } : node;
  }), [projected, measurements]);
  return { nodes, onNodesChange };
}

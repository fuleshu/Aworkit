import { useEffect, useLayoutEffect, useRef, useState } from "react";

/** Fit a resizable side pane to its container while reserving usable main content. */
export function usePaneWidth(
  initial: number,
  minimum: number,
  remaining: number,
  /** Persisted separator position, applied once when it arrives. */
  stored?: number,
) {
  const [container, ref] = useState<HTMLElement | null>(null);
  const [available, setAvailable] = useState(window.innerWidth);
  const [preferred, setWidth] = useState(initial);
  const appliedStored = useRef<number | undefined>(undefined);
  useEffect(() => {
    if (stored === undefined || appliedStored.current === stored) return;
    appliedStored.current = stored;
    setWidth(stored);
  }, [stored]);
  useLayoutEffect(() => {
    if (!container) return;
    const measure = () => { if (container.clientWidth > 0) setAvailable(container.clientWidth); };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(container);
    return () => observer.disconnect();
  }, [container]);
  const max = Math.max(minimum, available - remaining - 6);
  return { ref, width: Math.min(preferred, max), max, setWidth };
}

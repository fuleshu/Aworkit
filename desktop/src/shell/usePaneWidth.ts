import { useLayoutEffect, useState } from "react";

/** Fit a resizable side pane to its container while reserving usable main content. */
export function usePaneWidth(initial: number, minimum: number, remaining: number) {
  const [container, ref] = useState<HTMLElement | null>(null);
  const [available, setAvailable] = useState(window.innerWidth);
  const [preferred, setWidth] = useState(initial);
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

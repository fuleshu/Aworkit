import { useEffect, useRef } from "react";

interface PaneSplitterProps {
  readonly value: number;
  readonly min: number;
  readonly max: number;
  readonly onChange: (value: number) => void;
  readonly onPreview?: (value: number) => void;
  readonly label?: string;
  readonly direction?: 1 | -1;
  readonly className?: string;
}

interface ActiveDrag {
  readonly pointerId: number;
  readonly startX: number;
  readonly startValue: number;
  latestValue: number;
  previewFrame: number | null;
}

/**
 * Six-pixel pointer hit zone plus keyboard-adjustable separator semantics.
 * Pointer previews are frame-coalesced; controlled React state is committed
 * only when a preview-capable drag ends.
 */
export function PaneSplitter({
  value,
  min,
  max,
  onChange,
  onPreview,
  label = "Resize navigation pane",
  direction = 1,
  className = "",
}: PaneSplitterProps): React.JSX.Element {
  const separatorRef = useRef<HTMLDivElement>(null);
  const activeDrag = useRef<ActiveDrag | null>(null);
  const clamp = (candidate: number) => Math.min(max, Math.max(min, candidate));
  const adjust = (delta: number) =>
    onChange(clamp(value + delta));
  const applyPreview = (next: number) => {
    separatorRef.current?.setAttribute("aria-valuenow", String(next));
    (onPreview ?? onChange)(next);
  };
  const schedulePreview = (next: number) => {
    const drag = activeDrag.current;
    if (drag === null) return;
    drag.latestValue = next;
    if (drag.previewFrame !== null) return;
    drag.previewFrame = window.requestAnimationFrame(() => {
      const current = activeDrag.current;
      if (current === null) return;
      current.previewFrame = null;
      applyPreview(current.latestValue);
    });
  };
  const finishDrag = (commit: boolean) => {
    const drag = activeDrag.current;
    if (drag === null) return;
    if (drag.previewFrame !== null)
      window.cancelAnimationFrame(drag.previewFrame);
    activeDrag.current = null;
    const finalValue = commit ? drag.latestValue : drag.startValue;
    separatorRef.current?.setAttribute("aria-valuenow", String(finalValue));
    if (onPreview !== undefined) onPreview(finalValue);
    if (commit) onChange(finalValue);
    document.body.classList.remove("pane-resizing");
  };
  useEffect(
    () => () => {
      const frame = activeDrag.current?.previewFrame;
      if (frame !== null && frame !== undefined)
        window.cancelAnimationFrame(frame);
      activeDrag.current = null;
      document.body.classList.remove("pane-resizing");
    },
    [],
  );
  return (
    <div
      ref={separatorRef}
      aria-label={label}
      aria-orientation="vertical"
      aria-valuemax={max}
      aria-valuemin={min}
      aria-valuenow={value}
      className={`pane-splitter ${className}`.trim()}
      role="separator"
      tabIndex={0}
      onKeyDown={(event) => {
        if (event.key === "ArrowLeft") adjust(-8 * direction);
        if (event.key === "ArrowRight") adjust(8 * direction);
        if (event.key === "Home") onChange(min);
        if (event.key === "End") onChange(max);
      }}
      onPointerDown={(event) => {
        if (event.button !== 0 || !event.isPrimary) return;
        event.preventDefault();
        event.currentTarget.setPointerCapture?.(event.pointerId);
        activeDrag.current = {
          pointerId: event.pointerId,
          startX: event.clientX,
          startValue: value,
          latestValue: value,
          previewFrame: null,
        };
        document.body.classList.add("pane-resizing");
      }}
      onPointerMove={(event) => {
        const drag = activeDrag.current;
        if (drag === null || drag.pointerId !== event.pointerId) return;
        schedulePreview(
          clamp(
            drag.startValue + direction * (event.clientX - drag.startX),
          ),
        );
      }}
      onPointerUp={(event) => {
        if (activeDrag.current?.pointerId !== event.pointerId) return;
        event.currentTarget.releasePointerCapture?.(event.pointerId);
        finishDrag(true);
      }}
      onPointerCancel={() => finishDrag(false)}
    />
  );
}
